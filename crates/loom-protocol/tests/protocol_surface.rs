//! Verification tests for the `loom-protocol` crate's surface, per
//! `specs/gate.md` § *`loom-protocol` crate*. Each test pins a
//! load-bearing structural property of the wire-format contract so a
//! regression breaks visibly here rather than silently in downstream
//! consumers.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use loom_events::identifier::SpecLabel;
use loom_protocol::gate::{
    ConcernToken, DispatchScope, Finding, FindingTarget, FindingValidator, TerminalSurface,
    WalkOutput,
};

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .expect("workspace root reachable from CARGO_MANIFEST_DIR")
}

/// The `loom-protocol` crate is a public-contract leaf: its
/// `[dependencies]` table lists only the closed set agreed in
/// `specs/gate.md` § *`loom-protocol` crate*. Adding a new dep is a
/// breaking change to the consumer dependency surface and requires a
/// spec edit; this test fails if that lands without the spec change.
#[test]
fn loom_protocol_crate_has_minimal_leaf_dependency_set() {
    let manifest = workspace_root().join("crates/loom-protocol/Cargo.toml");
    let body = std::fs::read_to_string(&manifest)
        .unwrap_or_else(|e| panic!("read {}: {e}", manifest.display()));
    let parsed: toml::Value = toml::from_str(&body).expect("parse Cargo.toml");
    let deps = parsed
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .expect("[dependencies] table present");
    let mut keys: Vec<&str> = deps.keys().map(String::as_str).collect();
    keys.sort_unstable();

    let allowed: &[&str] = &[
        "blake3",
        "displaydoc",
        "loom-events",
        "serde",
        "serde_json",
        "thiserror",
    ];
    let mut allowed_sorted: Vec<&str> = allowed.to_vec();
    allowed_sorted.sort_unstable();
    assert_eq!(
        keys, allowed_sorted,
        "loom-protocol [dependencies] must equal the closed allow-list per specs/gate.md \
         (no transitive loom-templates / loom-workflow / loom-gate)",
    );

    let forbidden = ["loom-templates", "loom-workflow", "loom-gate"];
    for dep in forbidden {
        assert!(
            !deps.contains_key(dep),
            "loom-protocol must not depend on {dep} (leaf-crate invariant)",
        );
    }
}

/// `WalkOutput`'s fields are private at the `loom-protocol` crate
/// boundary; the only `pub` construction path is
/// [`WalkOutput::from_stdout`]. Struct-literal construction (`WalkOutput
/// { terminal, findings, finding_errors }`) is rejected at compile time
/// outside this crate, so the silent-loss failure class — production
/// caller constructs `WalkOutput` with bogus fields, bypassing the
/// typed parse pipeline — is structurally unrepresentable per
/// `specs/gate.md` § *Structural enforcement*.
///
/// At runtime we exercise the constructor and pin the accessor surface
/// (`terminal()` / `findings()` / `finding_errors()`) so consumers can
/// read state without naming the private field path. The
/// function-pointer assignment is a compile-time signature pin: if
/// `from_stdout` changes shape or stops being `pub`, this fails to
/// compile.
#[test]
fn walk_output_fields_private_only_constructor_is_from_stdout() {
    struct AcceptAll;
    impl FindingValidator for AcceptAll {
        fn spec_label_is_known(&self, _: &SpecLabel) -> bool {
            true
        }
        fn criterion_anchor_resolves(&self, _: &SpecLabel, _: &str) -> bool {
            true
        }
        fn annotation_resolves(&self, _: &str) -> bool {
            true
        }
        fn file_exists(&self, _: &str) -> bool {
            true
        }
        fn invariant_resolves(&self, _: &SpecLabel, _: &str, _: &str) -> bool {
            true
        }
    }

    let _: fn(&str, DispatchScope, &AcceptAll) -> WalkOutput = WalkOutput::from_stdout;

    let walk = WalkOutput::from_stdout("LOOM_COMPLETE\n", DispatchScope::Tree, &AcceptAll);
    assert_eq!(walk.terminal(), &TerminalSurface::Complete);
    assert!(walk.findings().is_empty());
    assert!(walk.finding_errors().is_empty());

    let gate_src = workspace_root().join("crates/loom-protocol/src/gate.rs");
    let body = std::fs::read_to_string(&gate_src)
        .unwrap_or_else(|e| panic!("read {}: {e}", gate_src.display()));
    assert!(
        !body.contains("pub terminal:")
            && !body.contains("pub findings:")
            && !body.contains("pub finding_errors:"),
        "WalkOutput fields must be private — found a `pub` field declaration in {}",
        gate_src.display(),
    );
    assert!(
        body.contains("pub fn from_stdout"),
        "WalkOutput::from_stdout must be `pub` so consumers can call it",
    );
}

/// The `LOOM_FINDING:` / `LOOM_CONCERN:` wire payloads carry no
/// `"protocol": <n>` field. Wire-format SemVer rides through Cargo +
/// the typed parse errors (`FindingParseError::Json` /
/// `TokenVariantMismatch`); per-line versioning would re-introduce a
/// silent-breakage path the leaf-crate dependency shape exists to
/// eliminate. Per `specs/gate.md` § *Canonical contract location*.
#[test]
fn loom_protocol_wire_format_does_not_carry_protocol_version_field() {
    let spec: SpecLabel = "gate".parse().expect("valid spec label");
    let finding = Finding {
        token: ConcernToken::SpecCoherenceFail,
        route: loom_protocol::gate::FindingRoute::Deferred,
        bonds: vec![spec.clone()],
        target: FindingTarget::Criterion {
            spec,
            anchor: "verifier-honesty".to_owned(),
        },
        evidence: "sample evidence".to_owned(),
    };
    let json = serde_json::to_value(&finding).expect("serialize finding");
    let obj = json.as_object().expect("finding serializes as object");
    assert!(
        !obj.contains_key("protocol"),
        "Finding wire JSON must not carry a `protocol` field: {json}",
    );
    let target_json = obj.get("target").and_then(|v| v.as_object());
    assert!(target_json.is_some(), "target field is an object");
    if let Some(target) = target_json {
        assert!(
            !target.contains_key("protocol"),
            "FindingTarget wire JSON must not carry a `protocol` field",
        );
    }

    let gate_src = workspace_root().join("crates/loom-protocol/src/gate.rs");
    let body = std::fs::read_to_string(&gate_src)
        .unwrap_or_else(|e| panic!("read {}: {e}", gate_src.display()));
    assert!(
        !body.contains("\"protocol\""),
        "loom-protocol::gate must not declare a wire `\"protocol\"` field",
    );
}

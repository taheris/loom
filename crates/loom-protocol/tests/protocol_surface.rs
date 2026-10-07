//! Verification tests for the `loom-protocol` crate's surface, per
//! `specs/gate.md` § *`loom-protocol` crate*. Each test pins a
//! load-bearing structural property of the wire-format contract so a
//! regression breaks visibly here rather than silently in downstream
//! consumers.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use loom_events::identifier::SpecLabel;
use loom_protocol::gate::{
    ConcernToken, DispatchScope, FindingTarget, FindingValidator, RawFinding, TerminalSurface,
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

/// External consumers can parse stdout but cannot bypass parsing with a struct literal.
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
    assert_eq!(walk.findings(), []);
    assert_eq!(walk.finding_errors(), []);

    // trybuild writes its generated manifest before taking its own build lock.
    // Protect that entire shared-directory lifecycle across independent gate processes.
    let output = std::process::Command::new("cargo")
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--manifest-path",
        ])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
        .output()
        .expect("cargo metadata for trybuild target directory");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let _guard =
        lock_compile_fixture(Path::new(metadata["target_directory"].as_str().unwrap())).unwrap();
    trybuild::TestCases::new().compile_fail("tests/ui/walk_output_literal.rs");
}

fn lock_compile_fixture(target: &Path) -> std::io::Result<std::fs::File> {
    let directory = target.join("tests");
    std::fs::create_dir_all(&directory)?;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(".loom-protocol-trybuild.lock"))?;
    file.lock()?;
    Ok(file)
}

#[test]
fn compile_fixture_lock_preserves_every_independent_process_update() -> std::io::Result<()> {
    let root = tempfile::tempdir()?;
    if !loom_test_support::parallel_process::run(
        "compile_fixture_lock_preserves_every_independent_process_update",
        root.path(),
        8,
        |root, worker| {
            let _guard = lock_compile_fixture(root)?;
            let path = root.join("manifest.json");
            let mut values: Vec<usize> = match std::fs::read(&path) {
                Ok(bytes) => serde_json::from_slice(&bytes)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(error) => return Err(error),
            };
            values.push(worker);
            std::fs::write(path, serde_json::to_vec(&values)?)
        },
    )? {
        return Ok(());
    }
    let mut values: Vec<usize> =
        serde_json::from_slice(&std::fs::read(root.path().join("manifest.json"))?)?;
    values.sort_unstable();
    assert_eq!(values, (0..8).collect::<Vec<_>>());
    Ok(())
}

/// The `LOOM_FINDING:` / `LOOM_CONCERN:` wire payloads carry no
/// `"protocol": <n>` field. Wire-format `SemVer` rides through Cargo +
/// the typed parse errors (`FindingParseError::Json` /
/// `TokenVariantMismatch`); per-line versioning would re-introduce a
/// silent-breakage path the leaf-crate dependency shape exists to
/// eliminate. Per `specs/gate.md` § *Canonical contract location*.
#[test]
fn loom_protocol_wire_format_does_not_carry_protocol_version_field() {
    let spec: SpecLabel = "gate".parse().expect("valid spec label");
    let finding = RawFinding {
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
}

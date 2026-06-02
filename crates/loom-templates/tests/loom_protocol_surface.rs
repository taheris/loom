//! Verifies the `loom-templates::finding` /
//! `loom-templates::previous_failure` re-export shape from
//! `loom-protocol::gate`, per `specs/gate.md` § *`loom-protocol`
//! crate*.
//!
//! The typed retry-context surface
//! (`PreviousFailure::ReviewConcern { findings: Vec<Finding> }`) carries
//! [`loom_protocol::gate::Finding`] as a field; `loom-templates`
//! re-exports the contract types via `pub use` so existing callers
//! compile without changes after the crate carve-out.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use loom_events::identifier::SpecLabel;

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .expect("workspace root reachable from CARGO_MANIFEST_DIR")
}

/// `loom-templates::finding` and `loom-templates::previous_failure`
/// re-export the typed contract from `loom-protocol::gate` via
/// `pub use`. The compile-time type-equality assertions below pin
/// each re-export — the right-hand-side type lives in
/// `loom-protocol::gate`, so a future rename / move on the protocol
/// side that drops the re-export breaks this test.
///
/// The file-content check additionally pins that the `loom-templates`
/// module bodies are *re-exports*, not parallel definitions — a
/// secondary defense against the
/// `finding_no_duplicate_definitions` walker being bypassed.
#[test]
fn loom_templates_re_exports_finding_contract_from_loom_protocol() {
    // Compile-time identity pins: each `let _: A = identity_b(...);`
    // requires `A` and the result type of `identity_b` to be the same
    // nominal type — i.e. the re-export points at the canonical
    // definition in `loom-protocol::gate`.
    fn identity<T>(x: T) -> T {
        x
    }

    let spec: SpecLabel = "gate".parse().expect("valid spec label");

    let proto_finding = loom_protocol::gate::Finding {
        token: loom_protocol::gate::ConcernToken::SpecCoherenceFail,
        bonds: vec![spec.clone()],
        target: loom_protocol::gate::FindingTarget::Criterion {
            spec: spec.clone(),
            anchor: "verifier-honesty".to_owned(),
        },
        evidence: "via loom-protocol".to_owned(),
    };
    let _: loom_templates::Finding = identity(proto_finding);

    let proto_token = loom_protocol::gate::ConcernToken::OrphanIntegration;
    let _: loom_templates::ConcernToken = identity(proto_token);

    let proto_target = loom_protocol::gate::FindingTarget::Contract {
        id: "molecule-lifecycle".to_owned(),
    };
    let _: loom_templates::FindingTarget = identity(proto_target);

    let proto_kind = loom_protocol::gate::TargetKind::Annotation;
    let _: loom_templates::TargetKind = identity(proto_kind);

    let proto_err = loom_protocol::gate::FindingParseError::Json {
        line_number: 1,
        raw: "raw".into(),
        message: "msg".into(),
    };
    let _: loom_templates::FindingParseError = identity(proto_err);

    let proto_badwalk = loom_protocol::gate::BadWalk::ConcernWithoutFindings {
        summary: "round-trip".into(),
    };
    let _: loom_templates::BadWalk = identity(proto_badwalk);

    let proto_terminal = loom_protocol::gate::TerminalSurface::Complete;
    let _: loom_templates::TerminalSurface = identity(proto_terminal);

    assert_eq!(
        loom_templates::LOOM_FINDING_PREFIX,
        loom_protocol::gate::LOOM_FINDING_PREFIX,
    );

    // Module-body re-export shape pin: `crates/loom-templates/src/finding.rs`
    // and `crates/loom-templates/src/previous_failure.rs` must re-export
    // from `loom_protocol::gate`, not re-declare. The
    // `finding_no_duplicate_definitions` walker enforces this at the
    // workspace level; this test pins it locally so a future edit that
    // re-introduces the type definitions breaks visibly here too.
    let finding_path = workspace_root().join("crates/loom-templates/src/finding.rs");
    let finding_body = std::fs::read_to_string(&finding_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", finding_path.display()));
    assert!(
        finding_body.contains("pub use loom_protocol::gate"),
        "{} must re-export from loom_protocol::gate",
        finding_path.display(),
    );
    assert!(
        !finding_body.contains("pub struct Finding"),
        "{} must not re-declare struct Finding (canonical home is loom-protocol::gate)",
        finding_path.display(),
    );
    assert!(
        !finding_body.contains("pub enum ConcernToken"),
        "{} must not re-declare enum ConcernToken",
        finding_path.display(),
    );

    let prev_path = workspace_root().join("crates/loom-templates/src/previous_failure.rs");
    let prev_body = std::fs::read_to_string(&prev_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", prev_path.display()));
    assert!(
        prev_body.contains("pub use loom_protocol::gate"),
        "{} must re-export BadWalk / TerminalSurface from loom_protocol::gate",
        prev_path.display(),
    );
    assert!(
        !prev_body.contains("pub enum BadWalk"),
        "{} must not re-declare enum BadWalk",
        prev_path.display(),
    );
    assert!(
        !prev_body.contains("pub enum TerminalSurface"),
        "{} must not re-declare enum TerminalSurface",
        prev_path.display(),
    );
}

/// `GitOid`'s canonical home is `loom-protocol::oid` (a public-contract
/// leaf), so `loom-templates` can carry it in
/// `PreviousFailure::IntegrationConflict { new_base_sha: GitOid }`
/// without depending on `loom-driver` (gix / rusqlite / tokio). This
/// test pins both halves of that decision: the type is reachable from
/// the leaf, and `loom-templates`' `[dependencies]` never names
/// `loom-driver`.
#[test]
fn loom_templates_reaches_git_oid_from_loom_protocol_leaf_without_loom_driver() {
    let oid = loom_protocol::oid::GitOid::new("deadbeefcafe1234567890abcdef0123456789ab")
        .expect("valid sha-1 oid");
    assert_eq!(oid.as_str(), "deadbeefcafe1234567890abcdef0123456789ab");

    let manifest = workspace_root().join("crates/loom-templates/Cargo.toml");
    let body = std::fs::read_to_string(&manifest)
        .unwrap_or_else(|e| panic!("read {}: {e}", manifest.display()));
    let parsed: toml::Value = toml::from_str(&body).expect("parse Cargo.toml");
    let deps = parsed
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .expect("[dependencies] table present");
    assert!(
        !deps.contains_key("loom-driver"),
        "loom-templates must not depend on loom-driver — GitOid lives in the \
         loom-protocol leaf so the integration-conflict retry context stays \
         driver-free",
    );
}

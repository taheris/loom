//! Verifies the `loom-workflow::review::finding` /
//! `loom-workflow::todo::exit` re-export shape from
//! `loom-protocol::gate`, per `specs/gate.md` § *`loom-protocol` crate*.
//!
//! `WalkOutput` / `WalkOutputError` / `parse_walk_output` and
//! `ExitSignal` / `parse_exit_signal` moved to `loom-protocol::gate`
//! in the carve-out diff; `loom-workflow` re-exports them so existing
//! `loom_workflow::review::{WalkOutput, ...}` and
//! `loom_workflow::todo::{ExitSignal, parse_exit_signal}` imports
//! continue to compile unchanged.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .expect("workspace root reachable from CARGO_MANIFEST_DIR")
}

/// `loom-workflow::review` and `loom-workflow::todo` re-export the
/// `WalkOutput` / `WalkOutputError` / `parse_walk_output` and
/// `ExitSignal` / `parse_exit_signal` surface from
/// `loom-protocol::gate`. Compile-time identity assertions pin each
/// re-export to the canonical type, and file-content checks pin that
/// the workflow module bodies are *re-exports*, not parallel
/// definitions.
#[test]
fn loom_workflow_re_exports_walk_output_and_exit_signal_from_loom_protocol() {
    fn identity<T>(x: T) -> T {
        x
    }

    // `WalkOutput` only ships a `pub` `from_stdout` constructor — exercise it
    // with a no-op validator and pin the type-identity at the boundary.
    struct AcceptAll;
    impl loom_protocol::gate::FindingValidator for AcceptAll {
        fn spec_label_is_known(&self, _: &loom_events::identifier::SpecLabel) -> bool {
            true
        }
        fn criterion_anchor_resolves(
            &self,
            _: &loom_events::identifier::SpecLabel,
            _: &str,
        ) -> bool {
            true
        }
        fn annotation_resolves(&self, _: &str) -> bool {
            true
        }
        fn file_exists(&self, _: &str) -> bool {
            true
        }
        fn invariant_resolves(
            &self,
            _: &loom_events::identifier::SpecLabel,
            _: &str,
            _: &str,
        ) -> bool {
            true
        }
    }
    let proto_walk = loom_protocol::gate::WalkOutput::from_stdout("LOOM_COMPLETE\n", &AcceptAll);
    let _: loom_workflow::review::WalkOutput = identity(proto_walk);

    // The error type rides through unchanged.
    let proto_err =
        loom_protocol::gate::WalkOutputError::MissingTerminalMarker { findings_count: 1 };
    let _: loom_workflow::review::WalkOutputError = identity(proto_err);

    // `ExitSignal` and `parse_exit_signal` re-export from the same home.
    let proto_signal = loom_protocol::gate::ExitSignal::Complete;
    let _: loom_workflow::todo::ExitSignal = identity(proto_signal);

    let parsed =
        loom_workflow::todo::parse_exit_signal("LOOM_COMPLETE\n").expect("complete marker parses");
    let _: loom_protocol::gate::ExitSignal = identity(parsed);

    // Function-pointer identity pin for parse_walk_output: same signature
    // on both sides means the re-export resolves to the canonical fn.
    let proto_fn: fn(&str, &AcceptAll) -> Result<Vec<_>, _> =
        loom_protocol::gate::parse_walk_output::<AcceptAll>;
    let workflow_fn: fn(&str, &AcceptAll) -> Result<Vec<_>, _> =
        loom_workflow::review::parse_walk_output::<AcceptAll>;
    assert_eq!(
        proto_fn as usize, workflow_fn as usize,
        "re-export must resolve to the same fn pointer as the canonical home",
    );

    // Module-body re-export shape pin.
    let finding_path = workspace_root().join("crates/loom-workflow/src/review/finding.rs");
    let finding_body = std::fs::read_to_string(&finding_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", finding_path.display()));
    assert!(
        finding_body.contains("pub use loom_protocol::gate"),
        "{} must re-export from loom_protocol::gate",
        finding_path.display(),
    );
    assert!(
        !finding_body.contains("pub struct WalkOutput"),
        "{} must not re-declare struct WalkOutput",
        finding_path.display(),
    );
    assert!(
        !finding_body.contains("pub fn parse_walk_output"),
        "{} must not re-declare parse_walk_output",
        finding_path.display(),
    );

    let exit_path = workspace_root().join("crates/loom-workflow/src/todo/exit.rs");
    let exit_body = std::fs::read_to_string(&exit_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", exit_path.display()));
    assert!(
        exit_body.contains("pub use loom_protocol::gate"),
        "{} must re-export from loom_protocol::gate",
        exit_path.display(),
    );
    assert!(
        !exit_body.contains("pub enum ExitSignal"),
        "{} must not re-declare ExitSignal",
        exit_path.display(),
    );
    assert!(
        !exit_body.contains("pub fn parse_exit_signal"),
        "{} must not re-declare parse_exit_signal",
        exit_path.display(),
    );
}

#![allow(clippy::unwrap_used)]
//! End-to-end coverage for [`InputResolver`] sitting on top of the
//! live `cargo metadata`-backed [`CargoMetadataScope`].
//!
//! The inline tests in `src/inputs.rs` cover the pure source paths
//! (judge collect mode, `--print-inputs` spawn, heuristics, override)
//! against synthetic fixtures. This integration test wires the resolver
//! to the real workspace's cargo metadata so the `[test]` source
//! actually examines the loom-gate crate's transitive dep closure.

use std::path::PathBuf;
use std::process::Command;

use loom_gate::annotation::{Annotation, Tier};
use loom_gate::inputs::{InputResolver, filter_by_files};
use loom_gate::scope::CargoMetadataScope;

fn workspace_manifest() -> PathBuf {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .parent()
        .and_then(std::path::Path::parent)
        .map(|p| p.join("Cargo.toml"))
        .unwrap()
}

fn workspace_root() -> PathBuf {
    workspace_manifest().parent().unwrap().to_path_buf()
}

fn cargo_available() -> bool {
    Command::new("cargo")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
}

fn ann(target: &str) -> Annotation {
    Annotation {
        tier: Tier::Test,
        target: target.into(),
        source_spec: PathBuf::from("specs/gate.md"),
        line: 1,
        criterion_line: 1,
        pending: false,
    }
}

#[test]
fn test_tier_resolution_uses_cargo_metadata_plus_spec_autoinclude() {
    if !cargo_available() {
        return;
    }
    let scope = CargoMetadataScope::from_manifest(&workspace_manifest()).unwrap();
    let mut resolver = InputResolver::new(workspace_root()).with_test_scope(Box::new(scope));
    let inputs = resolver.resolve(&ann("loom_gate::dispatch::ok"));
    assert!(
        inputs.paths.contains(&PathBuf::from("specs/gate.md")),
        "spec auto-include must be present: {:?}",
        inputs.paths,
    );
    let owns_dispatch = inputs
        .paths
        .iter()
        .any(|p| p.ends_with("crates/loom-gate/src/dispatch.rs"));
    assert!(
        owns_dispatch,
        "owning crate source must appear in declared inputs: {:?}",
        inputs.paths,
    );
    let pulls_loom_events = inputs
        .paths
        .iter()
        .any(|p| p.ends_with("crates/loom-events/src/lib.rs"));
    assert!(
        pulls_loom_events,
        "transitive dep source must appear in declared inputs: {:?}",
        inputs.paths,
    );
}

/// Scope-by-files end-to-end: a staged `.pre-commit-config.yaml` keeps
/// only annotations the file could affect. A `[test]` annotation whose
/// declared inputs (its owning crate's sources plus the spec
/// auto-include) are disjoint from the staged file drops out; a `[check]`
/// whose command references the staged file is kept by the heuristic.
/// The dropped annotation must declare inputs of its own — a no-input
/// verifier would always run under the Conservative default.
#[test]
fn filter_by_files_drops_unrelated_check_annotations_against_a_yaml_staged_file() {
    if !cargo_available() {
        return;
    }
    let scope = CargoMetadataScope::from_manifest(&workspace_manifest()).unwrap();
    let mut resolver = InputResolver::new(workspace_root()).with_test_scope(Box::new(scope));

    let annotations = vec![
        Annotation {
            tier: Tier::Test,
            target: "loom_gate::dispatch::ok".into(),
            source_spec: PathBuf::from("specs/gate.md"),
            line: 10,
            criterion_line: 10,
            pending: false,
        },
        Annotation {
            tier: Tier::Check,
            target: "grep -q 'verify-marker' .pre-commit-config.yaml".into(),
            source_spec: PathBuf::from("specs/pre-commit.md"),
            line: 20,
            criterion_line: 20,
            pending: false,
        },
    ];
    let files = vec![PathBuf::from(".pre-commit-config.yaml")];
    let got = filter_by_files(&annotations, &files, &mut resolver);
    assert_eq!(
        got.len(),
        1,
        "only the annotation whose command references the staged file should remain: {got:?}"
    );
    assert_eq!(
        got[0].target,
        "grep -q 'verify-marker' .pre-commit-config.yaml"
    );
}

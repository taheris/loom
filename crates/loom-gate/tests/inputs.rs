#![allow(clippy::unwrap_used)]
//! End-to-end coverage for [`InputResolver`] sitting on top of the
//! live `cargo metadata`-backed [`CargoMetadataScope`].
//!
//! The inline tests in `src/inputs.rs` cover the pure source paths
//! (script-header parsing, `--print-inputs` spawn, heuristics, override)
//! against synthetic fixtures. This integration test wires the resolver
//! to the real workspace's cargo metadata so the `[test]` source
//! actually examines the loom-gate crate's transitive dep closure.

use std::path::PathBuf;
use std::process::Command;

use loom_gate::annotation::{Annotation, Tier};
use loom_gate::inputs::InputResolver;
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

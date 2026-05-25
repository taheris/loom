#![allow(clippy::unwrap_used)]
//! End-to-end coverage for [`CargoMetadataScope::from_manifest`].
//!
//! The inline tests in `src/scope.rs` cover the pure logic (key extraction,
//! transitive closure, file walking) and the synthetic two-crate fixture
//! by feeding hand-rolled metadata JSON into a private constructor. This
//! integration test exercises the live `cargo metadata` subprocess by
//! pointing the resolver at this workspace and asserting that a known
//! workspace crate's scope contains both its own source files and a
//! known dependency crate's source files.

use std::path::PathBuf;
use std::process::Command;

use loom_gate::annotation::{Annotation, Tier};
use loom_gate::dispatch::TestScope;
use loom_gate::scope::CargoMetadataScope;

fn workspace_manifest() -> PathBuf {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .parent()
        .and_then(std::path::Path::parent)
        .map(|p| p.join("Cargo.toml"))
        .unwrap()
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
        source_spec: PathBuf::from("specs/tests.md"),
        line: 1,
        criterion_line: 1,
    }
}

#[test]
fn live_workspace_scope_includes_own_files_and_transitive_dep_files() {
    if !cargo_available() {
        return;
    }
    let scope = CargoMetadataScope::from_manifest(&workspace_manifest()).unwrap();

    let files = scope.scope_for(&ann("loom_gate::dispatch::ok"));
    assert!(
        !files.is_empty(),
        "loom_gate must resolve to a non-empty scope from the live workspace"
    );

    let owns_dispatch = files
        .iter()
        .any(|p| p.ends_with("crates/loom-gate/src/dispatch.rs"));
    assert!(
        owns_dispatch,
        "owning crate's own dispatch.rs must be in scope: {files:?}"
    );

    let pulls_loom_events = files
        .iter()
        .any(|p| p.ends_with("crates/loom-events/src/lib.rs"));
    assert!(
        pulls_loom_events,
        "transitive dep loom-events lib.rs must be in scope: {files:?}"
    );
}

#[test]
fn live_workspace_scope_for_unknown_crate_is_empty() {
    if !cargo_available() {
        return;
    }
    let scope = CargoMetadataScope::from_manifest(&workspace_manifest()).unwrap();
    let files = scope.scope_for(&ann("definitely_not_a_crate::tests::x"));
    assert!(
        files.is_empty(),
        "unknown crate must produce empty scope, got {files:?}"
    );
}

#[test]
fn live_workspace_scope_for_crate_placeholder_target_is_empty() {
    if !cargo_available() {
        return;
    }
    let scope = CargoMetadataScope::from_manifest(&workspace_manifest()).unwrap();
    let files = scope.scope_for(&ann("crate::placeholder::x"));
    assert!(
        files.is_empty(),
        "literal `crate::` cannot disambiguate workspace package, got {files:?}"
    );
}

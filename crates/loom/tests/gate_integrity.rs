//! Live-path tests for the integrity gate wired into `loom gate check`
//! and `loom gate verify`.
//!
//! `specs/gate.md` § Integrity gate pins that every `loom gate
//! check` run includes a self-test of the gate's resolution logic, and
//! that the integrity gate is itself a `[check]`-tier verifier — so it
//! must also surface during `loom gate verify`. Findings are terminal:
//! a surfaced finding fails the run with a non-zero exit code, matching
//! the spec's "Integrity findings are terminal" contract for the push
//! gate and the broader `[check]`-tier semantics for the verify lane.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::Command;

fn write_spec_with_unresolvable_annotation(workspace: &Path, label: &str) {
    let specs_dir = workspace.join("specs");
    std::fs::create_dir_all(&specs_dir).expect("mkdir specs");
    std::fs::write(
        specs_dir.join(format!("{label}.md")),
        "## Success Criteria\n\n\
         - resolved criterion [check](true)\n\
         - unresolved criterion \
           [check](definitely-not-a-real-command-xyz-integrity-test)\n",
    )
    .expect("write spec");
}

/// Discover `true`'s directory on the ambient `PATH` so we can pin a
/// minimal `PATH` for the loom child without assuming `/usr/bin:/bin`
/// — those paths are empty on NixOS-style hosts where coreutils lives
/// under `/nix/store/...`. The pinned `PATH` still excludes everything
/// else, so `definitely-not-a-real-command-xyz-integrity-test` remains
/// provably absent.
fn pinned_path() -> String {
    let path_var = std::env::var_os("PATH").expect("PATH must be set");
    for dir in std::env::split_paths(&path_var) {
        if dir.join("true").is_file() {
            return dir.to_string_lossy().into_owned();
        }
    }
    panic!("could not locate `true` on PATH={path_var:?}");
}

fn run_loom_gate(workspace: &Path, subcommand: &str) -> std::process::Output {
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .arg("gate")
        .arg(subcommand)
        .arg("--tree")
        .env_remove("LOOM_INSIDE")
        .env("PATH", pinned_path())
        .output()
        .expect("spawn loom")
}

#[test]
fn gate_check_fails_on_integrity_finding_for_unresolved_annotation() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    write_spec_with_unresolvable_annotation(workspace, "integrity_check");

    let output = run_loom_gate(workspace, "check");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "loom gate check must exit non-zero when an integrity finding \
         surfaces. status={:?}\nstderr={stderr}",
        output.status,
    );
    assert!(
        stderr.contains("loom gate [integrity]"),
        "stderr must label the integrity-gate finding. stderr:\n{stderr}",
    );
    assert!(
        stderr.contains("definitely-not-a-real-command-xyz-integrity-test"),
        "stderr must name the unresolved target. stderr:\n{stderr}",
    );
    assert!(
        stderr.contains("does not resolve"),
        "stderr must use the spec-prescribed `does not resolve` wording. \
         stderr:\n{stderr}",
    );
}

#[test]
fn gate_verify_fails_on_integrity_finding_for_unresolved_annotation() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    write_spec_with_unresolvable_annotation(workspace, "integrity_verify");

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .args(["gate", "verify", "--tree"])
        .env("LOOM_VERIFY_TIERS", "check")
        .env_remove("LOOM_INSIDE")
        .env("PATH", pinned_path())
        .output()
        .expect("spawn loom");

    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "loom gate verify must exit non-zero when an integrity finding \
         surfaces. status={:?}\nstderr={stderr}",
        output.status,
    );
    assert!(
        stderr.contains("loom gate [integrity]"),
        "stderr must label the integrity-gate finding under verify. \
         stderr:\n{stderr}",
    );
    assert!(
        stderr.contains("definitely-not-a-real-command-xyz-integrity-test"),
        "stderr must name the unresolved target. stderr:\n{stderr}",
    );
}

/// Regression: pending `[test?](plain_leaf)` annotation whose target
/// resolves in the workspace must fire `UnneededPendingMarker` under
/// `loom gate verify --files <paths>` even when `<paths>` excludes the
/// spec file. Plain test-leaf names have no `crate::`-prefix segment, so
/// `CargoMetadataScope::scope_for` returns an empty scope and
/// `filter_by_files` previously collapsed the annotation's declared
/// inputs to the spec file alone — dropping the annotation before
/// forward-resolution ever ran. The pending modifier's self-cleaning
/// contract (specs/gate.md § Pending modifier) requires forward-
/// resolution at every gate scope.
#[test]
fn gate_verify_files_fires_unneeded_pending_marker_for_plain_test_leaf_when_spec_excluded() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let specs_dir = workspace.join("specs");
    std::fs::create_dir_all(&specs_dir).unwrap();
    // The pending target lives in `pending.md`, which is NOT in the
    // staged set. `check_anchor.md` carries the Check annotation that
    // keeps `dispatch_tier`'s selected set non-empty (so the integrity
    // gate is reached at all); production diffs that hit this bug
    // similarly carry unrelated Check-tier annotations against staged
    // sources.
    std::fs::write(
        specs_dir.join("check_anchor.md"),
        "## Success Criteria\n\n- anchor [check](true)\n",
    )
    .unwrap();
    std::fs::write(
        specs_dir.join("pending.md"),
        "## Success Criteria\n\n\
         - pending leaf [test?](pending_leaf_lm_8rdt_6_resolved)\n",
    )
    .unwrap();

    // The pending target is a plain leaf — no `crate::` prefix, so the
    // production `CargoMetadataScope` cannot map it to a workspace
    // package. The leaf still resolves because `scan_workspace_pair`
    // walks every `.rs` file under the workspace.
    let src_dir = workspace.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(
        src_dir.join("test_landing.rs"),
        "#[test]\nfn pending_leaf_lm_8rdt_6_resolved() { assert_eq!(2 + 2, 4); }\n",
    )
    .unwrap();

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .args(["gate", "verify", "--files", "specs/check_anchor.md"])
        .env("LOOM_VERIFY_TIERS", "check")
        .env_remove("LOOM_INSIDE")
        .env("PATH", pinned_path())
        .output()
        .expect("spawn loom");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "pre-commit-shaped invocation must refuse when a pending marker \
         is stale — even when the spec file is outside the staged set. \
         status={:?}\nstdout={stdout}\nstderr={stderr}",
        output.status,
    );
    assert!(
        stderr.contains("loom gate [integrity]"),
        "stderr must label the integrity-gate finding. \
         stderr:\n{stderr}",
    );
    assert!(
        stderr.contains("pending_leaf_lm_8rdt_6_resolved"),
        "stderr must name the resolved pending target. stderr:\n{stderr}",
    );
    assert!(
        stderr.contains("drop the ? marker"),
        "stderr must carry the spec-prescribed wording. stderr:\n{stderr}",
    );
}

#[test]
fn gate_verify_tree_ignores_loom_verify_tiers_env() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let specs_dir = workspace.join("specs");
    std::fs::create_dir_all(&specs_dir).unwrap();
    std::fs::write(
        specs_dir.join("env_ignored.md"),
        "## Success Criteria\n\n- system still runs [system](false)\n",
    )
    .unwrap();

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .args(["gate", "verify", "--tree"])
        .env("LOOM_VERIFY_TIERS", "check")
        .env_remove("LOOM_INSIDE")
        .env("PATH", pinned_path())
        .output()
        .expect("spawn loom");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "tree verify must run [system] from scope, ignoring LOOM_VERIFY_TIERS. stderr={stderr}",
    );
    assert!(
        stderr.contains("loom gate [system] FAIL: false"),
        "stderr must show system tier ran despite env override: {stderr}",
    );
}

#[test]
fn gate_check_is_silent_when_every_annotation_resolves() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let specs_dir = workspace.join("specs");
    std::fs::create_dir_all(&specs_dir).unwrap();
    // Single annotation pointing at `true`, which always resolves on a
    // coreutils PATH. No second criterion → no atomic-acceptance flag.
    std::fs::write(
        specs_dir.join("integrity_clean.md"),
        "## Success Criteria\n\n- a criterion [check](true)\n",
    )
    .unwrap();

    let output = run_loom_gate(workspace, "check");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "loom gate check must exit 0 when no integrity finding surfaces \
         and the lone annotation passes. stdout={stdout}\nstderr={stderr}",
    );
    assert!(
        !stderr.contains("loom gate [integrity]:"),
        "stderr must NOT carry an integrity finding line when the gate \
         passes clean. stderr:\n{stderr}",
    );
}

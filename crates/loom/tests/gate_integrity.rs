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

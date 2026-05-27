//! Live-path test for `loom gate audit --tree --spec <X>` refusing to
//! start when no open epic exists for `<X>`.
//!
//! `specs/gate.md` § *Standing-safety-net checks* mandates this refusal
//! for the per-spec branch of the standing safety net: the agent must
//! not spawn until a fresh molecule is created via `loom todo`. The
//! integrity-gate's CLI smoke tests in `gate_integrity.rs` cover the
//! deterministic-tier refuse cases; this file pins the audit-tier
//! refuse path.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::Command;

fn pinned_path() -> String {
    let path_var = std::env::var_os("PATH").expect("PATH must be set");
    for dir in std::env::split_paths(&path_var) {
        if dir.join("true").is_file() {
            return dir.to_string_lossy().into_owned();
        }
    }
    panic!("could not locate `true` on PATH={path_var:?}");
}

fn run_loom_gate_audit(workspace: &Path, label: &str) -> std::process::Output {
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .args(["gate", "audit", "--tree", "--spec", label])
        .env_remove("LOOM_INSIDE")
        .env("PATH", pinned_path())
        .output()
        .expect("spawn loom")
}

#[test]
fn tree_scope_refuses_when_no_current_molecule_for_spec() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    // No bd shim on PATH and no open epic anywhere — the refuse path
    // fires before any agent spawn.
    let output = run_loom_gate_audit(workspace, "acme");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "loom gate audit --tree --spec acme must exit non-zero when no \
         open epic exists for acme. status={:?}\nstderr={stderr}",
        output.status,
    );
    assert!(
        stderr.contains("acme"),
        "stderr must name the spec. stderr:\n{stderr}",
    );

    let logs_dir = workspace.join(".wrapix/loom/logs");
    let log_entries: Vec<_> = std::fs::read_dir(&logs_dir)
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default();
    assert!(
        log_entries.is_empty(),
        "no agent log must be written when the refuse path fires. \
         entries={log_entries:?}",
    );
}

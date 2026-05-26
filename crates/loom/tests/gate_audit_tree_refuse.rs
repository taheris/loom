//! Live-path test for `loom gate audit --tree --spec <X>` refusing to
//! start when state.db has no `current_molecule[<X>]` pointer.
//!
//! `specs/gate.md` § *Standing-safety-net checks* mandates this refusal
//! for the per-spec branch of the standing safety net: the agent must
//! not spawn until the user seeds the pointer via `loom use <X> --epic
//! <id>` or creates a fresh molecule via `loom todo --spec <X>`. The
//! integrity-gate's CLI smoke tests in `gate_integrity.rs` cover the
//! deterministic-tier refuse cases; this file pins the audit-tier
//! refuse path.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::Command;

use loom_driver::identifier::SpecLabel;
use loom_driver::state::StateDb;

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

    let db_path = workspace.join(".wrapix/loom/state.db");
    let db = StateDb::open(&db_path).expect("open state.db");
    assert!(
        db.current_molecule(&SpecLabel::new("acme"))
            .expect("read current_molecule")
            .is_none(),
        "test premise: current_molecule[acme] must be unset",
    );
    drop(db);

    let output = run_loom_gate_audit(workspace, "acme");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "loom gate audit --tree --spec acme must exit non-zero when \
         current_molecule[acme] is empty. status={:?}\nstderr={stderr}",
        output.status,
    );
    assert!(
        stderr.contains("acme"),
        "stderr must name the spec. stderr:\n{stderr}",
    );
    assert!(
        stderr.contains("loom use acme --epic"),
        "stderr must point at `loom use <label> --epic <id>`. stderr:\n{stderr}",
    );
    assert!(
        stderr.contains("loom todo --spec acme"),
        "stderr must point at `loom todo --spec <label>`. stderr:\n{stderr}",
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

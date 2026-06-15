//! Spec verification tests for `specs/gate.md` § *Commands surface — bare
//! gate and status*. The Commands table pins bare `loom gate` and bare
//! inspection subcommands as help surfaces with no verifier or cache work.
//!
//! Spec targets covered:
//! - `bare_loom_gate_prints_subcommand_help`
//! - `bare_loom_gate_verify_prints_help_and_runs_nothing`
//! - `loom_gate_status_requires_explicit_scope`
//! - `loom_gate_status_is_allowed_under_loom_inside_env`

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

fn loom_bin() -> &'static str {
    env!("CARGO_BIN_EXE_loom")
}

/// Bare `loom gate` (no subcommand) renders the same help text as
/// `loom gate --help` and exits 0. No verifiers run, no cache read.
#[test]
fn bare_loom_gate_prints_subcommand_help() {
    let bare = Command::new(loom_bin())
        .arg("gate")
        .env("COLUMNS", "100")
        .env("CLAP_TERM_WIDTH", "100")
        .output()
        .expect("spawn loom gate");
    assert!(
        bare.status.success(),
        "bare `loom gate` must exit 0, got stderr={}",
        String::from_utf8_lossy(&bare.stderr),
    );
    let bare_stdout = String::from_utf8(bare.stdout).expect("utf-8");

    let help = Command::new(loom_bin())
        .args(["gate", "--help"])
        .env("COLUMNS", "100")
        .env("CLAP_TERM_WIDTH", "100")
        .output()
        .expect("spawn loom gate --help");
    let help_stdout = String::from_utf8(help.stdout).expect("utf-8");

    assert_eq!(
        bare_stdout, help_stdout,
        "bare `loom gate` must print identical output to `loom gate --help`",
    );
    assert!(
        bare_stdout.contains("\n  status "),
        "help output must list the `status` subcommand row, got:\n{bare_stdout}",
    );
}

#[test]
fn bare_loom_gate_verify_prints_help_and_runs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::create_dir_all(workspace.join(".loom")).unwrap();
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(workspace.join("specs/dummy.md"), "# dummy\n").unwrap();

    let out = Command::new(loom_bin())
        .arg("--workspace")
        .arg(workspace)
        .args(["gate", "verify"])
        .output()
        .expect("spawn loom gate verify");
    assert!(
        out.status.success(),
        "bare `loom gate verify` must exit 0, got stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    assert!(
        stdout.contains("Usage: loom gate verify"),
        "bare verify must print subcommand help, got:\n{stdout}",
    );
    assert!(
        !workspace.join(".loom/cache.db").exists(),
        "bare verify must not open the gate cache",
    );
}

#[test]
fn loom_gate_status_requires_explicit_scope() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::create_dir_all(workspace.join(".loom")).unwrap();
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(workspace.join("specs/dummy.md"), "# dummy\n").unwrap();

    let out = Command::new(loom_bin())
        .arg("--workspace")
        .arg(workspace)
        .args(["gate", "status"])
        .output()
        .expect("spawn loom gate status");
    assert!(
        out.status.success(),
        "bare `loom gate status` must print help and exit 0, got stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    assert!(
        stdout.contains("Usage: loom gate status"),
        "bare status must print help, got:\n{stdout}",
    );
    assert!(
        !workspace.join(".loom/cache.db").exists(),
        "bare status must not open the gate cache",
    );
}

/// `loom gate status` is read-only relative to workspace state and the
/// nested-loom guard must allow it under `LOOM_INSIDE=1`.
#[test]
fn loom_gate_status_is_allowed_under_loom_inside_env() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::create_dir_all(workspace.join(".loom")).unwrap();
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(workspace.join("specs/dummy.md"), "# dummy\n").unwrap();

    let out = Command::new(loom_bin())
        .arg("--workspace")
        .arg(workspace)
        .args(["gate", "status", "--tree"])
        .env("LOOM_INSIDE", "1")
        .env_remove("LOOM_PROFILES_MANIFEST")
        .output()
        .expect("spawn loom gate status under LOOM_INSIDE=1");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("loom cannot run inside"),
        "`loom gate status` must bypass the nested-loom guard, got:\n{stderr}",
    );
    assert!(
        out.status.success(),
        "`loom gate status` under LOOM_INSIDE=1 must exit 0, got stderr={stderr}",
    );
}

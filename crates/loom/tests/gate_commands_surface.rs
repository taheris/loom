//! Spec verification tests for `specs/gate.md` § *Commands surface — bare
//! gate and status*. The Commands table pins bare `loom gate` as a help
//! surface (no verifiers, no cache read) and `loom gate status` as the
//! cache-read subcommand inheriting the bare-invocation scope default.
//!
//! Spec targets covered:
//! - `bare_loom_gate_prints_subcommand_help`
//! - `loom_gate_status_subcommand_reads_cache_with_default_scope`
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

/// `loom gate status` runs against an empty workspace cache and prints
/// the "no cached verifier runs yet" line — confirming the subcommand
/// reads the sqlite cache instead of routing somewhere else. The default
/// scope path is exercised (no scope flag passed); the bare-invocation
/// scope-default helper degrades to `--diff HEAD` on a fresh workspace,
/// which is the contract for this case.
#[test]
fn loom_gate_status_subcommand_reads_cache_with_default_scope() {
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
        "`loom gate status` must exit 0 on a fresh workspace, got stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    assert!(
        stdout.contains("no cached verifier runs yet"),
        "`loom gate status` on a fresh workspace must report an empty cache, got:\n{stdout}",
    );
    assert!(
        workspace.join(".loom/gate-cache.sqlite").exists(),
        "`loom gate status` must open (and so create) the sqlite cache file",
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
        .args(["gate", "status"])
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

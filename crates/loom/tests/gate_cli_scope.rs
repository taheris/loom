#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn loom_bin() -> &'static str {
    env!("CARGO_BIN_EXE_loom")
}

fn git_command() -> Command {
    let mut command = Command::new("git");
    loom_test_support::scrub_git_local_env(&mut command);
    command
}

fn prepend_path(dir: &Path) -> std::ffi::OsString {
    let ambient = std::env::var_os("PATH").unwrap_or_default();
    let mut entries: Vec<PathBuf> = vec![dir.to_path_buf()];
    entries.extend(std::env::split_paths(&ambient));
    std::env::join_paths(entries).expect("join PATH")
}

fn write_executable(path: &Path, body: &str) {
    std::fs::write(path, body).expect("write executable");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn init_git(workspace: &Path) {
    let run = |args: &[&str]| {
        let ok = git_command()
            .arg("-C")
            .arg(workspace)
            .args(args)
            .status()
            .expect("spawn git")
            .success();
        assert!(ok, "git {args:?} failed");
    };
    run(&["init", "-q", "-b", "main"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test"]);
    run(&["config", "commit.gpgsign", "false"]);
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "init"]);
}

#[test]
fn gate_verify_rejects_positional_selector() {
    let out = Command::new(loom_bin())
        .args(["gate", "verify", "cargo test --lib"])
        .output()
        .expect("spawn loom");
    assert!(
        !out.status.success(),
        "positional target selector must be rejected",
    );
}

#[test]
fn gate_target_exact_match_and_ambiguity_rules() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(
        workspace.join("specs/ambiguous.md"),
        "## Success Criteria\n\n- check [check](same-target)\n- test [test](same-target)\n",
    )
    .unwrap();

    let ambiguous = Command::new(loom_bin())
        .arg("--workspace")
        .arg(workspace)
        .args(["gate", "verify", "--target", "same-target"])
        .output()
        .expect("spawn loom");
    let stderr = String::from_utf8_lossy(&ambiguous.stderr);
    assert!(
        !ambiguous.status.success() && stderr.contains("multiple tiers"),
        "verify --target must reject cross-tier ambiguity: {stderr}",
    );

    std::fs::write(
        workspace.join("specs/ambiguous.md"),
        "## Success Criteria\n\n- one [check](true)\n- two [check](true)\n",
    )
    .unwrap();
    let duplicate = Command::new(loom_bin())
        .arg("--workspace")
        .arg(workspace)
        .args(["gate", "check", "--target", "true"])
        .output()
        .expect("spawn loom");
    assert!(
        duplicate.status.success(),
        "tier subcommand must accept same-target duplicates: stderr={}",
        String::from_utf8_lossy(&duplicate.stderr),
    );
}

#[test]
fn verify_diff_runs_prek_pre_commit_lane_before_annotations() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::create_dir_all(workspace.join("bin")).unwrap();
    let order_log = workspace.join("order.log");
    std::fs::write(
        workspace.join("specs/gate.md"),
        "## Success Criteria\n\n- check after hooks [check](sh check.sh)\n",
    )
    .unwrap();
    write_executable(
        &workspace.join("check.sh"),
        "#!/usr/bin/env bash\nset -euo pipefail\nprintf 'check\n' >> \"$ORDER_LOG\"\n",
    );
    write_executable(
        &workspace.join("bin/prek"),
        "#!/usr/bin/env bash\nset -euo pipefail\nprintf 'prek %s\n' \"$*\" >> \"$ORDER_LOG\"\n",
    );
    init_git(workspace);

    let out = Command::new(loom_bin())
        .arg("--workspace")
        .arg(workspace)
        .args(["gate", "verify", "--diff", "HEAD..HEAD"])
        .env("PATH", prepend_path(&workspace.join("bin")))
        .env("ORDER_LOG", &order_log)
        .output()
        .expect("spawn loom");
    assert!(
        out.status.success(),
        "diff verify must pass: stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let log = std::fs::read_to_string(order_log).expect("order log");
    let mut lines = log.lines();
    let first = lines.next().unwrap_or_default();
    let second = lines.next().unwrap_or_default();
    assert!(
        first.starts_with("prek run --hook-stage pre-commit --from-ref "),
        "first lane must be concrete prek pre-commit: {log}",
    );
    assert_eq!(
        second, "check",
        "annotation lane must run after prek: {log}"
    );
}

#[test]
fn nested_verify_files_under_parent_diff_gate_records_skip() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(
        workspace.join("specs/gate.md"),
        "## Success Criteria\n\n- check [check](false)\n",
    )
    .unwrap();
    let out = Command::new(loom_bin())
        .arg("--workspace")
        .arg(workspace)
        .args(["gate", "verify", "--files", "specs/gate.md"])
        .env("LOOM_PARENT_DIFF_GATE", "1")
        .output()
        .expect("spawn loom");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stderr.contains("skipped under parent --diff gate"),
        "nested files gate must skip under parent diff gate: {stderr}",
    );
}

#[test]
fn verify_tier_policy_is_scope_derived_without_env_override() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(
        workspace.join("specs/gate.md"),
        "## Success Criteria\n\n- system still runs [system](false)\n",
    )
    .unwrap();
    let out = Command::new(loom_bin())
        .arg("--workspace")
        .arg(workspace)
        .args(["gate", "verify", "--tree"])
        .env("LOOM_VERIFY_TIERS", "check")
        .output()
        .expect("spawn loom");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && stderr.contains("loom gate [system] FAIL: false"),
        "tree scope must run [system] despite LOOM_VERIFY_TIERS: {stderr}",
    );
}

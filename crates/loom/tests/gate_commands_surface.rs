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

use std::path::Path;
use std::process::{Command, Output};

const fn loom_bin() -> &'static str {
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
    let dir = verifier_workspace();
    let workspace = dir.path();

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
        !workspace.join(".loom").exists(),
        "bare status must not open the gate cache or create logs",
    );
    assert!(!workspace.join("invocations.log").exists());
    let help = gate_command(workspace, "status", &["--help"]);
    assert!(help.status.success());
    assert_eq!(stdout.as_bytes(), help.stdout);
}

fn gate_command(workspace: &Path, subcommand: &str, args: &[&str]) -> Output {
    Command::new(loom_bin())
        .current_dir(workspace)
        .arg("--workspace")
        .arg(workspace)
        .args(["gate", subcommand])
        .args(args)
        .env_remove("LOOM_INSIDE")
        .env_remove("LOOM_PARENT_DIFF_GATE")
        .output()
        .expect("spawn loom gate")
}

fn verifier_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    std::fs::write(workspace.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    std::fs::write(
        workspace.join("specs/fixture.md"),
        "## Success Criteria\n\n\
         - selected check [check](check-selected)\n\
         - sibling check [check](check-sibling)\n\
         - selected system [system](system-selected)\n\
         - sibling system [system](system-sibling)\n",
    )
    .unwrap();
    std::fs::write(
        workspace.join("loom.toml"),
        "[runner.check.fixture]\n\
         match = '^check-'\n\
         command = 'bash verifier.sh {targets}'\n\
         parse = 'json-lines'\n\n\
         [runner.system.fixture]\n\
         match = '^system-'\n\
         command = 'bash verifier.sh {targets}'\n\
         parse = 'json-lines'\n",
    )
    .unwrap();
    std::fs::write(
        workspace.join("verifier.sh"),
        "set -euo pipefail\n\
         printf '%s\\n' \"$*\" >> invocations.log\n\
         for target in \"$@\"; do\n\
           printf '{\"target\":\"%s\",\"pass\":true,\"evidence\":\"executed fixture\"}\\n' \"$target\"\n\
         done\n",
    )
    .unwrap();
    dir
}

fn assert_status_report(workspace: &Path, args: &[&str]) {
    let out = gate_command(workspace, "status", args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("fixture: 4 criteria"), "{stdout}");
    assert!(stdout.contains("no cached verifier runs yet"), "{stdout}");
    assert!(
        !workspace.join("invocations.log").exists(),
        "status must not execute verifiers"
    );
}

/// Process assertions cover clap rejection before SQLite or verifier dispatch.
#[test]
fn gate_status_rejects_target_before_cache_or_subprocess_work() {
    let dir = verifier_workspace();
    let workspace = dir.path();
    let cache_path = workspace.join(".loom/cache.db");
    for seeded in [false, true] {
        if seeded {
            std::fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
            std::fs::write(&cache_path, b"not a SQLite cache").unwrap();
            let file = std::fs::File::open(&cache_path).unwrap();
            file.set_times(std::fs::FileTimes::new().set_accessed(std::time::UNIX_EPOCH))
                .unwrap();
        }
        let before_access =
            seeded.then(|| std::fs::metadata(&cache_path).unwrap().accessed().unwrap());
        let out = gate_command(workspace, "status", &["--target", "check-selected"]);
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert_eq!(out.status.code(), Some(2), "{stderr}");
        assert!(
            stderr.contains("unexpected argument '--target'"),
            "{stderr}"
        );
        assert!(stderr.contains("Usage: loom gate status"), "{stderr}");
        assert!(stderr.contains("--help"), "{stderr}");
        assert!(!workspace.join("invocations.log").exists());
        if let Some(access) = before_access {
            assert_eq!(
                std::fs::metadata(&cache_path).unwrap().accessed().unwrap(),
                access
            );
            assert_eq!(std::fs::read(&cache_path).unwrap(), b"not a SQLite cache");
            let entries = std::fs::read_dir(cache_path.parent().unwrap())
                .unwrap()
                .count();
            assert_eq!(
                entries, 1,
                "no journals, logs, or other gate state may be created"
            );
        } else {
            assert!(
                !workspace.join(".loom").exists(),
                "rejected scope must not create cache or logs"
            );
        }
    }
}

/// The real binary must accept variadic file scopes without executing a verifier.
#[test]
fn gate_status_files_reports_without_executing_verifiers() {
    let dir = verifier_workspace();
    assert_status_report(dir.path(), &["--files", "src/lib.rs", "specs/fixture.md"]);
}

/// Tree status follows the same reporting path, not verifier dispatch.
#[test]
fn gate_status_tree_reports_without_executing_verifiers() {
    let dir = verifier_workspace();
    assert_status_report(dir.path(), &["--tree"]);
}

fn fixture_git(workspace: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    loom_test_support::scrub_git_local_env(&mut command);
    let out = command.current_dir(workspace).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Diff scopes require real Git resolution; an empty diff remains a valid scope.
#[test]
fn gate_status_diff_preserves_valid_empty_and_invalid_range_behavior() {
    let dir = verifier_workspace();
    let workspace = dir.path();
    fixture_git(workspace, &["init", "-q"]);
    fixture_git(workspace, &["config", "core.hooksPath", "/dev/null"]);
    fixture_git(workspace, &["add", "."]);
    fixture_git(
        workspace,
        &[
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "fixture",
        ],
    );
    std::fs::write(workspace.join("src/lib.rs"), "pub fn changed() {}\n").unwrap();
    assert_status_report(workspace, &["--diff", "HEAD"]);
    assert_status_report(workspace, &["--diff", "HEAD..HEAD"]);
    let out = gate_command(
        workspace,
        "status",
        &["--diff", "missing-status-base..HEAD"],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("missing-status-base..HEAD"));
    assert!(!workspace.join("invocations.log").exists());
}

/// Parser conflicts must remain early errors rather than selecting one scope.
#[test]
fn gate_status_scope_flags_remain_mutually_exclusive() {
    let dir = verifier_workspace();
    for args in [
        vec!["--files", "src/lib.rs", "--tree"],
        vec!["--files", "src/lib.rs", "--diff", "HEAD"],
        vec!["--diff", "HEAD", "--tree"],
    ] {
        let out = gate_command(dir.path(), "status", &args);
        assert_eq!(out.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&out.stderr).contains("cannot be used with"));
        assert!(!dir.path().join(".loom").exists());
        assert!(!dir.path().join("invocations.log").exists());
    }
}

fn assert_target_executes_only_selected(subcommand: &str, target: &str) {
    let dir = verifier_workspace();
    let workspace = dir.path();
    for expected in [format!("{target}\n"), format!("{target}\n{target}\n")] {
        let out = gate_command(workspace, subcommand, &["--target", target]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("invocations.log")).unwrap(),
            expected
        );
    }
}

/// Exact verify targets must dispatch freshly, not return help or cached evidence.
#[test]
fn gate_verify_target_executes_selected_verifier_on_every_request() {
    assert_target_executes_only_selected("verify", "check-selected");
}

/// System target support must remain executable and exclude sibling scenarios.
#[test]
fn gate_system_target_executes_selected_verifier_on_every_request() {
    assert_target_executes_only_selected("system", "system-selected");
}

/// Shared system targets execute once per request, with no reuse across requests.
#[test]
fn gate_system_shared_target_executes_once_per_request() {
    let dir = verifier_workspace();
    let workspace = dir.path();
    std::fs::write(
        workspace.join("specs/fixture.md"),
        "## Success Criteria\n\n\
         - first owner [system](system-selected)\n\
         - second owner [system](system-selected)\n",
    )
    .unwrap();
    for expected_count in [1, 2] {
        let out = gate_command(workspace, "system", &["--target", "system-selected"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let invocations = std::fs::read_to_string(workspace.join("invocations.log")).unwrap();
        assert_eq!(
            invocations.lines().collect::<Vec<_>>(),
            vec!["system-selected"; expected_count],
            "shared targets must execute once on every fresh request"
        );
    }
}

/// Every target-capable command must parse the flag before exact-match validation.
#[test]
fn gate_target_commands_accept_target_and_reject_unknown_selection() {
    let dir = verifier_workspace();
    for subcommand in ["verify", "check", "test", "system", "judge"] {
        let out = gate_command(dir.path(), subcommand, &["--target", "missing-target"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "gate {subcommand} must reject zero matches"
        );
        assert!(
            stderr.contains("exactly matched `missing-target`"),
            "gate {subcommand}: {stderr}"
        );
        assert!(!dir.path().join("invocations.log").exists());
    }
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

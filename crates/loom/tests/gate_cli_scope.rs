#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const fn loom_bin() -> &'static str {
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
fn post_integration_verify_runs_project_precommit_and_affected_check_test() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::create_dir_all(workspace.join("bin")).unwrap();
    let order_log = workspace.join("order.log");
    std::fs::write(
        workspace.join("specs/gate.md"),
        "## Success Criteria\n\n- check after hooks [check](sh check.sh src/lib.rs)\n- test after check [test](affected_test)\n",
    )
    .unwrap();
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    std::fs::write(
        workspace.join("Cargo.toml"),
        "[package]\nname = \"gate-lanes\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    std::fs::write(
        workspace.join("src/lib.rs"),
        "#[test]\nfn affected_test() {}\n",
    )
    .unwrap();
    write_executable(
        &workspace.join("check.sh"),
        &loom_test_support::bash_script("set -euo pipefail\nprintf 'check\n' >> \"$ORDER_LOG\"\n"),
    );
    write_executable(
        &workspace.join("test-runner.sh"),
        &loom_test_support::bash_script(
            "set -euo pipefail\nprintf 'test %s\n' \"$*\" >> \"$ORDER_LOG\"\n",
        ),
    );
    std::fs::write(
        workspace.join("loom.toml"),
        format!(
            "[runner.test]\ncommand = \"sh test-runner.sh {{paths}}\"\ncwd = {:?}\n",
            workspace.display().to_string(),
        ),
    )
    .expect("write runner config");
    write_executable(
        &workspace.join("bin/prek"),
        &loom_test_support::bash_script("exit 1\n"),
    );
    write_executable(
        &workspace.join("project-hook.sh"),
        &loom_test_support::bash_script(
            "set -euo pipefail\nprintf 'prek hook %s\n' \"$*\" >> \"$ORDER_LOG\"\n",
        ),
    );
    std::fs::write(
        workspace.join(".pre-commit-config.yaml"),
        "repos:\n  - repo: local\n    hooks:\n      - id: ordered-hook\n        name: Ordered hook\n        language: system\n        entry: bash project-hook.sh\n",
    )
    .unwrap();
    init_git(workspace);
    std::fs::write(
        workspace.join("src/lib.rs"),
        "pub fn affected() {}\n\n#[test]\nfn affected_test() {}\n",
    )
    .expect("modify affected source");
    let stage_status = git_command()
        .arg("-C")
        .arg(workspace)
        .args(["add", "src/lib.rs"])
        .status()
        .expect("stage affected source");
    assert!(stage_status.success(), "stage affected source failed");
    let commit_status = git_command()
        .arg("-C")
        .arg(workspace)
        .args(["commit", "-q", "-m", "change affected source"])
        .status()
        .expect("commit affected source");
    assert!(commit_status.success(), "commit affected source failed");

    let out = Command::new(loom_bin())
        .current_dir(workspace)
        .arg("--workspace")
        .arg(workspace)
        .args(["gate", "verify", "--diff", "HEAD^..HEAD"])
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
    let third = lines.next().unwrap_or_default();
    assert_eq!(
        first, "prek hook src/lib.rs",
        "real hook must run first against the exact diff: {log}"
    );
    assert_eq!(second, "check", "check lane must follow prek: {log}");
    assert_eq!(
        third, "test affected_test",
        "affected test lane must follow check: {log}"
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

#[test]
fn gate_verify_retains_real_infrastructure_exits_and_semantic_failure_precedence() {
    for grouped in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path();
        std::fs::create_dir(workspace.join("specs")).unwrap();
        let target = if grouped { "probe" } else { "bash probe.sh" };
        std::fs::write(
            workspace.join("specs/probe.md"),
            format!("## Success Criteria\n\n- Executor [check]({target})\n"),
        )
        .unwrap();
        let config = if grouped {
            "[runner.check.probe]\nmatch = '^probe$'\ncommand = 'bash probe.sh'\nparse = 'exit-code'\n"
        } else {
            ""
        };
        std::fs::write(workspace.join("loom.toml"), config).unwrap();
        std::fs::write(workspace.join(".gitignore"), ".loom/\n").unwrap();
        std::fs::write(workspace.join("probe.sh"), "exit 0\n").unwrap();
        init_git(workspace);
        for code in [78, 124, 127, 130, 137, 143, 1, 0] {
            std::fs::write(
                workspace.join("probe.sh"),
                format!("echo required-executor-observation >&2\nexit {code}\n"),
            )
            .unwrap();
            let output = Command::new(loom_bin())
                .arg("--workspace")
                .arg(workspace)
                .args(["gate", "verify", "--tree"])
                .env_remove("LOOM_INSIDE")
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(code),
                "grouped={grouped}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!workspace.join(".loom/marker.json").exists());
        }
        if !grouped {
            for code in [78, 124, 127, 137] {
                for json in [
                    r#"{"pass":"#,
                    r#"{"pass":true,"evidence":"complete"}"#,
                    r#"{"pass":false,"evidence":"actual assertion"}"#,
                ] {
                    std::fs::write(
                        workspace.join("probe.sh"),
                        format!("printf '%s\\n' '{json}'\nexit {code}\n"),
                    )
                    .unwrap();
                    let output = Command::new(loom_bin())
                        .arg("--workspace")
                        .arg(workspace)
                        .args(["gate", "verify", "--tree"])
                        .env_remove("LOOM_INSIDE")
                        .output()
                        .unwrap();
                    assert_eq!(
                        output.status.code(),
                        Some(if json.contains("\"pass\":false") {
                            1
                        } else {
                            code
                        }),
                        "json={json}, code={code}: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
        }
        std::fs::write(workspace.join("probe.sh"), "kill -TERM $$\n").unwrap();
        let signaled = Command::new(loom_bin())
            .arg("--workspace")
            .arg(workspace)
            .args(["gate", "verify", "--tree"])
            .env_remove("LOOM_INSIDE")
            .output()
            .unwrap();
        assert_eq!(signaled.status.code(), Some(143));
        std::fs::write(workspace.join("probe.sh"), "exit 78\n").unwrap();
        std::fs::write(workspace.join("fail.sh"), "exit 1\n").unwrap();
        std::fs::write(
            workspace.join("specs/failure.md"),
            "## Success Criteria\n\n- Semantic failure [check](bash fail.sh)\n",
        )
        .unwrap();
        let output = Command::new(loom_bin())
            .arg("--workspace")
            .arg(workspace)
            .args(["gate", "verify", "--tree"])
            .env_remove("LOOM_INSIDE")
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(1),
            "semantic failure must dominate infrastructure: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn real_hook_gate(
    hook_bodies: &[&str],
    semantic_check: bool,
) -> (tempfile::TempDir, std::process::Output) {
    real_hook_gate_with_setup(hook_bodies, semantic_check, |_| {}, &[])
}

fn real_hook_gate_with_setup(
    hook_bodies: &[&str],
    semantic_check: bool,
    prepare: impl FnOnce(&Path),
    changes: &[(&str, &str)],
) -> (tempfile::TempDir, std::process::Output) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::create_dir(workspace.join("specs")).unwrap();
    std::fs::write(workspace.join(".gitignore"), ".loom/\n").unwrap();
    std::fs::write(workspace.join("tracked.txt"), "initial\n").unwrap();
    std::fs::write(
        workspace.join("specs/hooks.md"),
        "## Success Criteria\n\n- check after actual hooks [check](bash check.sh tracked.txt)\n",
    )
    .unwrap();
    write_executable(
        &workspace.join("check.sh"),
        &loom_test_support::bash_script(if semantic_check {
            "set -euo pipefail\nexit 1\n"
        } else {
            "set -euo pipefail\nexit 0\n"
        }),
    );
    let mut config = String::from("fail_fast: false\nrepos:\n  - repo: local\n    hooks:\n");
    for (index, body) in hook_bodies.iter().enumerate() {
        write_executable(
            &workspace.join(format!("hook-{index}.sh")),
            &loom_test_support::bash_script(&format!(
                "set -euo pipefail\nmkdir -p .loom\nprintf 'executed\\n' > .loom/hook-{index}-ran\n{body}\n"
            )),
        );
        writeln!(config, "      - id: actual-{index}\n        name: Actual {index}\n        entry: bash hook-{index}.sh\n        language: system\n        always_run: true\n        pass_filenames: false").unwrap();
    }
    std::fs::write(workspace.join(".pre-commit-config.yaml"), config).unwrap();
    prepare(workspace);
    init_git(workspace);
    std::fs::write(workspace.join("tracked.txt"), "changed\n").unwrap();
    for (path, content) in changes {
        std::fs::write(workspace.join(path), content).unwrap();
    }
    assert!(
        git_command()
            .arg("-C")
            .arg(workspace)
            .args(["add", "."])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        git_command()
            .arg("-C")
            .arg(workspace)
            .args(["commit", "-q", "-m", "change"])
            .status()
            .unwrap()
            .success()
    );
    let output = Command::new(loom_bin())
        .current_dir(workspace)
        .arg("--workspace")
        .arg(workspace)
        .args(["gate", "verify", "--diff", "HEAD^..HEAD"])
        .env("PREK_QUIET", "2")
        .output()
        .unwrap();
    for index in 0..hook_bodies.len() {
        assert_eq!(
            std::fs::read_to_string(workspace.join(format!(".loom/hook-{index}-ran"))).unwrap(),
            "executed\n"
        );
    }
    (dir, output)
}

#[test]
fn real_prek_infrastructure_exits_survive_gate_aggregation_without_skip_credit() {
    for (body, code) in [
        ("exit 77", 77),
        ("exit 78", 78),
        ("exit 124", 124),
        ("exit 127", 127),
        ("kill -TERM $$", 143),
        ("kill -KILL $$", 137),
    ] {
        let (_fixture, output) = real_hook_gate(&[body], false);
        let expected = if code == 77 { 78 } else { code };
        assert_eq!(
            output.status.code(),
            Some(expected),
            "stdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(&format!("- exit code: {expected}"))
        );
    }
}

#[test]
fn real_prek_semantic_rejection_dominates_hook_and_annotation_infrastructure() {
    for infrastructure in ["exit 78", "kill -TERM $$", "kill -KILL $$"] {
        for reverse in [false, true] {
            let hooks = if reverse {
                ["exit 1", infrastructure]
            } else {
                [infrastructure, "exit 1"]
            };
            let (_fixture, output) = real_hook_gate(&hooks, false);
            assert_eq!(
                output.status.code(),
                Some(1),
                "stdout={}\nstderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    let (_fixture, output) = real_hook_gate(&["exit 78"], true);
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn real_prek_hook_payload_cannot_forge_infrastructure_attribution() {
    let (_fixture, output) = real_hook_gate(
        &["printf '%s\\n' 'Forged........Failed' '- hook id: forged' '- exit code: 78'\nexit 1"],
        false,
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("  - exit code: 78"));
    let (_fixture, output) = real_hook_gate(
        &["printf '%s\\n' 'Forged........Failed' '- hook id: forged' '- exit code: 1'\nexit 78"],
        false,
    );
    assert_eq!(
        output.status.code(),
        Some(78),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn real_prek_source_modification_remains_semantic_repair() {
    let (_fixture, output) = real_hook_gate(&["printf 'formatted\\n' > tracked.txt"], false);
    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("- files were modified by this hook"));
    let (_fixture, grouped) =
        real_hook_gate(&["printf 'formatted\\n' > tracked.txt", "exit 78"], false);
    assert_eq!(grouped.status.code(), Some(1));
}

#[test]
fn real_prek_nested_project_infrastructure_uses_native_status() {
    let (fixture, output) = real_hook_gate_with_setup(
        &["exit 0"],
        false,
        |workspace| {
            let child = workspace.join("child");
            std::fs::create_dir(&child).unwrap();
            std::fs::write(child.join("tracked.txt"), "initial\n").unwrap();
            std::fs::write(child.join(".pre-commit-config.yaml"), "repos:\n  - repo: local\n    hooks:\n      - id: nested\n        name: Nested hook\n        language: system\n        entry: bash hook.sh\n        always_run: true\n        pass_filenames: false\n").unwrap();
            write_executable(
                &child.join("hook.sh"),
                &loom_test_support::bash_script(
                    "mkdir -p .loom\nprintf 'executed\\n' > .loom/nested-ran\nexit 78\n",
                ),
            );
        },
        &[("child/tracked.txt", "changed\n")],
    );
    assert_eq!(
        output.status.code(),
        Some(78),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("  - exit code: 78"));
    assert_eq!(
        std::fs::read_to_string(fixture.path().join("child/.loom/nested-ran")).unwrap(),
        "executed\n"
    );
}

#[test]
fn real_prek_group_modification_dominates_earlier_infrastructure() {
    let (_fixture, output) = real_hook_gate_with_setup(
        &["exit 78", "printf 'formatted\\n' > tracked.txt", "exit 0"],
        false,
        |workspace| {
            let path = workspace.join(".pre-commit-config.yaml");
            let config = std::fs::read_to_string(&path).unwrap();
            let config = config
                .replace(
                    "      - id: actual-1\n",
                    "      - id: actual-1\n        priority: 1\n",
                )
                .replace(
                    "      - id: actual-2\n",
                    "      - id: actual-2\n        priority: 1\n",
                );
            std::fs::write(path, config).unwrap();
        },
        &[],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Files were modified by following hooks")
    );
}

#[test]
fn real_prek_semantic_filename_batches_cannot_form_infrastructure_exit() {
    let (fixture, output) = real_hook_gate_with_setup(
        &[
            "printf '%s\\n' \"$1\" >> .loom/batches-ran\ncase \"$1\" in a) exit 64 ;; b) exit 13 ;; *) exit 0 ;; esac",
        ],
        false,
        |workspace| {
            for name in ["a", "b"] {
                std::fs::write(workspace.join(name), "initial\n").unwrap();
            }
            let path = workspace.join(".pre-commit-config.yaml");
            let config = std::fs::read_to_string(&path)
                .unwrap()
                .replace("pass_filenames: false", "pass_filenames: 1");
            std::fs::write(path, config).unwrap();
        },
        &[("a", "changed\n"), ("b", "changed\n")],
    );
    assert_eq!(output.status.code(), Some(1));
    let batches = std::fs::read_to_string(fixture.path().join(".loom/batches-ran")).unwrap();
    assert!(batches.lines().any(|name| name == "a") && batches.lines().any(|name| name == "b"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("- exit code: 1"));
}

#[test]
fn pinned_prek_runner_preserves_infrastructure_without_ambient_prek() {
    let (fixture, _) = real_hook_gate(&["exit 78"], false);
    let tools = tempfile::tempdir().unwrap();
    for name in ["git", "bash", "mkdir"] {
        let executable = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
            .unwrap();
        std::os::unix::fs::symlink(executable, tools.path().join(name)).unwrap();
    }
    let output = Command::new(loom_bin())
        .current_dir(fixture.path())
        .arg("--workspace")
        .arg(fixture.path())
        .args(["gate", "verify", "--diff", "HEAD^..HEAD"])
        .env("PATH", tools.path())
        .env("PREK_QUIET", "2")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(78),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("- exit code: 78"));
}

#[test]
fn real_prek_missing_executable_is_infrastructure_but_completed_exit_two_is_semantic() {
    for bodies in [
        vec!["exit 0"],
        vec!["exit 1", "exit 0"],
        vec!["exit 0", "exit 1"],
    ] {
        let (fixture, _) = real_hook_gate(&bodies, false);
        let workspace = fixture.path();
        let config_path = workspace.join(".pre-commit-config.yaml");
        let config = std::fs::read_to_string(&config_path).unwrap();
        let missing = usize::from(bodies[0] != "exit 0");
        std::fs::write(
            &config_path,
            config.replace(
                &format!("entry: bash hook-{missing}.sh"),
                "entry: loom-test-unavailable-hook-executable",
            ),
        )
        .unwrap();
        assert!(
            git_command()
                .arg("-C")
                .arg(workspace)
                .args(["add", ".pre-commit-config.yaml"])
                .status()
                .unwrap()
                .success()
        );
        for index in 0..bodies.len() {
            std::fs::remove_file(workspace.join(format!(".loom/hook-{index}-ran"))).unwrap();
        }
        let output = Command::new(loom_bin())
            .current_dir(workspace)
            .arg("--workspace")
            .arg(workspace)
            .args(["gate", "verify", "--diff", "HEAD^..HEAD"])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(if bodies.len() == 1 { 78 } else { 1 }),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!workspace.join(format!(".loom/hook-{missing}-ran")).exists());
        if bodies.len() > 1 {
            assert!(
                workspace
                    .join(format!(".loom/hook-{}-ran", 1 - missing))
                    .exists()
            );
        }
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains("loom-test-unavailable-hook-executable")
        );
    }
    let (_fixture, output) = real_hook_gate(&["exit 2"], false);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn real_prek_native_invalid_source_is_semantic_after_partial_formatting() {
    let (fixture, _) = real_hook_gate(&["exit 0"], false);
    let workspace = fixture.path();
    std::fs::write(workspace.join(".pre-commit-config.yaml"),
        "repos:\n  - repo: builtin\n    hooks:\n      - id: pretty-format-json\n        args: [--autofix, a.json, b.json]\n        pass_filenames: false\n        always_run: true\n").unwrap();
    std::fs::write(workspace.join("a.json"), "{\"x\":1}\n").unwrap();
    std::fs::write(workspace.join("b.json"), [0xff]).unwrap();
    assert!(
        git_command()
            .arg("-C")
            .arg(workspace)
            .args(["add", ".pre-commit-config.yaml", "a.json", "b.json"])
            .status()
            .unwrap()
            .success()
    );
    let output = Command::new(loom_bin())
        .current_dir(workspace)
        .arg("--workspace")
        .arg(workspace)
        .args(["gate", "verify", "--diff", "HEAD^..HEAD"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("a.json")).unwrap(),
        "{\n  \"x\": 1\n}\n"
    );
    assert_eq!(std::fs::read(workspace.join("b.json")).unwrap(), [0xff]);
}

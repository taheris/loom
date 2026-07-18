#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

struct Fixture {
    _root: TempDir,
    workspace: PathBuf,
    tools: PathBuf,
    log: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let workspace = root.path().join("workspace");
        let tools = root.path().join("tools");
        let log = root.path().join("pre-push.log");
        std::fs::create_dir_all(workspace.join("bin")).expect("create workspace bin");
        std::fs::create_dir_all(&tools).expect("create tools");

        let source_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        std::fs::copy(
            source_root.join(".pre-commit-config.yaml"),
            workspace.join(".pre-commit-config.yaml"),
        )
        .expect("copy pre-commit config");
        std::fs::copy(
            source_root.join("bin/pre-push-checks"),
            workspace.join("bin/pre-push-checks"),
        )
        .expect("copy pre-push wrapper");

        install_recording_command(&tools, "cargo");
        install_recording_command(&tools, "loom");

        git(&workspace, &["init", "-q", "-b", "main"]);
        git(&workspace, &["config", "user.email", "test@example.com"]);
        git(&workspace, &["config", "user.name", "Test"]);
        git(&workspace, &["config", "commit.gpgsign", "false"]);
        std::fs::write(workspace.join("README.md"), "fixture\n").expect("write seed");
        git(&workspace, &["add", "."]);
        git(&workspace, &["commit", "-q", "-m", "Seed fixture"]);

        Self {
            _root: root,
            workspace,
            tools,
            log,
        }
    }

    fn head(&self) -> String {
        git_stdout(&self.workspace, &["rev-parse", "HEAD"])
    }

    fn commit(&self, path: &str, body: &str, message: &str) -> String {
        let path = self.workspace.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create file parent");
        }
        std::fs::write(&path, body).expect("write committed file");
        let relative = path
            .strip_prefix(&self.workspace)
            .expect("workspace-relative path");
        let output = git_command()
            .arg("-C")
            .arg(&self.workspace)
            .arg("add")
            .arg(relative)
            .output()
            .expect("spawn git add");
        assert_success(&output, "git add");
        git(&self.workspace, &["commit", "-q", "-m", message]);
        self.head()
    }

    fn run_hooks(&self, hooks: &[&str], from_ref: &str, to_ref: &str) -> Vec<String> {
        std::fs::write(&self.log, "").expect("clear invocation log");
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut entries = vec![self.tools.clone()];
        entries.extend(std::env::split_paths(&path));
        let path = std::env::join_paths(entries).expect("join PATH");

        let mut command = Command::new("prek");
        loom_test_support::scrub_git_local_env(&mut command);
        let output = command
            .current_dir(&self.workspace)
            .args(["run"])
            .args(hooks)
            .args([
                "--stage",
                "pre-push",
                "--from-ref",
                from_ref,
                "--to-ref",
                to_ref,
            ])
            .env("PATH", path)
            .env("PRE_PUSH_TEST_LOG", &self.log)
            .env_remove("LOOM_VERIFY_TIERS")
            .output()
            .expect("spawn prek");
        assert_success(&output, "prek pre-push");

        std::fs::read_to_string(&self.log)
            .expect("read invocation log")
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

fn install_recording_command(tools: &Path, name: &str) {
    let body = format!(
        "set -euo pipefail\n\
         {{\n\
             printf '{name}'\n\
             for arg in \"$@\"; do\n\
                 printf '\\t%s' \"$arg\"\n\
             done\n\
             if [[ -v LOOM_VERIFY_TIERS ]]; then\n\
                 printf '\\ttiers=%s\\n' \"$LOOM_VERIFY_TIERS\"\n\
             else\n\
                 printf '\\ttiers=<unset>\\n'\n\
             fi\n\
         }} >> \"$PRE_PUSH_TEST_LOG\"\n",
    );
    loom_test_support::write_executable_bash_script(tools.join(name), &body)
        .expect("write recording command");
}

fn git(workspace: &Path, args: &[&str]) {
    let output = git_command()
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
        .expect("spawn git");
    assert_success(&output, &format!("git {}", args.join(" ")));
}

fn git_stdout(workspace: &Path, args: &[&str]) -> String {
    let output = git_command()
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
        .expect("spawn git");
    assert_success(&output, &format!("git {}", args.join(" ")));
    String::from_utf8(output.stdout)
        .expect("git stdout is UTF-8")
        .trim()
        .to_owned()
}

fn git_command() -> Command {
    let mut command = Command::new("git");
    loom_test_support::scrub_git_local_env(&mut command);
    command
}

fn assert_success(output: &Output, operation: &str) {
    assert!(
        output.status.success(),
        "{operation} failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn pre_push_config_runs_clippy_and_verify_diff_without_loom_verify_tiers() {
    let fixture = Fixture::new();
    let text_base = fixture.head();
    let text_head = fixture.commit("notes.txt", "text only\n", "Add text fixture");

    let text_lines = fixture.run_hooks(
        &["cargo-clippy", "loom-gate-verify-diff"],
        &text_base,
        &text_head,
    );
    assert_eq!(
        text_lines,
        vec![format!(
            "loom\tgate\tverify\t--diff\t{text_base}..{text_head}\ttiers=<unset>"
        )],
        "a non-Rust push must skip clippy while the always-run gate hook stays active",
    );

    let rust_base = text_head;
    let rust_head = fixture.commit("src/lib.rs", "pub fn live() {}\n", "Add Rust fixture");
    let rust_lines = fixture.run_hooks(
        &["cargo-clippy", "loom-gate-verify-diff"],
        &rust_base,
        &rust_head,
    );
    assert_eq!(rust_lines.len(), 2, "Rust pushes must run both hooks");
    assert_eq!(
        rust_lines[0],
        "cargo\tclippy\t--workspace\t--all-targets\t--\t-D\twarnings\ttiers=<unset>",
    );
    assert_eq!(
        rust_lines[1],
        format!("loom\tgate\tverify\t--diff\t{rust_base}..{rust_head}\ttiers=<unset>"),
    );
}

#[test]
fn pre_push_config_runs_verify_diff_for_pushed_range() {
    let fixture = Fixture::new();
    let pushed_base = fixture.head();

    git(&fixture.workspace, &["checkout", "-q", "-b", "stale"]);
    let stale_tip = fixture.commit("stale.txt", "not pushed\n", "Add stale fixture");
    git(&fixture.workspace, &["checkout", "-q", "main"]);
    let pushed_head = fixture.commit("pushed.txt", "pushed\n", "Add pushed fixture");
    git(
        &fixture.workspace,
        &["branch", "--set-upstream-to", "stale", "main"],
    );
    assert_eq!(
        git_stdout(&fixture.workspace, &["rev-parse", "@{u}"]),
        stale_tip
    );
    assert_ne!(
        stale_tip, pushed_base,
        "upstream must not be the pushed base"
    );

    let lines = fixture.run_hooks(&["loom-gate-verify-diff"], &pushed_base, &pushed_head);
    assert_eq!(
        lines,
        vec![format!(
            "loom\tgate\tverify\t--diff\t{pushed_base}..{pushed_head}\ttiers=<unset>"
        )],
        "the gate hook must use prek's pushed endpoints, not the branch upstream",
    );
}

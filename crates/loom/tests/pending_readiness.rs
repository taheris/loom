use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

struct Fixture {
    root: TempDir,
    tools: PathBuf,
}

#[expect(
    clippy::expect_used,
    reason = "fixture setup failures must abort the test"
)]
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("fixture root");
        let tools = root.path().join("tools");
        std::fs::create_dir(&tools).expect("tools directory");
        std::fs::create_dir(root.path().join("specs")).expect("specs directory");
        std::fs::write(
            root.path().join("specs/readiness.md"),
            "## Success Criteria\n\n\
             - Existing check [check](true)\n\n\
             <!-- prettier-ignore -->\n\
             - Prospective check [check?](sh -c 'printf probed > check-probed; exit \"$PENDING_CHECK_EXIT\"')\n\n\
             <!-- prettier-ignore -->\n\
             - Prospective system [system?](nix run .#test-quint -- cache-model)\n",
        )
        .expect("write spec");
        loom_test_support::write_executable_bash_script(
            tools.join("nix"),
            "set -euo pipefail\nprintf '%s\\n' \"$*\" >> system-probed\nexit \"$PENDING_SYSTEM_EXIT\"\n",
        )
        .expect("recording Nix dependency");
        Self { root, tools }
    }

    fn workspace(&self) -> &Path {
        self.root.path()
    }

    fn command(&self, executable: &str) -> Command {
        let mut command = Command::new(executable);
        let mut paths = vec![self.tools.clone()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").expect("PATH"),
        ));
        loom_test_support::scrub_git_local_env(&mut command);
        loom_test_support::configure_hermetic_git(&mut command);
        command
            .current_dir(self.workspace())
            .env("PATH", std::env::join_paths(paths).expect("fixture PATH"))
            .env("PREK_HOME", self.workspace().join("prek-cache"))
            .env_remove("LOOM_INSIDE")
            .env_remove("LOOM_PARENT_DIFF_GATE")
            .env("PENDING_CHECK_EXIT", "1")
            .env("PENDING_SYSTEM_EXIT", "1");
        command
    }

    fn gate(&self, args: &[&str], system_exit: &str) -> Output {
        self.command(env!("CARGO_BIN_EXE_loom"))
            .arg("gate")
            .args(args)
            .env("PENDING_SYSTEM_EXIT", system_exit)
            .output()
            .expect("run real gate")
    }

    fn git(&self, args: &[&str]) {
        assert_success(&self.command("git").args(args).output().expect("git"));
    }

    fn prepare_commit(&self) {
        self.git(&["init", "-q", "-b", "main"]);
        self.git(&["config", "user.name", "Readiness Test"]);
        self.git(&["config", "user.email", "readiness@example.invalid"]);
        self.git(&["config", "commit.gpgsign", "false"]);
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        std::fs::write(self.workspace().join("flake.nix"), "{ }\n")
            .expect("formatter workspace anchor");
        std::fs::copy(
            source.join(".pre-commit-config.yaml"),
            self.workspace().join(".pre-commit-config.yaml"),
        )
        .expect("canonical hook configuration");
        std::fs::create_dir(self.workspace().join("scripts")).expect("scripts directory");
        std::fs::copy(
            source.join("scripts/check-shell-reexec"),
            self.workspace().join("scripts/check-shell-reexec"),
        )
        .expect("shell check");
        loom_test_support::write_executable_bash_script(
            self.tools.join("loom"),
            &format!(
                "set -euo pipefail\nexec '{}' \"$@\"\n",
                env!("CARGO_BIN_EXE_loom")
            ),
        )
        .expect("real Loom entrypoint");
        loom_test_support::write_executable_bash_script(
            self.workspace().join(".git/hooks/pre-commit"),
            "set -euo pipefail\nexec prek hook-impl --hook-type=pre-commit --hook-dir .git/hooks --script-version 4 -- \"$@\"\n",
        )
        .expect("Git to real prek bridge");
        self.git(&["add", "specs/readiness.md"]);
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn finite_verify_does_not_execute_system_readiness_outside_requested_lanes() {
    let fixture = Fixture::new();
    assert_success(&fixture.gate(&["verify", "--files", "specs/readiness.md"], "1"));
    assert!(fixture.workspace().join("check-probed").is_file());
    assert!(!fixture.workspace().join("system-probed").exists());
}

#[test]
fn check_tree_does_not_execute_system_readiness() {
    let fixture = Fixture::new();
    assert_success(&fixture.gate(&["check", "--tree"], "1"));
    assert!(fixture.workspace().join("check-probed").is_file());
    assert!(!fixture.workspace().join("system-probed").exists());
}

#[test]
fn tree_verify_executes_requested_system_readiness() {
    let fixture = Fixture::new();
    assert_success(&fixture.gate(&["verify", "--tree"], "1"));
    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("system-probed")).expect("system probe"),
        "run .#test-quint -- cache-model\n",
    );
}

#[test]
fn successful_requested_system_probe_still_flags_stale_pending_marker() {
    let fixture = Fixture::new();
    let output = fixture.gate(&["verify", "--tree"], "0");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("drop the ? marker"), "{stderr}");
    assert!(
        stderr.contains("nix run .#test-quint -- cache-model"),
        "{stderr}"
    );
    assert!(fixture.workspace().join("system-probed").is_file());
}

#[test]
fn ordinary_commit_runs_hooks_without_provisioning_pending_system_checks() {
    let fixture = Fixture::new();
    fixture.prepare_commit();
    let output = fixture
        .command("git")
        .args(["commit", "-m", "Exercise worker feedback"])
        .output()
        .expect("ordinary commit");
    assert_success(&output);
    assert!(fixture.workspace().join("check-probed").is_file());
    assert!(!fixture.workspace().join("system-probed").exists());
}

#[test]
fn ordinary_commit_still_blocks_on_successful_pending_check() {
    let fixture = Fixture::new();
    fixture.prepare_commit();
    let output = fixture
        .command("git")
        .args(["commit", "-m", "Must reject stale pending check"])
        .env("PENDING_CHECK_EXIT", "0")
        .output()
        .expect("ordinary commit");
    assert!(!output.status.success());
    let diagnostics = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(diagnostics.contains("drop the ? marker"), "{diagnostics}");
    assert!(fixture.workspace().join("check-probed").is_file());
    assert!(!fixture.workspace().join("system-probed").exists());
}

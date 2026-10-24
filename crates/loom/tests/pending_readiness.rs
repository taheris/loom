//! Pending owners do not waive independent baselines, and readiness stays lane scoped.
#![allow(
    clippy::unwrap_used,
    reason = "isolated native/hook fixture setup and assertions"
)]

use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "common/reported.rs"]
mod reported;

struct Fixture {
    check: reported::Fixture,
    tools: tempfile::TempDir,
}

fn copy(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() == ".git" {
            continue;
        }
        if entry.file_type().unwrap().is_dir() {
            copy(&entry.path(), &destination.join(entry.file_name()));
        } else {
            std::fs::copy(entry.path(), destination.join(entry.file_name())).unwrap();
        }
    }
}

impl Fixture {
    fn new() -> Self {
        let check = reported::Fixture::new("check", "json-lines", &["check-probe"]);
        check.policy("check", false);
        check.record("check-probe", &format!("{}\n", json!({"target":"check-probe","pass":false,"evidence":"pending check not implemented"})), 1);
        let system = reported::Fixture::new("system", "json-lines", &["system-probe"]);
        system.record("system-probe", &format!("{}\n", json!({"target":"system-probe","pass":false,"evidence":"pending system not implemented"})), 1);
        copy(system.path(), &check.path().join("system"));
        std::fs::remove_file(check.path().join("system/loom.toml")).unwrap();
        for relative in ["specs/fixture", "system/specs/fixture"] {
            std::fs::remove_dir_all(check.path().join(relative)).unwrap();
        }
        std::fs::write(
            check.path().join("docs/README.md"),
            "- [Readiness](../specs/readiness.md)\n",
        )
        .unwrap();
        std::fs::write(
            check.path().join("system/docs/README.md"),
            "- [Readiness](../../specs/readiness.md)\n",
        )
        .unwrap();
        let fixture = Self {
            check,
            tools: tempfile::tempdir().unwrap(),
        };
        std::fs::write(fixture.workspace().join("specs/readiness.md"), format!("## Success Criteria\n- Existing context [check](loom:project-context)\n- Prospective check [check?]({})\n- Prospective system [system?]({})\n", fixture.check.address("check-probe"), fixture.system_address())).unwrap();
        fixture
    }
    fn workspace(&self) -> &Path {
        self.check.path()
    }
    fn system_address(&self) -> String {
        self.check
            .address("system-probe")
            .replacen(" .#", " ./system#", 1)
    }
    fn command(&self, executable: &str) -> Command {
        let mut command = Command::new(executable);
        let paths = std::iter::once(self.tools.path().to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap()))
            .collect::<Vec<PathBuf>>();
        loom_test_support::scrub_git_local_env(&mut command);
        loom_test_support::configure_hermetic_git(&mut command);
        command
            .current_dir(self.workspace())
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("PREK_HOME", self.tools.path().join("prek-cache"))
            .env_remove("LOOM_INSIDE")
            .env_remove("LOOM_PARENT_DIFF_GATE");
        command
    }
    fn gate(&self, args: &[&str], system_pass: bool) -> Output {
        let value = json!({"target":"system-probe","pass":system_pass,"evidence":"actual requested system predicate"});
        std::fs::write(
            self.workspace().join("system/records/system-probe.output"),
            format!("{value}\n"),
        )
        .unwrap();
        std::fs::write(
            self.workspace().join("system/records/system-probe.status"),
            if system_pass { "0" } else { "1" },
        )
        .unwrap();
        if args == ["check", "--tree"] {
            self.check
                .run(Path::new(env!("CARGO_BIN_EXE_loom")), "check", false)
        } else {
            self.command(env!("CARGO_BIN_EXE_loom"))
                .arg("gate")
                .args(args)
                .output()
                .unwrap()
        }
    }
    fn prepare_commit(&self) {
        let configuration = format!(
            "repos:\n  - repo: local\n    hooks:\n      - id: checked-readiness\n        name: Native check readiness\n        entry: {} gate check --tree\n        language: system\n        pass_filenames: false\n        always_run: true\n",
            env!("CARGO_BIN_EXE_loom")
        );
        std::fs::write(
            self.workspace().join(".pre-commit-config.yaml"),
            configuration,
        )
        .unwrap();
        let prek = env!("LOOM_PREK_BIN");
        loom_test_support::write_executable_bash_script(self.workspace().join(".git/hooks/pre-commit"), &format!("set -euo pipefail\nexec '{prek}' hook-impl --hook-type=pre-commit --hook-dir .git/hooks --script-version 4 -- \"$@\"\n")).unwrap();
        for args in [
            vec!["config", "user.name", "Readiness Test"],
            vec!["config", "user.email", "readiness@fixture.invalid"],
            vec!["config", "commit.gpgsign", "false"],
            vec!["add", "--all"],
        ] {
            assert!(self.command("git").args(args).status().unwrap().success());
        }
    }
}

fn feedback(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
fn system_was_run(fixture: &Fixture, output: &Output) -> bool {
    feedback(output).contains(&format!("loom gate [{}]:", fixture.system_address()))
}

#[test]
fn finite_verify_does_not_execute_system_readiness_outside_requested_lanes() {
    let fixture = Fixture::new();
    let output = fixture.gate(&["verify", "--files", "records/check-probe.output"], false);
    assert_eq!(
        output.status.code(),
        Some(1),
        "required failing baseline cannot be waived by a pending owner: {}",
        feedback(&output)
    );
    assert!(feedback(&output).contains("pending check not implemented"));
    assert!(!system_was_run(&fixture, &output));
}

#[test]
fn check_tree_does_not_execute_system_readiness() {
    let fixture = Fixture::new();
    let output = fixture.gate(&["check", "--tree"], false);
    assert_eq!(output.status.code(), Some(1), "{}", feedback(&output));
    assert!(feedback(&output).contains("pending check not implemented"));
    assert!(!system_was_run(&fixture, &output));
}

#[test]
fn tree_verify_executes_requested_system_readiness() {
    let fixture = Fixture::new();
    let output = fixture.gate(&["verify", "--tree"], false);
    assert_eq!(
        output.status.code(),
        Some(1),
        "pending does not waive failed native coverage"
    );
    assert!(system_was_run(&fixture, &output), "{}", feedback(&output));
}

#[test]
fn successful_requested_system_probe_still_flags_stale_pending_marker() {
    let fixture = Fixture::new();
    let output = fixture.gate(&["verify", "--tree"], true);
    assert!(!output.status.success());
    assert!(
        feedback(&output).contains("unneeded pending marker"),
        "{}",
        feedback(&output)
    );
    assert!(system_was_run(&fixture, &output));
}

#[test]
fn ordinary_commit_runs_hooks_without_provisioning_pending_system_checks() {
    let fixture = Fixture::new();
    fixture.prepare_commit();
    let output = fixture
        .command("git")
        .args(["commit", "-m", "Exercise checked worker feedback"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "required baseline failure must still block commit"
    );
    assert!(
        feedback(&output).contains("pending check not implemented"),
        "{}",
        feedback(&output)
    );
    assert!(!system_was_run(&fixture, &output));
}

#[test]
fn ordinary_commit_still_blocks_on_successful_pending_check() {
    let fixture = Fixture::new();
    fixture.check.record(
        "check-probe",
        &format!(
            "{}\n",
            json!({"target":"check-probe","pass":true,"evidence":"check implemented"})
        ),
        0,
    );
    fixture.prepare_commit();
    let output = fixture
        .command("git")
        .args(["commit", "-m", "Reject stale pending check"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        feedback(&output).contains("unneeded pending marker"),
        "{}",
        feedback(&output)
    );
    assert!(!system_was_run(&fixture, &output));
}

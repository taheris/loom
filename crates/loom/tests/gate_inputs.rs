//! Native input ownership comes from independently staged constructors, never input-query JSON.
#![allow(
    clippy::unwrap_used,
    reason = "isolated integration setup and result assertions"
)]

use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Output};

#[path = "common/reported.rs"]
mod reported;

struct Fixture {
    native: reported::Fixture,
    tier: &'static str,
}

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() == ".git" {
            continue;
        }
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &destination.join(entry.file_name()));
        } else {
            std::fs::copy(entry.path(), destination.join(entry.file_name())).unwrap();
        }
    }
}

impl Fixture {
    fn new() -> Self {
        Self::for_tier("check")
    }
    fn for_tier(tier: &'static str) -> Self {
        let native = reported::Fixture::new(tier, "json-lines", &["alpha"]);
        native.policy(tier, false);
        native.record(
            "alpha",
            &format!(
                "{}\n",
                json!({"target":"alpha","pass":true,"evidence":"alpha actual producer"})
            ),
            0,
        );
        let beta = reported::Fixture::new(tier, "json-lines", &["beta"]);
        beta.record(
            "beta",
            &format!(
                "{}\n",
                json!({"target":"beta","pass":true,"evidence":"beta actual producer"})
            ),
            0,
        );
        copy_tree(beta.path(), &native.path().join("beta"));
        // Only the workspace's root configuration is actually read by Gate.
        std::fs::remove_file(native.path().join("beta/loom.toml")).unwrap();
        std::fs::remove_dir_all(native.path().join("specs/fixture")).unwrap();
        std::fs::remove_dir_all(native.path().join("beta/specs/fixture")).unwrap();
        std::fs::write(
            native.path().join("beta/docs/README.md"),
            "- [inputs](../../specs/inputs.md)\n",
        )
        .unwrap();
        std::fs::write(
            native.path().join("docs/README.md"),
            "- [inputs](../specs/inputs.md)\n- [unrelated](unrelated.md)\n",
        )
        .unwrap();
        std::fs::write(
            native.path().join("docs/unrelated.md"),
            "# Unrelated documentation\n",
        )
        .unwrap();
        let fixture = Self { native, tier };
        std::fs::write(
            fixture.root().join("specs/inputs.md"),
            format!(
                "## Success Criteria\n- Alpha [{tier}]({})\n- Beta [{tier}]({})\n",
                fixture.address("alpha"),
                fixture.address("beta")
            ),
        )
        .unwrap();
        fixture.commit("native input fixture");
        fixture
    }
    fn root(&self) -> &Path {
        self.native.path()
    }
    fn address(&self, name: &str) -> String {
        if name == "alpha" {
            self.native.address(name)
        } else {
            self.native.address(name).replacen(" .#", " ./beta#", 1)
        }
    }
    fn commit(&self, message: &str) {
        for args in [vec!["add", "--all"], vec!["commit", "-qm", message]] {
            let mut command = Command::new("git");
            loom_test_support::scrub_git_local_env(&mut command);
            assert!(
                command
                    .current_dir(self.root())
                    .args([
                        "-c",
                        "user.name=Inputs fixture",
                        "-c",
                        "user.email=inputs@fixture.invalid",
                        "-c",
                        "commit.gpgsign=false",
                        "-c",
                        "core.hooksPath=/dev/null"
                    ])
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }
    fn run(&self, gate: &str, scope: &[&str]) -> Output {
        let binary = Path::new(env!("CARGO_BIN_EXE_loom"));
        let output = if scope == ["--tree"] && gate == self.tier {
            self.native.run(binary, gate, false)
        } else {
            Command::new(binary)
                .arg("--workspace")
                .arg(self.root())
                .args(["gate", gate])
                .args(scope)
                .env_remove("LOOM_INSIDE")
                .env_remove("LOOM_PARENT_DIFF_GATE")
                .output()
                .unwrap()
        };
        assert!(!self.root().join(".loom/marker.json").exists());
        output
    }
    fn config(&self, text: &str) {
        std::fs::write(self.root().join("loom.toml"), text).unwrap();
    }
    fn fail_alpha(&self) {
        self.native.record(
            "alpha",
            &format!(
                "{}\n",
                json!({"target":"alpha","pass":false,"evidence":"alpha failed"})
            ),
            1,
        );
    }
}

fn diagnostic(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
fn assert_exit(output: &Output, code: i32) {
    assert_eq!(output.status.code(), Some(code), "{}", diagnostic(output));
}
fn witnesses(output: &Output) -> BTreeSet<String> {
    diagnostic(output)
        .lines()
        .filter(|line| line.starts_with("loom gate ["))
        .filter_map(|line| {
            line.split_once("]: ")
                .and_then(|(_, value)| serde_json::from_str::<Value>(value).ok())
        })
        .flat_map(|value| {
            value["observations"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .filter(|value| value["kind"] == "fresh-execution")
        .map(|value| value["derivation"].as_str().unwrap().to_owned())
        .collect()
}
fn observed(output: &Output, target: &str) -> bool {
    diagnostic(output).contains(&format!("loom gate [{target}]: "))
}

#[test]
fn cli_selection_and_integrity_query_shared_provider_once() {
    for gate in ["check", "verify"] {
        let fixture = Fixture::new();
        let output = fixture.run(gate, &["--files", "records/alpha.output"]);
        assert_exit(&output, 0);
        assert_eq!(witnesses(&output).len(), 1);
        assert!(observed(&output, &fixture.address("alpha")));
        assert!(!observed(&output, &fixture.address("beta")));
        let output = fixture.run(gate, &["--files", "specs/inputs.md"]);
        assert_exit(&output, 0);
        assert_eq!(
            witnesses(&output).len(),
            2,
            "a contract edit selects both independent owners"
        );
    }
}

#[test]
fn cli_known_unaffected_targets_are_not_executed() {
    let fixture = Fixture::new();
    for gate in ["check", "verify"] {
        let output = fixture.run(gate, &["--files", "docs/unrelated.md"]);
        assert_exit(&output, 0);
        assert_eq!(witnesses(&output).len(), 0);
    }
}

#[test]
fn cli_cached_protocol_errors_remain_loud_for_each_owner() {
    let fixture = Fixture::new();
    let body = std::fs::read_to_string(fixture.root().join("specs/inputs.md")).unwrap();
    std::fs::write(
        fixture.root().join("specs/inputs.md"),
        format!(
            "{body}- Alpha sibling [check]({})\n",
            fixture.address("alpha")
        ),
    )
    .unwrap();
    fixture.native.record("alpha", "{malformed\n", 0);
    let output = fixture.run("check", &["--tree"]);
    assert_exit(&output, 1);
    assert_eq!(
        diagnostic(&output)
            .matches(&format!("ERROR: {}", fixture.address("alpha")))
            .count(),
        2
    );
    assert!(
        !loom_gate::StatusCache::open(&fixture.root().join(".loom/state.db"))
            .unwrap()
            .read_all()
            .unwrap()
            .iter()
            .any(|row| row.annotation_target == fixture.address("alpha")
                && row.verdict == loom_gate::Verdict::Pass)
    );
}

#[test]
fn cli_omitted_inputs_stay_always_run_without_extra_queries() {
    // Omission in caller JSON cannot replace a constructor's complete closure.
    let fixture = Fixture::new();
    fixture.config("[runner.check.legacy]\nmatch='^verify:(.+)$'\ncommand='touch executed'\ninputs='printf omitted'\n");
    std::fs::write(
        fixture.root().join("specs/inputs.md"),
        "## Success Criteria\n- Unknown [check](verify:beta)\n",
    )
    .unwrap();
    let output = fixture.run("check", &["--tree"]);
    assert_exit(&output, 1);
    assert_eq!(witnesses(&output).len(), 0);
    assert!(!fixture.root().join("executed").exists());
}

#[test]
fn cli_unknown_inputs_keep_runner_owned_targets_in_explicit_files() {
    let fixture = Fixture::new();
    fixture.config(
        "[runner.check.untrusted]\nmatch='.*'\ncommand='touch executed'\ninputs='printf unknown'\n",
    );
    fixture.fail_alpha();
    let output = fixture.run("check", &["--files", "records/alpha.output"]);
    assert_exit(&output, 1);
    assert!(observed(&output, &fixture.address("alpha")));
    assert!(!observed(&output, &fixture.address("beta")));
    assert!(!fixture.root().join("executed").exists());
}

#[test]
fn cli_runner_owned_failures_block_explicit_files() {
    let fixture = Fixture::new();
    fixture.fail_alpha();
    let output = fixture.run("check", &["--files", "records/alpha.output"]);
    assert_exit(&output, 1);
    assert!(diagnostic(&output).contains("alpha failed"));
}

#[test]
fn cli_flat_group_document_keeps_per_target_discovery_fallback() {
    let fixture = Fixture::new();
    let output = fixture.run("check", &["--files", "beta/records/beta.output"]);
    assert_exit(&output, 0);
    assert_eq!(witnesses(&output).len(), 1);
    assert!(!observed(&output, &fixture.address("alpha")));
    assert!(observed(&output, &fixture.address("beta")));
}

#[test]
fn cli_duplicate_owners_do_not_duplicate_discovery_targets() {
    let fixture = Fixture::new();
    let body = std::fs::read_to_string(fixture.root().join("specs/inputs.md")).unwrap();
    std::fs::write(
        fixture.root().join("specs/inputs.md"),
        format!(
            "{body}- Shared alpha [check]({})\n",
            fixture.address("alpha")
        ),
    )
    .unwrap();
    let output = fixture.run("check", &["--tree"]);
    assert_exit(&output, 0);
    assert_eq!(witnesses(&output).len(), 2);
    let rows = loom_gate::StatusCache::open(&fixture.root().join(".loom/state.db"))
        .unwrap()
        .read_all()
        .unwrap();
    assert_eq!(
        rows.iter()
            .filter(|row| row.annotation_target == fixture.address("alpha"))
            .count(),
        2
    );
}

#[test]
fn cli_queries_are_not_reused_by_later_gate_invocations() {
    let fixture = Fixture::new();
    let first = fixture.run("check", &["--tree"]);
    assert_exit(&first, 0);
    let second = fixture.run("check", &["--tree"]);
    assert_exit(&second, 0);
    assert_eq!(witnesses(&first).len(), 2);
    assert_eq!(witnesses(&second).len(), 2);
    assert!(witnesses(&first).is_disjoint(&witnesses(&second)));
}

#[test]
fn cli_verify_rejects_invalid_later_lane_queries_before_valid_sibling_execution() {
    let fixture = Fixture::new();
    let sdk = fixture.root().join("beta/verification.nix");
    let body = std::fs::read_to_string(&sdk).unwrap();
    assert!(body.contains("version = 1;"));
    std::fs::write(sdk, body.replace("version = 1;", "version = 99;")).unwrap();
    let output = fixture.run("verify", &["--tree"]);
    assert_exit(&output, 1);
    assert_eq!(witnesses(&output).len(), 0);
}

#[test]
fn cli_query_respects_tier_cwd_and_runner_override() {
    let fixture = Fixture::new();
    fixture.config("[runner.check.override]\nmatch='.*'\ncommand='touch executed'\ninputs='touch queried'\ncwd='beta'\n");
    let output = fixture.run("check", &["--files", "beta/records/beta.output"]);
    assert_exit(&output, 0);
    assert_eq!(witnesses(&output).len(), 1);
    assert!(observed(&output, &fixture.address("beta")));
    for directory in [fixture.root().to_path_buf(), fixture.root().join("beta")] {
        assert!(!directory.join("executed").exists());
        assert!(!directory.join("queried").exists());
    }
}

#[test]
fn cli_system_files_keeps_runner_owned_targets() {
    let fixture = Fixture::for_tier("system");
    let output = fixture.run("system", &["--files", "records/alpha.output"]);
    assert_exit(&output, 0);
    assert_eq!(witnesses(&output).len(), 1);
}

#[test]
fn cli_runner_ownership_does_not_cross_tiers() {
    let fixture = Fixture::for_tier("system");
    fixture.fail_alpha();
    let output = fixture.run("check", &["--tree"]);
    assert_exit(&output, 0);
    assert_eq!(witnesses(&output).len(), 0);
    let output = fixture.run("system", &["--tree"]);
    assert_exit(&output, 1);
}

#[test]
fn cli_explicit_files_still_reports_unclaimed_missing_paths() {
    let fixture = Fixture::new();
    let output = fixture.run("check", &["--files", "absent/unowned.txt"]);
    assert_exit(&output, 1);
    assert_eq!(witnesses(&output).len(), 0);
}

#[test]
fn cli_logical_target_diff_selection_keeps_affectedness() {
    let fixture = Fixture::new();
    fixture.fail_alpha();
    fixture.commit("change alpha data");
    let output = fixture.run("check", &["--diff", "HEAD~1..HEAD"]);
    assert_exit(&output, 1);
    assert_eq!(witnesses(&output).len(), 1);
    assert!(observed(&output, &fixture.address("alpha")));
    assert!(!observed(&output, &fixture.address("beta")));
}

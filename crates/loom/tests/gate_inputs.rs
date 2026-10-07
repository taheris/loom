//! Native CLI coverage for runner-owned logical targets and shared input discovery.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::{Command, Output};

use loom_gate::Tier;

const PROVIDER: &str = r#"#!/usr/bin/env bash
set -euo pipefail
mode=$(<"$INPUT_FIXTURE/mode.txt")
if [[ "${1-}" == "--print-inputs" ]]; then
    printf '%s\n' "$*" >> "$INPUT_FIXTURE/queries.txt"
    shift
    case "$mode" in
        malformed) printf '{"inputs":null}\n'; exit 0 ;;
        nonzero) exit 7 ;;
        single) printf '{"inputs":["src/alpha.rs"]}\n'; exit 0 ;;
        unknown) printf '{"inputs":{}}\n'; exit 0 ;;
    esac
    printf '{"inputs":{'
    first=1
    for target in "$@"; do
        if [[ "$mode" == omitted && "$target" == beta ]]; then
            continue
        fi
        if [[ "$first" != 1 ]]; then printf ','; fi
        printf '"%s":["provider.sh","loom.toml","src/%s.rs"]' "$target" "$target"
        first=0
    done
    printf '}}\n'
else
    printf '%s\n' "$*" >> "$INPUT_FIXTURE/batches.txt"
    for target in "$@"; do
        printf '%s\n' "$target" >> "$INPUT_FIXTURE/executions.txt"
        if [[ "$mode" == failed-exit ]]; then exit 9; fi
        pass=true
        if [[ "$mode" == failed-verdict && "$target" == alpha ]]; then pass=false; fi
        printf '{"target":"%s","pass":%s,"evidence":"fixture ran"}\n' "$target" "$pass"
    done
fi
"#;

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self::for_tier(Tier::Check)
    }

    fn for_tier(tier: Tier) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("specs")).unwrap();
        std::fs::create_dir(root.join("src")).unwrap();
        for target in ["alpha", "beta", "unrelated"] {
            std::fs::write(root.join(format!("src/{target}.rs")), "").unwrap();
        }
        std::fs::write(root.join("provider.sh"), PROVIDER).unwrap();
        std::fs::write(root.join("mode.txt"), "batch").unwrap();
        for name in ["queries.txt", "executions.txt", "batches.txt"] {
            std::fs::write(root.join(name), "").unwrap();
        }
        std::fs::write(
            root.join("loom.toml"),
            format!(
                r"[runner.{tier}.fixture]
match = '^verify:(.+)$'
command = 'bash provider.sh {{targets}}'
target = '{{capture_1}}'
join = ' '
parse = 'json-lines'
inputs = 'bash provider.sh {{print_inputs}} {{targets}}'
",
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("specs/inputs.md"),
            format!(
                "## Success Criteria\n\n- alpha [{tier}](verify:alpha)\n- beta [{tier}](verify:beta)\n",
            ),
        )
        .unwrap();
        Self { dir }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn run(&self, scope: &[&str]) -> Output {
        self.run_gate("check", scope)
    }

    fn run_gate(&self, gate: &str, scope: &[&str]) -> Output {
        // A real CLI process is required to exercise scope filtering before runner dispatch.
        let binary = std::env::var_os("LOOM_GATE_INPUTS_BIN")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_loom").into());
        let output = Command::new(binary)
            .arg("--workspace")
            .arg(self.root())
            .args(["gate", gate])
            .args(scope)
            .env_remove("LOOM_INSIDE")
            .env_remove("LOOM_PARENT_DIFF_GATE")
            .env("INPUT_FIXTURE", self.root())
            .output()
            .expect("run gate CLI");
        assert!(!self.root().join(".loom/marker.json").exists());
        output
    }

    fn lines(&self, name: &str) -> Vec<String> {
        std::fs::read_to_string(self.root().join(name))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn assert_exit(output: &Output, code: i32) {
        assert_eq!(
            output.status.code(),
            Some(code),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    fn assert_success(output: &Output) {
        Self::assert_exit(output, 0);
    }
}

#[test]
fn cli_selection_and_integrity_query_shared_provider_once() {
    for gate in ["check", "verify"] {
        for (scope, executed, batch) in [
            (vec!["--files", "src/alpha.rs"], vec!["alpha"], "alpha"),
            (
                vec!["--files", "specs/inputs.md"],
                vec!["alpha", "beta"],
                "alpha beta",
            ),
            (vec!["--tree"], vec!["alpha", "beta"], "alpha beta"),
        ] {
            let fixture = Fixture::new();
            Fixture::assert_success(&fixture.run_gate(gate, &scope));
            assert_eq!(fixture.lines("queries.txt"), ["--print-inputs alpha beta"]);
            assert_eq!(fixture.lines("executions.txt"), executed);
            assert_eq!(fixture.lines("batches.txt"), [batch]);
        }
    }
}

#[test]
fn cli_known_unaffected_targets_are_not_executed() {
    for gate in ["check", "verify"] {
        let fixture = Fixture::new();
        Fixture::assert_success(&fixture.run_gate(gate, &["--files", "src/unrelated.rs"]));
        assert_eq!(fixture.lines("queries.txt"), ["--print-inputs alpha beta"]);
        assert!(fixture.lines("executions.txt").is_empty());
        assert!(fixture.lines("batches.txt").is_empty());
    }
}

#[test]
fn cli_cached_protocol_errors_remain_loud_for_each_owner() {
    for gate in ["check", "verify"] {
        for mode in ["malformed", "nonzero"] {
            let fixture = Fixture::new();
            std::fs::write(fixture.root().join("mode.txt"), mode).unwrap();
            std::fs::write(
                fixture.root().join("specs/shared.md"),
                "## Success Criteria\n\n- shared alpha [check](verify:alpha)\n",
            )
            .unwrap();
            let output = fixture.run_gate(gate, &["--files", "src/unrelated.rs"]);
            Fixture::assert_exit(&output, 1);
            let stderr = String::from_utf8_lossy(&output.stderr);
            for owner in [
                "specs/inputs.md:3",
                "specs/inputs.md:4",
                "specs/shared.md:3",
            ] {
                assert!(stderr.contains(owner), "{stderr}");
            }
            assert!(
                stderr.contains("verify:alpha") && stderr.contains("verify:beta"),
                "{stderr}"
            );
            assert_eq!(stderr.matches("input-query errored").count(), 3, "{stderr}");
            assert_eq!(fixture.lines("queries.txt"), ["--print-inputs alpha beta"]);
            assert_eq!(fixture.lines("executions.txt"), ["alpha", "beta"]);
            assert_eq!(fixture.lines("batches.txt"), ["alpha beta"]);
        }
    }
}

#[test]
fn cli_omitted_inputs_stay_always_run_without_extra_queries() {
    for gate in ["check", "verify"] {
        let fixture = Fixture::new();
        std::fs::write(fixture.root().join("mode.txt"), "omitted").unwrap();
        Fixture::assert_success(&fixture.run_gate(gate, &["--files", "src/unrelated.rs"]));
        assert_eq!(fixture.lines("queries.txt"), ["--print-inputs alpha beta"]);
        assert_eq!(fixture.lines("executions.txt"), ["beta"]);
        assert_eq!(fixture.lines("batches.txt"), ["beta"]);
    }
}

#[test]
fn cli_unknown_inputs_keep_runner_owned_targets_in_explicit_files() {
    for gate in ["check", "verify"] {
        let fixture = Fixture::new();
        std::fs::write(fixture.root().join("mode.txt"), "unknown").unwrap();
        Fixture::assert_success(&fixture.run_gate(gate, &["--files", "src/unrelated.rs"]));
        assert_eq!(fixture.lines("queries.txt"), ["--print-inputs alpha beta"]);
        assert_eq!(fixture.lines("executions.txt"), ["alpha", "beta"]);
        assert_eq!(fixture.lines("batches.txt"), ["alpha beta"]);
    }
}

#[test]
fn cli_runner_owned_failures_block_explicit_files() {
    for gate in ["check", "verify"] {
        for mode in ["failed-verdict", "failed-exit"] {
            let fixture = Fixture::new();
            std::fs::write(fixture.root().join("mode.txt"), mode).unwrap();
            let output = fixture.run_gate(gate, &["--files", "src/alpha.rs"]);
            Fixture::assert_exit(&output, 1);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("verify:alpha"), "{stderr}");
            assert_eq!(fixture.lines("queries.txt"), ["--print-inputs alpha beta"]);
            assert_eq!(fixture.lines("executions.txt"), ["alpha"]);
            assert_eq!(fixture.lines("batches.txt"), ["alpha"]);
        }
    }
}

#[test]
fn cli_flat_group_document_keeps_per_target_discovery_fallback() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root().join("mode.txt"), "single").unwrap();
    Fixture::assert_success(&fixture.run(&["--files", "src/alpha.rs"]));
    assert_eq!(
        fixture.lines("queries.txt"),
        [
            "--print-inputs alpha beta",
            "--print-inputs alpha",
            "--print-inputs beta"
        ],
    );
    assert_eq!(fixture.lines("executions.txt"), ["alpha", "beta"]);
    assert_eq!(fixture.lines("batches.txt"), ["alpha beta"]);
}

#[test]
fn cli_duplicate_owners_do_not_duplicate_discovery_targets() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.root().join("specs/inputs.md"),
        "## Success Criteria\n\n- first [check](verify:alpha)\n- second [check](verify:alpha)\n- third [check](verify:beta)\n",
    )
    .unwrap();
    Fixture::assert_success(&fixture.run(&["--tree"]));
    assert_eq!(fixture.lines("queries.txt"), ["--print-inputs alpha beta"]);
    assert_eq!(fixture.lines("executions.txt"), ["alpha", "beta"]);
    assert_eq!(fixture.lines("batches.txt"), ["alpha beta"]);
}

#[test]
fn cli_queries_are_not_reused_by_later_gate_invocations() {
    let fixture = Fixture::new();
    Fixture::assert_success(&fixture.run(&["--files", "src/alpha.rs"]));
    std::fs::write(fixture.root().join("mode.txt"), "nonzero").unwrap();
    Fixture::assert_exit(&fixture.run(&["--files", "src/alpha.rs"]), 1);
    assert_eq!(
        fixture.lines("queries.txt"),
        ["--print-inputs alpha beta"; 2]
    );
    assert_eq!(fixture.lines("executions.txt"), ["alpha", "alpha", "beta"]);
    assert_eq!(fixture.lines("batches.txt"), ["alpha", "alpha beta"]);
}

#[test]
fn cli_query_respects_tier_cwd_and_runner_override() {
    for runner_override in [false, true] {
        let fixture = Fixture::new();
        std::fs::create_dir(fixture.root().join("tier-dir")).unwrap();
        std::fs::create_dir(fixture.root().join("runner-dir")).unwrap();
        let cwd = if runner_override {
            "runner-dir"
        } else {
            "tier-dir"
        };
        std::fs::rename(
            fixture.root().join("provider.sh"),
            fixture.root().join(cwd).join("provider.sh"),
        )
        .unwrap();
        let config = std::fs::read_to_string(fixture.root().join("loom.toml")).unwrap();
        let override_config = if runner_override {
            "cwd = 'runner-dir'\n"
        } else {
            ""
        };
        std::fs::write(
            fixture.root().join("loom.toml"),
            format!("[runner.check]\ncwd = 'tier-dir'\n{config}{override_config}"),
        )
        .unwrap();
        Fixture::assert_success(&fixture.run(&["--files", "src/alpha.rs"]));
        assert_eq!(fixture.lines("queries.txt"), ["--print-inputs alpha beta"]);
        assert_eq!(fixture.lines("executions.txt"), ["alpha"]);
        assert_eq!(fixture.lines("batches.txt"), ["alpha"]);
    }
}

#[test]
fn cli_system_files_keeps_runner_owned_targets() {
    let fixture = Fixture::for_tier(Tier::System);
    Fixture::assert_success(&fixture.run_gate("system", &["--files", "src/alpha.rs"]));
    assert_eq!(
        fixture.lines("queries.txt"),
        ["--print-inputs alpha", "--print-inputs beta"]
    );
    assert_eq!(fixture.lines("executions.txt"), ["alpha"]);
    assert_eq!(fixture.lines("batches.txt"), ["alpha"]);
}

#[test]
fn cli_runner_ownership_does_not_cross_tiers() {
    for (runner_tier, annotation_tier) in [(Tier::Check, Tier::System), (Tier::System, Tier::Check)]
    {
        let fixture = Fixture::for_tier(runner_tier);
        std::fs::write(
            fixture.root().join("specs/inputs.md"),
            format!("## Success Criteria\n\n- unclaimed [{annotation_tier}](verify:alpha)\n"),
        )
        .unwrap();
        Fixture::assert_success(&fixture.run_gate(
            &annotation_tier.to_string(),
            &["--files", "specs/inputs.md"],
        ));
        let output = fixture.run(&["--tree"]);
        Fixture::assert_exit(&output, 1);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("verify:alpha") && stderr.contains("does not resolve"),
            "{stderr}"
        );
        assert!(fixture.lines("queries.txt").is_empty());
        assert!(fixture.lines("executions.txt").is_empty());
        assert!(fixture.lines("batches.txt").is_empty());
    }
}

#[test]
fn cli_explicit_files_still_reports_unclaimed_missing_paths() {
    for gate in ["check", "verify"] {
        let fixture = Fixture::new();
        std::fs::write(
            fixture.root().join("specs/inputs.md"),
            "## Success Criteria\n\n- missing path [check](./missing-verifier.sh)\n",
        )
        .unwrap();
        let output = fixture.run_gate(gate, &["--files", "specs/inputs.md"]);
        Fixture::assert_exit(&output, 1);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("./missing-verifier.sh") && stderr.contains("does not resolve"),
            "{stderr}"
        );
        assert!(fixture.lines("queries.txt").is_empty());
        assert!(fixture.lines("executions.txt").is_empty());
    }
}

#[test]
fn cli_logical_target_diff_selection_keeps_affectedness() {
    for gate in ["check", "verify"] {
        let fixture = Fixture::new();
        std::fs::write(
            fixture.root().join(".gitignore"),
            ".loom/\nqueries.txt\nexecutions.txt\nbatches.txt\n",
        )
        .unwrap();
        loom_driver::git::init_test_repo_with_integration(fixture.root()).unwrap();
        std::fs::write(
            fixture.root().join(".pre-commit-config.yaml"),
            "repos:\n  - repo: local\n    hooks:\n      - id: feedback\n        name: Fixture feedback\n        entry: true\n        language: system\n        stages: [pre-commit]\n        always_run: true\n        pass_filenames: false\n",
        )
        .unwrap();
        loom_driver::git::commit_all_in(fixture.root(), "Seed logical target fixture").unwrap();
        std::fs::write(fixture.root().join("src/alpha.rs"), "changed\n").unwrap();
        Fixture::assert_success(&fixture.run_gate(gate, &["--diff", "HEAD"]));
        assert_eq!(fixture.lines("queries.txt"), ["--print-inputs alpha beta"]);
        assert_eq!(fixture.lines("executions.txt"), ["alpha"]);
        assert_eq!(fixture.lines("batches.txt"), ["alpha"]);
    }
}

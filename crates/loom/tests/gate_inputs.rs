//! Live-path discovery sharing between CLI selection and integrity auditing.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::{Command, Output};

const PROVIDER: &str = r#"#!/bin/sh
set -eu
if [ "${1-}" = "--print-inputs" ]; then
    printf '%s\n' "$*" >> "$INPUT_FIXTURE/queries.txt"
    shift
    mode=$(cat "$INPUT_FIXTURE/mode.txt")
    case "$mode" in
        malformed) printf '{"inputs":null}\n'; exit 0 ;;
        nonzero) exit 7 ;;
        single) printf '{"inputs":["src/alpha.rs"]}\n'; exit 0 ;;
    esac
    printf '{"inputs":{'
    first=1
    for target in "$@"; do
        [ "$mode" = "omitted" ] && [ "$target" = "beta" ] && continue
        [ "$first" = 1 ] || printf ','
        printf '"%s":["provider.sh","loom.toml","src/%s.rs"]' "$target" "$target"
        first=0
    done
    printf '}}\n'
else
    for target in "$@"; do
        printf '%s\n' "$target" >> "$INPUT_FIXTURE/executions.txt"
        printf '{"target":"%s","pass":true,"evidence":"fixture ran"}\n' "$target"
    done
fi
"#;

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("specs")).unwrap();
        std::fs::create_dir(root.join("src")).unwrap();
        for target in ["alpha", "beta", "unrelated"] {
            std::fs::write(root.join(format!("src/{target}.rs")), "").unwrap();
        }
        std::fs::write(root.join("provider.sh"), PROVIDER).unwrap();
        std::fs::write(root.join("mode.txt"), "batch").unwrap();
        std::fs::write(root.join("queries.txt"), "").unwrap();
        std::fs::write(root.join("executions.txt"), "").unwrap();
        std::fs::write(
            root.join("loom.toml"),
            r"[runner.check.fixture]
match = '^sh provider\.sh -- (\S+)$'
command = 'sh provider.sh {targets}'
target = '{capture_1}'
join = ' '
parse = 'json-lines'
inputs = 'sh provider.sh {print_inputs} {targets}'
",
        )
        .unwrap();
        std::fs::write(
            root.join("specs/inputs.md"),
            "## Success Criteria\n\n- alpha [check](sh provider.sh -- alpha)\n- beta [check](sh provider.sh -- beta)\n",
        )
        .unwrap();
        Self { dir }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn run(&self, scope: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_loom"))
            .arg("--workspace")
            .arg(self.root())
            .args(["gate", "check"])
            .args(scope)
            .env_remove("LOOM_INSIDE")
            .env("INPUT_FIXTURE", self.root())
            .output()
            .expect("run gate check")
    }

    fn lines(&self, name: &str) -> Vec<String> {
        std::fs::read_to_string(self.root().join(name))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn assert_success(output: &Output) {
        assert!(
            output.status.success(),
            "status={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

#[test]
fn cli_selection_and_integrity_query_shared_provider_once() {
    for (scope, executed) in [
        (vec!["--files", "src/alpha.rs"], vec!["alpha"]),
        (vec!["--files", "specs/inputs.md"], vec!["alpha", "beta"]),
        (vec!["--tree"], vec!["alpha", "beta"]),
    ] {
        let fixture = Fixture::new();
        Fixture::assert_success(&fixture.run(&scope));
        assert_eq!(fixture.lines("queries.txt"), ["--print-inputs alpha beta"]);
        assert_eq!(fixture.lines("executions.txt"), executed);
    }
}

#[test]
fn cli_cached_protocol_errors_remain_loud_for_each_owner() {
    for mode in ["malformed", "nonzero"] {
        let fixture = Fixture::new();
        std::fs::write(fixture.root().join("mode.txt"), mode).unwrap();
        let output = fixture.run(&["--files", "src/unrelated.rs"]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "invalid input query must fail the gate"
        );
        assert!(
            stderr.contains("sh provider.sh -- alpha") && stderr.contains("sh provider.sh -- beta"),
            "{stderr}"
        );
        assert!(stderr.contains("input-query errored"), "{stderr}");
        assert_eq!(fixture.lines("queries.txt").len(), 1);
        assert_eq!(fixture.lines("executions.txt"), ["alpha", "beta"]);
    }
}

#[test]
fn cli_omitted_inputs_stay_always_run_without_extra_queries() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root().join("mode.txt"), "omitted").unwrap();
    Fixture::assert_success(&fixture.run(&["--files", "src/unrelated.rs"]));
    assert_eq!(fixture.lines("queries.txt").len(), 1);
    assert_eq!(fixture.lines("executions.txt"), ["beta"]);
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
}

#[test]
fn cli_duplicate_owners_do_not_duplicate_discovery_targets() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.root().join("specs/inputs.md"),
        "## Success Criteria\n\n- first [check](sh provider.sh -- alpha)\n- second [check](sh provider.sh -- alpha)\n- third [check](sh provider.sh -- beta)\n",
    )
    .unwrap();
    Fixture::assert_success(&fixture.run(&["--tree"]));
    assert_eq!(fixture.lines("queries.txt"), ["--print-inputs alpha beta"]);
    assert_eq!(fixture.lines("executions.txt"), ["alpha", "beta"]);
}

#[test]
fn cli_queries_are_not_reused_by_later_gate_invocations() {
    let fixture = Fixture::new();
    Fixture::assert_success(&fixture.run(&["--files", "src/alpha.rs"]));
    std::fs::write(fixture.root().join("mode.txt"), "nonzero").unwrap();
    let output = fixture.run(&["--files", "src/alpha.rs"]);
    assert!(!output.status.success());
    assert_eq!(fixture.lines("queries.txt").len(), 2);
    assert_eq!(fixture.lines("executions.txt"), ["alpha", "alpha", "beta"]);
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
        assert_eq!(fixture.lines("queries.txt").len(), 1);
        assert_eq!(fixture.lines("executions.txt"), ["alpha"]);
    }
}

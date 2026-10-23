//! Shared native System execution retains every owner without diagnostic authority.
#![expect(
    clippy::unwrap_used,
    reason = "isolated integration setup fails closed"
)]

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::process::Output;

use loom_gate::{StatusCache, Verdict};
use serde_json::{Value, json};

#[path = "common/reported.rs"]
mod reported;

fn fixture(configured: bool) -> reported::Fixture {
    let fixture = reported::Fixture::new("system", "json-lines", &["shared"]);
    fs::remove_dir_all(fixture.path().join("specs/fixture")).unwrap();
    let mut links = String::new();
    for (label, count) in [("a", 2), ("b", 1), ("c", 1)] {
        let mut body = format!("# {label}\n\n## Success Criteria\n\n");
        for index in 0..count {
            writeln!(
                body,
                "- Owner {label}-{index} is verified\n  [system]({})",
                fixture.address("shared")
            )
            .unwrap();
        }
        fs::write(fixture.path().join(format!("specs/{label}.md")), body).unwrap();
        writeln!(links, "- [{label}](../specs/{label}.md)").unwrap();
    }
    fs::write(fixture.path().join("docs/README.md"), links).unwrap();
    fixture.policy("system", false);
    if configured {
        fs::write(fixture.path().join("loom.toml"), "[runner.system.untrusted]\nmatch='^nix report .*::shared$'\ncommand='touch should-not-run'\ninputs='touch should-not-query'\nparse='json-lines'\n").unwrap();
    }
    fixture
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Pass,
    Fail,
    Skip,
    Invalid,
}

fn run(fixture: &reported::Fixture, mode: Mode, scope: &[&str]) -> Output {
    let (record, status) = match mode {
        Mode::Pass => (json!({"target":"shared","pass":true,"evidence":"executed"}).to_string(), 0),
        Mode::Fail => (json!({"target":"shared","pass":false,"evidence":"assertion failed"}).to_string(), 1),
        Mode::Skip => (json!({"target":"shared","pass":false,"skipped":true,"evidence":"prerequisite missing"}).to_string(), 77),
        Mode::Invalid => (json!({"target":"shared","pass":"wrong type","evidence":"invalid"}).to_string(), 0),
    };
    fixture.record("shared", &format!("{record}\n"), status);
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_loom"));
    command
        .arg("--workspace")
        .arg(fixture.path())
        .args(["gate", "system"])
        .args(scope)
        .env_remove("LOOM_INSIDE");
    let output = if scope == ["--tree"] {
        fixture.run(
            std::path::Path::new(env!("CARGO_BIN_EXE_loom")),
            "system",
            false,
        )
    } else {
        command.output().unwrap()
    };
    assert!(!fixture.path().join("should-not-run").exists());
    assert!(!fixture.path().join("should-not-query").exists());
    output
}

fn witnesses(diagnostic: &str) -> BTreeSet<String> {
    diagnostic
        .lines()
        .filter(|line| line.starts_with("loom gate ["))
        .filter_map(|line| {
            line.split_once("]: ")
                .and_then(|(_, body)| serde_json::from_str::<Value>(body).ok())
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

#[test]
fn system_cli_sharing_preserves_per_criterion_reports_failures_skips_and_cache_records() {
    for configured in [false, true] {
        for (mode, exit, verdict) in [
            (Mode::Pass, 0, Verdict::Pass),
            (Mode::Fail, 1, Verdict::Fail),
            (Mode::Skip, 78, Verdict::Skipped),
        ] {
            let fixture = fixture(configured);
            let output = run(&fixture, mode, &["--tree"]);
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert_eq!(output.status.code(), Some(exit), "{stderr}");
            assert_eq!(
                witnesses(&stderr).len(),
                1,
                "one current producer execution, not a cached verdict: {stderr}"
            );
            let rows = StatusCache::open(&fixture.path().join(".loom/state.db"))
                .unwrap()
                .read_all()
                .unwrap();
            assert_eq!(rows.len(), 4, "same-spec siblings also require evidence");
            assert_eq!(
                rows.iter()
                    .map(|row| (&row.spec_label, &row.criterion_anchor))
                    .collect::<BTreeSet<_>>()
                    .len(),
                4
            );
            assert!(
                rows.iter()
                    .all(|row| row.verdict == verdict && row.evidence == rows[0].evidence)
            );
            let records = fs::read_to_string(
                fixture
                    .path()
                    .join(".loom/logs/gate/verifier-results.jsonl"),
            )
            .unwrap();
            let reports = records
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(reports.len(), 4);
            assert!(
                reports
                    .iter()
                    .all(|record| record["outcome"] == verdict.as_wire()
                        && record["coverage_verified"] == (verdict == Verdict::Pass))
            );
            if mode != Mode::Pass {
                assert_eq!(
                    stderr
                        .matches(if mode == Mode::Skip {
                            "SKIP (coverage unverified"
                        } else {
                            "FAIL:"
                        })
                        .count(),
                    4,
                    "{stderr}"
                );
            }
            assert!(!fixture.path().join(".loom/marker.json").exists());
        }
    }
}

#[test]
fn system_cli_failure_reports_diagnostics_after_build_preamble() {
    let fixture = fixture(false);
    let script = fixture.path().join("producer.sh");
    let original = fs::read_to_string(&script).unwrap();
    fs::write(script, format!("for i in {{1..15}}; do printf 'building derivation %s\\n' \"$i\" >&2; done\nprintf 'invalid spec label: smoke.md\\n' >&2\n{original}")).unwrap();
    let output = run(&fixture, Mode::Fail, &["--tree"]);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("building derivation 1\\n") || stderr.contains("building derivation 1\n"),
        "{stderr}"
    );
    assert!(stderr.contains("invalid spec label: smoke.md"), "{stderr}");
}

#[test]
fn system_cli_shared_dispatch_errors_report_every_owner_without_passing_cache_entries() {
    for configured in [false, true] {
        let fixture = fixture(configured);
        let output = run(&fixture, Mode::Invalid, &["--tree"]);
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(1), "{stderr}");
        assert_eq!(stderr.matches("ERROR:").count(), 4, "{stderr}");
        assert_eq!(
            StatusCache::open(&fixture.path().join(".loom/state.db"))
                .unwrap()
                .read_all()
                .unwrap()
                .len(),
            0
        );
    }
}

#[test]
fn system_cli_exact_shared_target_executes_again_on_a_later_gate_run() {
    let fixture = fixture(false);
    let mut previous = None;
    for _ in 0..2 {
        let output = run(
            &fixture,
            Mode::Pass,
            &["--target", &fixture.address("shared")],
        );
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "{stderr}");
        let current = witnesses(&stderr);
        assert_eq!(current.len(), 1, "{stderr}");
        if let Some(previous) = previous {
            assert_ne!(current, previous);
        }
        previous = Some(current);
        assert!(
            !fixture.path().join(".loom/state.db").exists(),
            "exact diagnostics cannot authorize criterion coverage"
        );
    }
}

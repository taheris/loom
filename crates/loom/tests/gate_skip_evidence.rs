//! Opaque parser results retain criterion evidence; Rust names require an actual compiler catalogue.
#![allow(
    clippy::unwrap_used,
    reason = "isolated integration setup and assertions"
)]

use loom_driver::git::GitClient;
use loom_gate::cache::{StatusCache, Verdict};
use loom_templates::criterion_status::{CriterionResult, EvidenceState};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

#[path = "common/cargo.rs"]
mod cargo;
#[path = "common/reported.rs"]
mod reported;

const TARGETS: [&str; 3] = ["executed", "unavailable", "failed"];
fn address(target: &str) -> String {
    format!(
        "nix report .#checks.{}.probe::{target}",
        loom_gate::runner::result::current_platform()
    )
}

fn fixture(parser: &str) -> reported::Fixture {
    let fixture = reported::Fixture::new("check", parser, &TARGETS);
    fixture.policy("check", false);
    assert_eq!(fixture.address("executed"), address("executed"));
    std::fs::remove_dir_all(fixture.path().join("specs/fixture")).unwrap();
    std::fs::write(
        fixture.path().join("docs/README.md"),
        "- [Gate](../specs/gate.md)\n",
    )
    .unwrap();
    let criteria = TARGETS
        .map(|target| format!("- Evidence for {target} [check]({})\n", address(target)))
        .concat();
    std::fs::write(
        fixture.path().join("specs/gate.md"),
        format!("# Gate\n\n## Success Criteria\n{criteria}"),
    )
    .unwrap();
    fixture
}

fn run_report(fixture: &reported::Fixture, report: &str, code: i32) -> std::process::Output {
    fixture.record("executed", report, code);
    for target in ["unavailable", "failed"] {
        fixture.record(target, "", 0);
    }
    // Operator cwd is deliberately not the admitted fixture cwd.
    fixture.run(Path::new(env!("CARGO_BIN_EXE_loom")), "check", false)
}

fn report(parser: &str, outcomes: [Verdict; 3]) -> String {
    let cases = TARGETS
        .into_iter()
        .zip(outcomes)
        .map(|(target, outcome)| {
            if parser == "libtest-json" {
                let event = match outcome {
                    Verdict::Pass => "ok",
                    Verdict::Fail => "failed",
                    Verdict::Skipped => "ignored",
                };
                format!("{{\"type\":\"test\",\"event\":\"{event}\",\"name\":\"{target}\"}}\n")
            } else {
                let child = match outcome {
                    Verdict::Pass => "",
                    Verdict::Fail => "<failure message='broken'/>",
                    Verdict::Skipped => "<skipped message='unavailable'/>",
                };
                format!("<testcase name='{target}'>{child}</testcase>")
            }
        })
        .collect::<Vec<_>>()
        .concat();
    if parser == "libtest-json" {
        cases
    } else {
        format!("<testsuite>{cases}</testsuite>")
    }
}

#[expect(
    clippy::panic,
    reason = "missing/currentness failures must abort the evidence assertion"
)]
async fn criterion_results(workspace: &Path) -> BTreeMap<String, CriterionResult> {
    let git = GitClient::open(workspace).unwrap();
    loom_workflow::todo::build_criterion_status(
        workspace,
        &workspace.join(".loom/state.db"),
        &"gate".parse().unwrap(),
        Path::new("specs/gate.md"),
        &git,
    )
    .await
    .unwrap()
    .into_iter()
    .map(|row| {
        let EvidenceState::Current { result, .. } = row.evidence else {
            panic!("expected current evidence for {}", row.annotation.target);
        };
        (row.annotation.target.to_string(), result)
    })
    .collect()
}

#[tokio::test]
async fn skipped_tests_survive_cli_cache_and_criterion_evidence() {
    for parser in ["libtest-json", "junit-xml"] {
        let fixture = fixture(parser);
        let output = run_report(&fixture, &report(parser, [Verdict::Pass; 3]), 0);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let initial = criterion_results(fixture.path()).await;
        assert_eq!(initial.len(), 3);
        assert!(
            initial
                .values()
                .all(|result| *result == CriterionResult::Pass)
        );
        let output = run_report(
            &fixture,
            &report(parser, [Verdict::Pass, Verdict::Skipped, Verdict::Fail]),
            1,
        );
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("SKIP"));
        let mixed = criterion_results(fixture.path()).await;
        assert_eq!(mixed[&address("executed")], CriterionResult::Pass);
        assert_eq!(mixed[&address("unavailable")], CriterionResult::Skipped);
        assert_eq!(mixed[&address("failed")], CriterionResult::Fail);
        let output = run_report(&fixture, &report(parser, [Verdict::Skipped; 3]), 77);
        assert!(
            !output.status.success(),
            "unclassified skips cannot grant permission"
        );
        let rows = StatusCache::open(&fixture.path().join(".loom/state.db"))
            .unwrap()
            .read_all()
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.verdict == Verdict::Skipped));
        assert!(
            criterion_results(fixture.path())
                .await
                .values()
                .all(|result| *result == CriterionResult::Skipped)
        );
    }
}

#[test]
fn unstructured_cargo_skips_do_not_certify_every_target() {
    let fixture = cargo::fixture();
    let contract = fixture.path().join("specs/probe/tests.md");
    // Runnable/ignored facts come from the actual compiler. JSON cannot promote an ignored case.
    std::fs::write(
        &contract,
        "## Success Criteria\n- Ignored case [test](ignored_case)\n",
    )
    .unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_loom"))
            .arg("--workspace")
            .arg(fixture.path())
            .args(["gate", "test", "--tree"])
            .output()
            .unwrap()
    };
    let blocked = run();
    assert!(
        !blocked.status.success(),
        "ignored compiler facts cannot authorize a named test"
    );
    assert!(!String::from_utf8_lossy(&blocked.stderr).contains("\"kind\":\"fresh-execution\""));
    std::fs::write(
        &contract,
        "## Success Criteria\n- Actual case [test](native_case)\n",
    )
    .unwrap();
    let passed = run();
    assert!(
        passed.status.success(),
        "{}",
        String::from_utf8_lossy(&passed.stderr)
    );
    std::fs::write(fixture.path().join("payload/input.txt"), "invalid\n").unwrap();
    let failed = run();
    assert!(
        !failed.status.success(),
        "real assertion failure must dominate ignored suite events"
    );
    let rows = StatusCache::open(&fixture.path().join(".loom/state.db"))
        .unwrap()
        .read_all()
        .unwrap();
    assert!(
        rows.iter()
            .any(|row| row.annotation_target == "native_case" && row.verdict == Verdict::Fail)
    );
}

#[test]
fn malformed_junit_fails_without_persisting_partial_passes() {
    let fixture = fixture("junit-xml");
    let malformed = report("junit-xml", [Verdict::Pass; 3]).replace("</testsuite>", "");
    let output = run_report(&fixture, &malformed, 1);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("junit-xml"), "{stderr}");
    assert_eq!(
        StatusCache::open(&fixture.path().join(".loom/state.db"))
            .unwrap()
            .read_all()
            .unwrap()
            .len(),
        0
    );
}

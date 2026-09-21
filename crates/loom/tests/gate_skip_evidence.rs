//! Skips survive the real CLI, persisted cache and todo evidence boundary.

#![allow(
    clippy::unwrap_used,
    reason = "integration fixture setup and assertions"
)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Output};

use loom_driver::git::GitClient;
use loom_gate::cache::{StatusCache, Verdict};
use loom_templates::criterion_status::{CriterionResult, EvidenceState};

const TARGETS: [&str; 3] = ["executed", "unavailable", "failed"];

fn fixture(parser: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("specs")).unwrap();
    let mut spec = "# Gate\n\n## Success Criteria\n\n".to_string();
    for target in TARGETS {
        writeln!(spec, "- Evidence for {target} [test]({target})").unwrap();
    }
    std::fs::write(dir.path().join("specs/gate.md"), spec).unwrap();
    std::fs::write(dir.path().join("runner.sh"), "cat report\n").unwrap();
    std::fs::write(dir.path().join("report"), "").unwrap();
    std::fs::write(
        dir.path().join("loom.toml"),
        format!("[runner.test]\ncommand = 'sh runner.sh'\nparse = '{parser}'\n"),
    )
    .unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "."],
        vec!["commit", "-qm", "fixture"],
    ] {
        let mut git = Command::new("git");
        loom_test_support::scrub_git_local_env(&mut git);
        let output = git
            .current_dir(dir.path())
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    dir
}

fn run_report(workspace: &Path, report: &str) -> Output {
    std::fs::write(workspace.join("report"), report).unwrap();
    // Deliberately invoke from outside the fixture: runner cwd must be the
    // selected workspace rather than the operator's current directory.
    Command::new(env!("CARGO_BIN_EXE_loom"))
        .args([
            "--workspace",
            workspace.to_str().unwrap(),
            "gate",
            "test",
            "--tree",
        ])
        .env("CARGO_TARGET_DIR", workspace.join("target"))
        .output()
        .unwrap()
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
        .collect::<String>();
    if parser == "libtest-json" {
        cases
    } else {
        format!("<testsuite>{cases}</testsuite>")
    }
}

#[expect(
    clippy::panic,
    reason = "fixture must fail if persisted evidence is absent or stale"
)]
async fn criterion_results(workspace: &Path) -> BTreeMap<String, CriterionResult> {
    let git = GitClient::open(workspace).unwrap();
    loom_workflow::todo::build_criterion_status(
        workspace,
        &workspace.join(".loom/cache.db"),
        &"gate".parse().unwrap(),
        Path::new("specs/gate.md"),
        &git,
    )
    .await
    .into_iter()
    .map(|row| {
        let EvidenceState::Current { result, .. } = row.evidence else {
            panic!(
                "expected persisted current evidence for {}",
                row.annotation.target
            )
        };
        (row.annotation.target.to_string(), result)
    })
    .collect()
}

#[tokio::test]
async fn skipped_tests_survive_cli_cache_and_criterion_evidence() {
    for parser in ["libtest-json", "junit-xml"] {
        let dir = fixture(parser);
        let output = run_report(dir.path(), &report(parser, [Verdict::Pass; 3]));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let initial = criterion_results(dir.path()).await;
        assert_eq!(initial.len(), 3);
        assert!(
            initial
                .values()
                .all(|result| *result == CriterionResult::Pass)
        );

        let output = run_report(
            dir.path(),
            &report(parser, [Verdict::Pass, Verdict::Skipped, Verdict::Fail]),
        );
        assert!(
            !output.status.success(),
            "a reported failure must fail even when the fixture process exits zero"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("SKIP"));
        let mixed = criterion_results(dir.path()).await;
        assert_eq!(mixed["executed"], CriterionResult::Pass);
        assert_eq!(mixed["unavailable"], CriterionResult::Skipped);
        assert_eq!(mixed["failed"], CriterionResult::Fail);

        let output = run_report(dir.path(), &report(parser, [Verdict::Skipped; 3]));
        assert!(
            output.status.success(),
            "legitimate skips remain neutral, not passes"
        );
        let cache = StatusCache::open(&dir.path().join(".loom/cache.db")).unwrap();
        let rows = cache.read_all().unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.verdict == Verdict::Skipped));
        let skipped = criterion_results(dir.path()).await;
        assert_eq!(skipped.len(), 3);
        assert!(
            skipped
                .values()
                .all(|result| *result == CriterionResult::Skipped)
        );
    }
}

#[test]
fn unstructured_cargo_skips_do_not_certify_every_target() {
    let dir = fixture("libtest-json");
    std::fs::write(
        dir.path().join("loom.toml"),
        "[runner.test]\ncommand = 'cargo test -q'\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = 'skip-evidence-fixture'\nversion = '0.0.0'\nedition = '2024'\n",
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        r#"
#[test]
fn executed() { assert!(std::path::Path::new("Cargo.toml").is_file()); }
#[test]
#[ignore = "prerequisite unavailable in protocol fixture"]
fn unavailable() { panic!("must remain skipped"); }
#[test]
#[ignore = "prerequisite unavailable in protocol fixture"]
fn failed() { panic!("must remain skipped"); }
"#,
    )
    .unwrap();
    let output = run_report(dir.path(), "");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("per-target outcomes unavailable"));
    let cache = StatusCache::open(&dir.path().join(".loom/cache.db")).unwrap();
    let rows = cache.read_all().unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|row| row.verdict == Verdict::Skipped));

    let source = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), format!("{source}\n#[test]\nfn actual_failure() {{ panic!(\"failure must dominate skips\"); }}\n")).unwrap();
    let output = run_report(dir.path(), "");
    assert!(
        !output.status.success(),
        "a mixed failed/skipped batch must not become a skip"
    );
    assert!(
        cache
            .read_all()
            .unwrap()
            .iter()
            .all(|row| row.verdict == Verdict::Fail)
    );

    std::fs::write(
        dir.path().join("specs/gate.md"),
        "## Success Criteria\n\n- Evidence for executed [test](executed)\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("loom.toml"),
        "[runner.test]\ncommand = 'cargo test -q executed'\n",
    )
    .unwrap();
    let output = run_report(dir.path(), "");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows = cache.read_all().unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.annotation_target == "executed")
            .unwrap()
            .verdict,
        Verdict::Pass
    );
}

#[test]
fn malformed_junit_fails_without_persisting_partial_passes() {
    let dir = fixture("junit-xml");
    let malformed = report("junit-xml", [Verdict::Pass; 3]).replace("</testsuite>", "");
    let output = run_report(dir.path(), &malformed);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid junit-xml"), "{stderr}");
    let cache = StatusCache::open(&dir.path().join(".loom/cache.db")).unwrap();
    assert!(cache.read_all().unwrap().is_empty());
}

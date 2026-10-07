//! Real CLI and dispatch through Wrix's runner seam; only the producer is synthetic.

#![allow(
    clippy::unwrap_used,
    reason = "fallible fixture setup and subprocess helpers abort the test when its isolated environment cannot be constructed"
)]

use std::path::Path;
use std::process::{Command, Output};

use loom_gate::cache::{StatusCache, Verdict};
use loom_gate::gate_outcome::{GateRunStatus, VerifiedScope, parse_gate_runs_from_jsonl};
use loom_gate::runner::result::current_platform;
use serde_json::{Value, json};

fn fixture(policy: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("specs")).unwrap();
    std::fs::write(
        dir.path().join("specs/gate.md"),
        "## Success Criteria\n\n- First [check](verify:a)\n- Second [check](verify:b)\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("producer.sh"),
        include_str!("../../../tests/fixtures/verifier/sandbox-results.sh"),
    )
    .unwrap();
    let policy = if policy {
        "skip_policy = 'sandbox-capability'\nskip_capabilities = ['container-runtime', 'kvm']\n"
    } else {
        ""
    };
    std::fs::write(dir.path().join("loom.toml"), format!("[runner.check.wrix]\nmatch = '^verify:(.+)$'\ncommand = 'bash producer.sh {{targets}}'\ntarget = '{{capture_1}}'\njoin = ' '\nparse = 'json-lines'\ncwd = '.'\n{policy}")).unwrap();
    std::fs::write(dir.path().join(".gitignore"), ".loom/\nreceived-targets\n").unwrap();
    for args in [
        ["init", "-q"].as_slice(),
        &["add", "."],
        &["commit", "-qm", "fixture"],
    ] {
        let mut command = Command::new("git");
        loom_test_support::scrub_git_local_env(&mut command);
        let output = command
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

fn record(target: &str, outcome: &str) -> Value {
    json!({"target":target, "outcome":outcome, "evidence":"producer evidence", "execution":{"platform":current_platform(),"platforms":[],"capabilities":[]}})
}

fn foreign(target: &str) -> Value {
    let mut result = record(target, "skipped");
    result["execution"]["platforms"] = json!([if current_platform().ends_with("-darwin") {
        "x86_64-linux"
    } else {
        "aarch64-darwin"
    }]);
    result["skip_reason"] =
        json!({"kind":"foreign-platform","reason":"target requires a different operating system"});
    result
}

fn capability(target: &str, name: &str) -> Value {
    let mut result = record(target, "skipped");
    result["execution"]["capabilities"] = json!([name]);
    result["skip_reason"] = json!({"kind":"missing-capability","capability":name,"reason":"runtime capability unavailable in sandbox"});
    result
}

fn set_record(dir: &Path, target: &str, result: &Value, code: i32) {
    std::fs::write(dir.join(format!("{target}.json")), format!("{result}\n")).unwrap();
    std::fs::write(dir.join(format!("{target}.exit")), code.to_string()).unwrap();
}

fn run(dir: &Path, tier: &str, worker: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_loom"));
    command.args(["--workspace", dir.to_str().unwrap(), "gate", tier, "--tree"]);
    if worker {
        command.env("LOOM_INSIDE", "1");
    } else {
        command.env_remove("LOOM_INSIDE");
    }
    command.output().unwrap()
}

fn rows(dir: &Path) -> std::collections::BTreeMap<String, loom_gate::cache::CacheRow> {
    StatusCache::open(&dir.join(".loom/cache.db"))
        .unwrap()
        .read_all()
        .unwrap()
        .into_iter()
        .map(|row| (row.annotation_target.clone(), row))
        .collect()
}

#[test]
fn synthetic_wrix_producer_aggregates_failures_before_skips_before_passes() {
    let dir = fixture(true);
    for (a, b, expected) in [(0, 0, 0), (0, 77, 77), (77, 0, 77), (1, 77, 1), (77, 1, 1)] {
        set_record(
            dir.path(),
            "a",
            &json!({"target":"a","pass":a == 0,"evidence":"fixture"}),
            a,
        );
        set_record(
            dir.path(),
            "b",
            &json!({"target":"b","pass":b == 0,"evidence":"fixture"}),
            b,
        );
        let output = Command::new("bash")
            .current_dir(dir.path())
            .args(["producer.sh", "a", "b"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(expected));
        assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 2);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("received-targets")).unwrap(),
            "a\nb\n"
        );
    }
}

#[test]
fn sandbox_passes_and_real_failures_remain_individual_outcomes() {
    let dir = fixture(true);
    set_record(dir.path(), "a", &record("a", "passed"), 0);
    set_record(dir.path(), "b", &record("b", "passed"), 0);
    assert!(run(dir.path(), "check", true).status.success());
    set_record(dir.path(), "b", &record("b", "failed"), 1);
    assert_eq!(run(dir.path(), "check", true).status.code(), Some(1));
    let rows = rows(dir.path());
    assert_eq!(rows["verify:a"].verdict, Verdict::Pass);
    assert_eq!(rows["verify:b"].verdict, Verdict::Fail);
}

#[test]
fn worker_accepts_foreign_platform_and_declared_capability_skips_without_verifying_them() {
    for skip in [
        foreign("b"),
        capability("b", "container-runtime"),
        capability("b", "kvm"),
    ] {
        let dir = fixture(true);
        set_record(dir.path(), "a", &record("a", "passed"), 0);
        set_record(dir.path(), "b", &skip, 77);
        let execution = run(dir.path(), "check", true);
        assert_eq!(
            execution.status.code(),
            Some(77),
            "{}",
            String::from_utf8_lossy(&execution.stderr)
        );
        let accepted = run(dir.path(), "verify", true);
        assert!(
            accepted.status.success(),
            "{}",
            String::from_utf8_lossy(&accepted.stderr)
        );
        let stderr = String::from_utf8_lossy(&accepted.stderr);
        assert!(stderr.contains("verify:b"));
        assert!(stderr.contains("coverage remains unverified"));
        let cached = rows(dir.path());
        assert_eq!(cached["verify:a"].verdict, Verdict::Pass);
        assert_eq!(cached["verify:b"].verdict, Verdict::Skipped);
        let evidence: Value = serde_json::from_str(&cached["verify:b"].evidence).unwrap();
        assert_eq!(evidence["pass"], false);
        assert_eq!(evidence["execution"], skip["execution"]);
        assert_eq!(evidence["skip_reason"], skip["skip_reason"]);
        let report =
            std::fs::read_to_string(dir.path().join(".loom/logs/gate/verifier-results.jsonl"))
                .unwrap();
        let reported = report
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|line| line["target"] == "verify:b")
            .unwrap();
        assert_eq!(reported["coverage_verified"], false);
        let runs =
            parse_gate_runs_from_jsonl(&dir.path().join(".loom/logs/gate/worker-acceptance.jsonl"));
        assert_ne!(runs, [] as [loom_gate::GateRun; 0]);
        assert!(
            runs.iter()
                .all(|run| run.status == GateRunStatus::AcceptedWithSkips
                    && VerifiedScope::from_run(run).is_none())
        );
        assert!(!dir.path().join(".loom/marker.json").exists());
        assert!(
            !Command::new(env!("CARGO_BIN_EXE_loom"))
                .args([
                    "--workspace",
                    dir.path().to_str().unwrap(),
                    "gate",
                    "verify-marker"
                ])
                .output()
                .unwrap()
                .status
                .success()
        );
        assert_eq!(
            run(dir.path(), "verify", false).status.code(),
            Some(77),
            "host does not accept worker skip coverage"
        );
    }
}

#[test]
fn all_skipped_worker_batches_remain_unverified_coverage() {
    let dir = fixture(true);
    set_record(dir.path(), "a", &foreign("a"), 77);
    set_record(dir.path(), "b", &capability("b", "kvm"), 77);
    assert_eq!(run(dir.path(), "check", true).status.code(), Some(77));
    assert!(run(dir.path(), "verify", true).status.success());
    assert!(
        rows(dir.path())
            .values()
            .all(|row| row.verdict == Verdict::Skipped)
    );
    assert!(!dir.path().join(".loom/marker.json").exists());
}

#[test]
fn unexpected_skips_cannot_be_accepted_by_worker_policy() {
    let mut undeclared = capability("b", "kvm");
    undeclared["execution"]["capabilities"] = json!([]);
    let mut wrong_platform = capability("b", "kvm");
    wrong_platform["execution"]["platform"] = json!("invented-platform");
    let mut applicable = foreign("b");
    applicable["execution"]["platforms"] = json!([current_platform()]);
    for (policy, skip) in [
        (false, foreign("b")),
        (true, capability("b", "arbitrary-resource")),
        (true, undeclared),
        (true, wrong_platform),
        (true, applicable),
        (
            true,
            json!({"target":"b","pass":false,"skipped":true,"evidence":"missing KVM"}),
        ),
    ] {
        let dir = fixture(policy);
        set_record(dir.path(), "a", &record("a", "passed"), 0);
        set_record(dir.path(), "b", &skip, 77);
        assert_eq!(
            run(dir.path(), "verify", true).status.code(),
            Some(1),
            "{skip}"
        );
        assert_eq!(rows(dir.path())["verify:b"].verdict, Verdict::Skipped);
    }
}

#[test]
fn failure_skip_batches_preserve_failures_even_when_producer_exits_77() {
    for code in [1, 77] {
        let dir = fixture(true);
        set_record(dir.path(), "a", &record("a", "failed"), 1);
        set_record(dir.path(), "b", &foreign("b"), 77);
        std::fs::write(dir.path().join("producer-exit"), code.to_string()).unwrap();
        assert_eq!(run(dir.path(), "verify", true).status.code(), Some(1));
        let rows = rows(dir.path());
        assert_eq!(rows["verify:a"].verdict, Verdict::Fail);
        assert_eq!(rows["verify:b"].verdict, Verdict::Skipped);
    }
}

#[test]
fn invalid_json_batches_fail_closed_without_partial_pass_cache_entries() {
    for malformed in [
        "{\"target\":\"a\",\"pass\":false\n",
        "{\"target\":\"a\",\"pass\":false,\"evidence\":\"failure\"}\n{\"target\":\"a\",\"pass\":true,\"evidence\":\"overwrite\"}\n",
        "{\"target\":\"a\",\"pass\":true,\"skipped\":true,\"evidence\":\"conflict\"}\n",
        "{\"target\":\"a\",\"pass\":true}\n",
        "{\"target\":\"a\",\"pass\":true,\"skipped\":null,\"evidence\":\"malformed\"}\n",
        "{\"target\":\"a\",\"pass\":false,\"pass\":true,\"evidence\":\"duplicate field\"}\n",
        "{\"target\":\"unrequested\",\"pass\":true,\"evidence\":\"bad identity\"}\n",
    ] {
        let dir = fixture(true);
        set_record(dir.path(), "a", &record("a", "passed"), 0);
        set_record(dir.path(), "b", &record("b", "passed"), 0);
        std::fs::write(dir.path().join("a.json"), malformed).unwrap();
        let output = run(dir.path(), "check", true);
        assert_eq!(output.status.code(), Some(1), "{malformed}");
        assert!(rows(dir.path()).is_empty());
    }
}

#[test]
fn missing_target_results_are_dispatch_errors() {
    let dir = fixture(true);
    set_record(dir.path(), "a", &record("a", "passed"), 0);
    set_record(dir.path(), "b", &record("b", "passed"), 0);
    std::fs::write(dir.path().join("b.json"), "").unwrap();
    let output = run(dir.path(), "check", true);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("did not report a verdict"));
}

#[test]
fn producer_failure_and_zero_exit_skips_never_create_verified_cache_evidence() {
    for (b, code, expected_b) in [
        (record("b", "passed"), 1, Verdict::Fail),
        (foreign("b"), 0, Verdict::Skipped),
    ] {
        let dir = fixture(true);
        set_record(dir.path(), "a", &record("a", "passed"), 0);
        set_record(dir.path(), "b", &b, 0);
        std::fs::write(dir.path().join("producer-exit"), code.to_string()).unwrap();
        assert_eq!(run(dir.path(), "verify", true).status.code(), Some(1));
        let rows = rows(dir.path());
        assert_eq!(rows["verify:a"].verdict, Verdict::Fail);
        assert_eq!(rows["verify:b"].verdict, expected_b);
    }
}

#[test]
fn legacy_boolean_failures_never_become_capability_skips_from_evidence_text() {
    let dir = fixture(true);
    set_record(
        dir.path(),
        "a",
        &json!({"target":"a","pass":true,"evidence":"ok"}),
        0,
    );
    set_record(
        dir.path(),
        "b",
        &json!({"target":"b","pass":false,"evidence":"skipped unavailable KVM"}),
        1,
    );
    assert_eq!(run(dir.path(), "check", true).status.code(), Some(1));
    let rows = rows(dir.path());
    assert_eq!(rows["verify:a"].verdict, Verdict::Pass);
    assert_eq!(rows["verify:b"].verdict, Verdict::Fail);
}

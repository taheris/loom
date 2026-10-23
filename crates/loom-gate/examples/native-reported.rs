//! Genuine ordinary-CLI opaque report and permission regressions in the isolated kernel.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    reason = "fixed isolated regression harness fails closed on invalid setup and emits its complete case ledger"
)]

use std::path::Path;

use loom_gate::cache::{StatusCache, Verdict};
use serde_json::{Value, json};

#[path = "../../loom/tests/common/reported.rs"]
mod reported;

fn result(target: &str, outcome: &str) -> Value {
    json!({"target":target,"outcome":outcome,"evidence":"actual immutable producer", "execution":{"platform":loom_gate::runner::result::current_platform(),"platforms":[],"capabilities":[]}})
}

fn capability(target: &str, capability: &str) -> Value {
    let mut value = result(target, "skipped");
    value["execution"]["capabilities"] = json!([capability]);
    value["skip_reason"] = json!({"kind":"missing-capability","capability":capability,"reason":"declared native prerequisite unavailable"});
    value
}

fn put(fixture: &reported::Fixture, target: &str, value: &Value, code: i32) {
    fixture.record(target, &format!("{value}\n"), code);
}

fn rows(fixture: &reported::Fixture) -> Vec<loom_gate::cache::CacheRow> {
    StatusCache::open(&fixture.path().join(".loom/state.db"))
        .unwrap()
        .read_all()
        .unwrap()
}

fn run(fixture: &reported::Fixture, binary: &Path, tier: &str, expected: i32) -> String {
    let output = fixture.run(binary, tier, true);
    let diagnostic = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(expected), "{diagnostic}");
    diagnostic
}

fn witness(diagnostic: &str) -> String {
    let witnesses: std::collections::BTreeSet<_> = diagnostic
        .lines()
        .filter_map(|line| {
            line.strip_prefix("loom gate [")
                .and_then(|line| line.split_once("]: "))
        })
        .filter_map(|(_, data)| serde_json::from_str::<Value>(data).ok())
        .flat_map(|value| {
            value["observations"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .filter(|value| value["kind"] == "fresh-execution")
        .map(|value| value["derivation"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(witnesses.len(), 1, "{diagnostic}");
    witnesses.into_iter().next().unwrap()
}

fn main() {
    let binary = std::env::args_os()
        .nth(1)
        .expect("expected immutable CLI artifact");
    let binary = Path::new(&binary);
    let fixture = reported::Fixture::new("system", "json-lines", &["a", "b"]);
    fixture.policy("system", true);
    put(&fixture, "a", &result("a", "passed"), 0);
    put(&fixture, "b", &result("b", "passed"), 0);
    let first = witness(&run(&fixture, binary, "system", 0));
    let second = witness(&run(&fixture, binary, "system", 0));
    assert_ne!(first, second, "a later invocation reused a verdict");
    println!("{}", json!({"phase":"reported-fresh-runs"}));

    put(&fixture, "b", &result("b", "failed"), 1);
    run(&fixture, binary, "system", 1);
    let cached = rows(&fixture);
    assert!(
        cached.iter().any(
            |row| row.annotation_target == fixture.address("a") && row.verdict == Verdict::Pass
        )
    );
    assert!(
        cached.iter().any(
            |row| row.annotation_target == fixture.address("b") && row.verdict == Verdict::Fail
        )
    );
    println!("{}", json!({"phase":"reported-mixed-results"}));

    for capability_name in ["container-runtime", "kvm"] {
        put(&fixture, "b", &capability("b", capability_name), 77);
        run(&fixture, binary, "system", 77);
        run(&fixture, binary, "verify", 0);
        let cached = rows(&fixture);
        let skipped = cached
            .iter()
            .find(|row| row.annotation_target == fixture.address("b"))
            .unwrap();
        assert_eq!(skipped.verdict, Verdict::Skipped);
        let evidence: Value = serde_json::from_str(&skipped.evidence).unwrap();
        assert_eq!(
            evidence["execution"]["capabilities"],
            json!([capability_name])
        );
        assert_eq!(evidence["skip_reason"]["capability"], capability_name);
        assert!(!fixture.path().join(".loom/marker.json").exists());
    }
    println!("{}", json!({"phase":"reported-permitted-skips"}));
    fixture.policy("system", false);
    run(&fixture, binary, "system", 78);
    fixture.policy("system", true);
    let mut wrong_platform = capability("b", "kvm");
    wrong_platform["execution"]["platform"] = json!("invalid-actual-platform");
    put(&fixture, "b", &wrong_platform, 77);
    run(&fixture, binary, "system", 1);
    println!("{}", json!({"phase":"reported-denied-skips"}));

    let broken = reported::Fixture::new("check", "json-lines", &["a", "b"]);
    put(&broken, "a", &result("a", "passed"), 0);
    broken.record("b", "", 0);
    let diagnostic = run(&broken, binary, "check", 1);
    assert!(
        diagnostic.contains(&format!("ERROR: {}", broken.address("a"))),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains(&format!("ERROR: {}", broken.address("b"))),
        "{diagnostic}"
    );
    assert_eq!(rows(&broken).len(), 0);
    println!("{}", json!({"phase":"reported-missing-result-atomicity"}));

    let contradictory = reported::Fixture::new("check", "json-lines", &["a", "b"]);
    contradictory.policy("check", true);
    put(&contradictory, "a", &result("a", "passed"), 0);
    put(&contradictory, "b", &capability("b", "kvm"), 0);
    run(&contradictory, binary, "check", 1);
    assert!(
        rows(&contradictory)
            .iter()
            .all(|row| row.verdict != Verdict::Pass)
    );
    println!("{}", json!({"phase":"reported-status-contradiction"}));

    let xml = reported::Fixture::new("check", "junit-xml", &["suite.a", "suite.b"]);
    xml.record("suite.a", "<testsuite><testcase classname=\"suite\" name=\"a\"/><testcase classname=\"suite\" name=\"b\"><skipped message=\"prerequisite absent\"/></testcase></testsuite>", 77);
    xml.record("suite.b", "", 0);
    run(&xml, binary, "check", 78);
    let cached = rows(&xml);
    assert!(
        cached
            .iter()
            .any(|row| row.annotation_target == xml.address("suite.a")
                && row.verdict == Verdict::Pass)
    );
    assert!(
        cached
            .iter()
            .any(|row| row.annotation_target == xml.address("suite.b")
                && row.verdict == Verdict::Skipped)
    );
    println!("{}", json!({"phase":"reported-junit-results"}));
}

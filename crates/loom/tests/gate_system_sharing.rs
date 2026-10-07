//! Real CLI coverage for shared system execution, reporting and cache fan-out.
#![expect(
    clippy::unwrap_used,
    reason = "isolated integration fixtures must be constructed before exercising the CLI"
)]

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::Command;

use loom_gate::{StatusCache, Verdict};

fn fixture(matched: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("specs")).unwrap();
    let target = if matched {
        "system:shared"
    } else {
        "bash verifier.sh"
    };
    for (label, count) in [("a", 2), ("b", 1), ("c", 1)] {
        let mut body = format!("# {label}\n\n## Success Criteria\n\n");
        for index in 0..count {
            write!(
                body,
                "- Owner {label}-{index} is verified\n  [system]({target})\n"
            )
            .unwrap();
        }
        fs::write(dir.path().join("specs").join(format!("{label}.md")), body).unwrap();
    }
    if matched {
        fs::write(dir.path().join("loom.toml"), "[runner.system.shared]\nmatch = '^system:shared$'\ncommand = 'bash verifier.sh'\ntarget = 'shared'\nparse = 'json-lines'\n").unwrap();
    }
    fs::write(dir.path().join("verifier.sh"), r#"set -euo pipefail
printf 'spawn\n' >> spawns
if [[ "$MATCHED" == 1 ]]; then
    prefix='"target":"shared",'
else
    prefix=''
fi
case "$RESULT_MODE" in
    pass) printf '{%s"pass":true,"evidence":"executed"}\n' "$prefix" ;;
    fail) printf '{%s"pass":false,"evidence":"assertion failed"}\n' "$prefix"; exit 1 ;;
    skip) printf '{%s"pass":false,"skipped":true,"evidence":"prerequisite missing"}\n' "$prefix"; exit 77 ;;
    invalid) printf '{%s"pass":"wrong type","evidence":"invalid"}\n' "$prefix" ;;
esac
"#).unwrap();
    let mut git = Command::new("git");
    loom_test_support::scrub_git_local_env(&mut git);
    assert!(
        git.current_dir(dir.path())
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success()
    );
    dir
}

fn run(workspace: &Path, matched: bool, mode: &str, scope: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_loom"))
        .arg("--workspace")
        .arg(workspace)
        .args(["gate", "system"])
        .args(scope)
        .env("MATCHED", if matched { "1" } else { "0" })
        .env("RESULT_MODE", mode)
        .output()
        .unwrap()
}

#[test]
fn system_cli_sharing_preserves_per_criterion_reports_failures_skips_and_cache_records() {
    for matched in [false, true] {
        for (mode, exit, verdict) in [
            ("pass", 0, Verdict::Pass),
            ("fail", 1, Verdict::Fail),
            ("skip", 1, Verdict::Skipped),
        ] {
            let dir = fixture(matched);
            let output = run(dir.path(), matched, mode, &["--tree"]);
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert_eq!(output.status.code(), Some(exit), "{stderr}");
            assert_eq!(
                fs::read_to_string(dir.path().join("spawns"))
                    .unwrap()
                    .lines()
                    .count(),
                1
            );
            let rows = StatusCache::open(&dir.path().join(".loom/cache.db"))
                .unwrap()
                .read_all()
                .unwrap();
            assert_eq!(
                rows.len(),
                4,
                "all owners, including same-spec siblings, need evidence"
            );
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
            let records =
                fs::read_to_string(dir.path().join(".loom/logs/gate/verifier-results.jsonl"))
                    .unwrap();
            let reports = records
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(reports.len(), 4);
            assert!(
                reports
                    .iter()
                    .all(|record| record["outcome"] == verdict.as_wire()
                        && record["coverage_verified"] == (verdict == Verdict::Pass))
            );
            if mode != "pass" {
                assert_eq!(
                    stderr
                        .matches(if mode == "skip" {
                            "SKIP (coverage unverified"
                        } else {
                            "FAIL:"
                        })
                        .count(),
                    4,
                    "{stderr}"
                );
            }
            assert!(!dir.path().join(".loom/marker.json").exists());
        }
    }
}

#[test]
fn system_cli_shared_dispatch_errors_report_every_owner_without_passing_cache_entries() {
    for matched in [false, true] {
        let dir = fixture(matched);
        let output = run(dir.path(), matched, "invalid", &["--tree"]);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            String::from_utf8(output.stderr)
                .unwrap()
                .matches("dispatch error:")
                .count(),
            4
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("spawns"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert_eq!(
            StatusCache::open(&dir.path().join(".loom/cache.db"))
                .unwrap()
                .read_all()
                .unwrap(),
            [] as [loom_gate::CacheRow; 0]
        );
    }
}

#[test]
fn system_cli_exact_shared_target_executes_again_on_a_later_gate_run() {
    let dir = fixture(false);
    for count in 1..=2 {
        let output = run(dir.path(), false, "pass", &["--target", "bash verifier.sh"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("spawns"))
                .unwrap()
                .lines()
                .count(),
            count
        );
        assert_eq!(
            StatusCache::open(&dir.path().join(".loom/cache.db"))
                .unwrap()
                .read_all()
                .unwrap()
                .len(),
            4
        );
    }
}

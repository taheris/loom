#![allow(clippy::unwrap_used)]
//! Integration coverage for the per-tier dispatcher.
//!
//! Exercises end-to-end paths the gate hits when `loom gate verify`
//! runs: real subprocess spawn, env-var contract (`LOOM_FILES`,
//! `LOOM_SPEC`), JSON-line verdict parsing, batched `[test]` runner
//! invocation with `--files` scope filtering, and `[judge]` batching.
//! The inline tests in `src/dispatch.rs` cover unit-level concerns
//! (verdict parser corner cases, filter logic, error formatting);
//! these tests cover the seam to a real subprocess.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use loom_gate::annotation::{Annotation, Tier};
use loom_gate::dispatch::{
    DispatchError, DispatchOptions, EmptyScope, TestScope, TierCwds, run_check, run_judge,
    run_system, run_test, run_with_runners,
};
use loom_gate::runner::{BuiltinParser, RunnerSpec, RunnerTemplate};
use tempfile::TempDir;

fn ann(tier: Tier, target: &str) -> Annotation {
    Annotation {
        tier,
        target: target.into(),
        source_spec: PathBuf::from("specs/a.md"),
        line: 1,
        criterion_line: 1,
        pending: false,
    }
}

fn pending_ann(tier: Tier, target: &str) -> Annotation {
    Annotation {
        pending: true,
        ..ann(tier, target)
    }
}

/// Write a shell-script body to `dir/name` and return an annotation
/// target that invokes it via `sh <path>`. Routing through `sh` skips
/// the chmod race that produces `ETXTBSY` on freshly-written
/// executables and keeps the fixture portable across hosts.
fn write_script(dir: &Path, name: &str, body: &str) -> String {
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    format!("sh {}", path.display())
}

struct StubScope {
    map: HashMap<String, Vec<PathBuf>>,
}

impl StubScope {
    fn new(entries: &[(&str, &[&str])]) -> Self {
        let map = entries
            .iter()
            .map(|(t, fs)| {
                (
                    (*t).to_string(),
                    fs.iter().map(PathBuf::from).collect::<Vec<_>>(),
                )
            })
            .collect();
        Self { map }
    }
}

impl TestScope for StubScope {
    fn scope_for(&self, a: &Annotation) -> Vec<PathBuf> {
        self.map.get(&a.target).cloned().unwrap_or_default()
    }
}

fn fixture_dir() -> TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn dispatcher_spawns_one_subprocess_per_unmatched_check_annotation() {
    let dir = fixture_dir();
    let counter = dir.path().join("spawns.txt");
    fs::write(&counter, "").unwrap();
    let counter_path = counter.display();
    let pass_script = write_script(
        dir.path(),
        "a.sh",
        &format!(
            "#!/bin/sh\nprintf 'x\\n' >> \"{counter_path}\"\nprintf '{{\"pass\": true, \"evidence\": \"a-ok\"}}\\n'\n"
        ),
    );
    let fail_script = write_script(
        dir.path(),
        "b.sh",
        &format!(
            "#!/bin/sh\nprintf 'x\\n' >> \"{counter_path}\"\nprintf '{{\"pass\": false, \"evidence\": \"b-fail\"}}\\n'\nexit 1\n"
        ),
    );

    let inputs = vec![
        ann(Tier::Check, &pass_script),
        ann(Tier::Check, &fail_script),
    ];
    let opts = DispatchOptions::default();
    let results = run_check(&inputs, &[], &opts, dir.path(), &TierCwds::default());
    assert_eq!(results.len(), 2);

    let first = results[0].as_ref().unwrap();
    assert!(first.verdict.pass);
    assert_eq!(first.verdict.evidence, "a-ok");

    let second = results[1].as_ref().unwrap();
    assert!(!second.verdict.pass);
    assert_eq!(second.verdict.evidence, "b-fail");

    let observed_subprocess_count = fs::read_to_string(&counter).unwrap().lines().count();
    assert_eq!(
        observed_subprocess_count, 2,
        "one fallback subprocess spawned per unmatched [check] annotation",
    );
}

#[test]
fn dispatcher_spawns_one_subprocess_per_system_annotation() {
    let dir = fixture_dir();
    let script = write_script(
        dir.path(),
        "system.sh",
        "#!/bin/sh\nprintf '{\"pass\": true, \"evidence\": \"system-ok\"}\\n'\n",
    );

    let inputs = vec![ann(Tier::System, &script)];
    let opts = DispatchOptions::default();
    let results = run_system(&inputs, &[], &opts, dir.path(), &TierCwds::default());
    assert_eq!(results.len(), 1);
    assert!(results[0].as_ref().unwrap().verdict.pass);
}

#[test]
fn dispatcher_sets_loom_files_and_loom_spec_env_on_verifier_subprocess() {
    let dir = fixture_dir();
    let script = write_script(
        dir.path(),
        "env.sh",
        "#!/bin/sh\nprintf '{\"pass\": true, \"evidence\": \"FILES=%s|SPEC=%s\"}\\n' \"$LOOM_FILES\" \"$LOOM_SPEC\"\n",
    );

    let inputs = vec![ann(Tier::Check, &script)];
    let opts = DispatchOptions {
        files: vec![PathBuf::from("src/lib.rs"), PathBuf::from("src/main.rs")],
        spec: Some("tests".into()),
    };
    let results = run_check(&inputs, &[], &opts, dir.path(), &TierCwds::default());
    let outcome = results.into_iter().next().unwrap().unwrap();
    assert_eq!(
        outcome.verdict.evidence,
        "FILES=src/lib.rs:src/main.rs|SPEC=tests"
    );
}

#[test]
fn check_tier_falls_back_to_exit_code_pass_when_verifier_omits_json() {
    let dir = fixture_dir();
    let script = write_script(
        dir.path(),
        "noverdict.sh",
        "#!/bin/sh\necho some informational message\nexit 0\n",
    );

    let inputs = vec![ann(Tier::Check, &script)];
    let opts = DispatchOptions::default();
    let results = run_check(&inputs, &[], &opts, dir.path(), &TierCwds::default());
    let outcome = results.into_iter().next().unwrap().unwrap();
    assert!(
        outcome.verdict.pass,
        "exit 0 with no JSON line interprets as pass (matches batched-tier fallback)"
    );
    assert!(
        outcome
            .verdict
            .evidence
            .contains("some informational message"),
        "stdout surfaced as evidence on the pass path"
    );
}

#[test]
fn system_tier_exit_77_classifies_as_skipped_not_failed() {
    let dir = fixture_dir();
    let script = write_script(
        dir.path(),
        "skip.sh",
        "#!/bin/sh\necho 'skip: prerequisite missing' >&2\nexit 77\n",
    );

    let inputs = vec![ann(Tier::System, &script)];
    let opts = DispatchOptions::default();
    let results = run_system(&inputs, &[], &opts, dir.path(), &TierCwds::default());
    let outcome = results.into_iter().next().unwrap().unwrap();
    assert!(
        outcome.verdict.skipped,
        "exit 77 should surface the skipped verdict"
    );
    assert!(
        !outcome.verdict.pass,
        "skipped verdict does not also report pass"
    );
    assert!(
        outcome.verdict.evidence.contains("prerequisite missing"),
        "stderr surfaced as evidence on the skip path: {}",
        outcome.verdict.evidence,
    );
}

#[test]
fn check_tier_falls_back_to_exit_code_fail_when_verifier_omits_json() {
    let dir = fixture_dir();
    let script = write_script(
        dir.path(),
        "noverdict-fail.sh",
        "#!/bin/sh\necho informational >&1\necho the actual diagnostic >&2\nexit 1\n",
    );

    let inputs = vec![ann(Tier::Check, &script)];
    let opts = DispatchOptions::default();
    let results = run_check(&inputs, &[], &opts, dir.path(), &TierCwds::default());
    let outcome = results.into_iter().next().unwrap().unwrap();
    assert!(!outcome.verdict.pass, "non-zero exit interprets as fail");
    assert!(
        outcome.verdict.evidence.contains("the actual diagnostic"),
        "stderr surfaced as evidence on the fail path"
    );
}

#[test]
fn dispatcher_surfaces_malformed_verdict_when_pass_key_has_wrong_type() {
    let dir = fixture_dir();
    let script = write_script(
        dir.path(),
        "bad.sh",
        "#!/bin/sh\nprintf '{\"pass\": \"yes\", \"evidence\": \"oops\"}\\n'\nexit 0\n",
    );

    let inputs = vec![ann(Tier::Check, &script)];
    let opts = DispatchOptions::default();
    let results = run_check(&inputs, &[], &opts, dir.path(), &TierCwds::default());
    let err = results.into_iter().next().unwrap().unwrap_err();
    assert!(matches!(err, DispatchError::MalformedVerdict { .. }));
}

#[test]
fn dispatcher_falls_through_to_exit_code_on_incidental_json() {
    let dir = fixture_dir();
    let script = write_script(
        dir.path(),
        "incidental.sh",
        "#!/bin/sh\nprintf '{\"some\":\"data\"}\\n'\nexit 0\n",
    );

    let inputs = vec![ann(Tier::Check, &script)];
    let opts = DispatchOptions::default();
    let results = run_check(&inputs, &[], &opts, dir.path(), &TierCwds::default());
    let outcome = results.into_iter().next().unwrap().unwrap();
    assert!(outcome.verdict.pass);
}

#[test]
fn test_tier_batches_all_targets_into_one_runner_subprocess() {
    let dir = fixture_dir();
    let runner = write_script(
        dir.path(),
        "runner.sh",
        "#!/bin/sh\nprintf '{\"pass\": true, \"evidence\": \"argv=%s\"}\\n' \"$*\"\n",
    );

    let template = RunnerTemplate::new(format!("{runner} {{paths}}"));
    let inputs = vec![
        ann(Tier::Test, "crate::a::one"),
        ann(Tier::Test, "crate::b::two"),
        ann(Tier::Test, "crate::c::three"),
    ];
    let opts = DispatchOptions::default();
    let outcome = run_test(&inputs, &opts, &template, &EmptyScope)
        .unwrap()
        .unwrap();
    assert_eq!(
        outcome.annotations.len(),
        3,
        "single batched call covers all"
    );
    assert!(outcome.verdict.pass);
    assert!(outcome.verdict.evidence.contains("crate::a::one"));
    assert!(outcome.verdict.evidence.contains("crate::b::two"));
    assert!(outcome.verdict.evidence.contains("crate::c::three"));
}

#[test]
fn run_test_filters_targets_by_files_scope_intersection() {
    let dir = fixture_dir();
    let runner = write_script(
        dir.path(),
        "scopecheck.sh",
        "#!/bin/sh\nprintf '{\"pass\": true, \"evidence\": \"argv=%s\"}\\n' \"$*\"\n",
    );
    let template = RunnerTemplate::new(format!("{runner} {{paths}}"));

    let inputs = vec![
        ann(Tier::Test, "crate::a::keep"),
        ann(Tier::Test, "crate::b::drop"),
        ann(Tier::Test, "crate::c::keep"),
    ];
    let scope = StubScope::new(&[
        ("crate::a::keep", &["src/a.rs"]),
        ("crate::b::drop", &["src/b.rs"]),
        ("crate::c::keep", &["src/a.rs", "src/c.rs"]),
    ]);
    let opts = DispatchOptions {
        files: vec![PathBuf::from("src/a.rs")],
        spec: None,
    };
    let outcome = run_test(&inputs, &opts, &template, &scope)
        .unwrap()
        .unwrap();
    let kept: Vec<&str> = outcome
        .annotations
        .iter()
        .map(|a| a.target.as_str())
        .collect();
    assert_eq!(kept, vec!["crate::a::keep", "crate::c::keep"]);
    assert!(outcome.verdict.evidence.contains("crate::a::keep"));
    assert!(outcome.verdict.evidence.contains("crate::c::keep"));
    assert!(!outcome.verdict.evidence.contains("crate::b::drop"));
}

#[test]
fn test_tier_returns_none_when_files_filter_excludes_everything() {
    let template = RunnerTemplate::new("/nonexistent {paths}");
    let inputs = vec![ann(Tier::Test, "crate::a::ok")];
    let scope = StubScope::new(&[("crate::a::ok", &["src/a.rs"])]);
    let opts = DispatchOptions {
        files: vec![PathBuf::from("src/b.rs")],
        spec: None,
    };
    assert!(
        run_test(&inputs, &opts, &template, &scope)
            .unwrap()
            .is_none(),
        "no scope match → no subprocess spawned, returns None"
    );
}

#[test]
fn test_tier_returns_none_when_no_test_annotations_in_input() {
    let template = RunnerTemplate::new("/nonexistent {paths}");
    let inputs = vec![ann(Tier::Check, "x"), ann(Tier::System, "y")];
    let opts = DispatchOptions::default();
    assert!(
        run_test(&inputs, &opts, &template, &EmptyScope)
            .unwrap()
            .is_none()
    );
}

#[test]
fn test_tier_falls_back_to_exit_code_when_runner_omits_json_line() {
    let dir = fixture_dir();
    let runner = write_script(
        dir.path(),
        "exitcode.sh",
        "#!/bin/sh\necho running tests\nexit 0\n",
    );
    let template = RunnerTemplate::new(format!("{runner} {{paths}}"));

    let inputs = vec![ann(Tier::Test, "crate::a::ok")];
    let opts = DispatchOptions::default();
    let outcome = run_test(&inputs, &opts, &template, &EmptyScope)
        .unwrap()
        .unwrap();
    assert!(
        outcome.verdict.pass,
        "exit 0 with no JSON line interprets as pass"
    );
}

#[test]
fn judge_tier_batches_all_targets_into_one_runner_subprocess() {
    let dir = fixture_dir();
    let runner = write_script(
        dir.path(),
        "judge.sh",
        "#!/bin/sh\nprintf '{\"pass\": true, \"evidence\": \"argv=%s\"}\\n' \"$*\"\n",
    );
    let template = RunnerTemplate::new(format!("{runner} {{paths}}"));

    let inputs = vec![
        ann(Tier::Judge, "rubrics/a.md"),
        ann(Tier::Judge, "rubrics/b.md"),
    ];
    let opts = DispatchOptions::default();
    let outcome = run_judge(&inputs, &opts, &template).unwrap().unwrap();
    assert_eq!(outcome.annotations.len(), 2);
    assert!(outcome.verdict.pass);
    assert!(outcome.verdict.evidence.contains("rubrics/a.md"));
    assert!(outcome.verdict.evidence.contains("rubrics/b.md"));
}

#[test]
fn judge_tier_ignores_files_scope_unlike_test_tier() {
    let dir = fixture_dir();
    let runner = write_script(
        dir.path(),
        "judge2.sh",
        "#!/bin/sh\nprintf '{\"pass\": true, \"evidence\": \"argv=%s\"}\\n' \"$*\"\n",
    );
    let template = RunnerTemplate::new(format!("{runner} {{paths}}"));

    let inputs = vec![ann(Tier::Judge, "rubrics/a.md")];
    let opts = DispatchOptions {
        files: vec![PathBuf::from("src/unrelated.rs")],
        spec: None,
    };
    let outcome = run_judge(&inputs, &opts, &template).unwrap().unwrap();
    assert_eq!(
        outcome.annotations.len(),
        1,
        "judges are not filtered by --files scope"
    );
}

/// Per `specs/gate.md` § Pending modifier — *Dispatch-side skip*:
/// `[check?]` annotations are filtered out before subprocess spawn.
/// Plan sessions can author `[check?](cargo run -p not-yet-built)`
/// without breaking their own `loom gate verify` lane. Target points at
/// a path that would emit `DispatchError::Spawn` if dispatched; the test
/// passes when the dispatcher returns an empty result vec.
#[test]
fn dispatcher_skips_check_pending_annotation() {
    let dir = fixture_dir();
    let inputs = vec![pending_ann(
        Tier::Check,
        "/no/such/binary/anywhere-pending-check",
    )];
    let opts = DispatchOptions::default();
    let results = run_check(&inputs, &[], &opts, dir.path(), &TierCwds::default());
    assert!(
        results.is_empty(),
        "[check?] must be skipped at dispatch — got {} result(s): {:?}",
        results.len(),
        results,
    );
}

/// Same contract for `[test?]`: the batched runner subprocess is
/// not spawned. `run_test` returns `Ok(None)` (the same shape it uses
/// for "no test annotations" and "scope-filtered to empty"), letting
/// the caller distinguish "skipped" from "ran-and-failed".
#[test]
fn dispatcher_skips_test_pending_annotation() {
    let template = RunnerTemplate::new("/no/such/runner-pending-test {paths}");
    let inputs = vec![pending_ann(Tier::Test, "crate::not::a::real::pending_test")];
    let opts = DispatchOptions::default();
    let result = run_test(&inputs, &opts, &template, &EmptyScope).unwrap();
    assert!(
        result.is_none(),
        "[test?] must be skipped at dispatch — got {result:?}",
    );
}

/// Same contract for `[system?]`. The dispatcher emits no result
/// for the pending entry; the result vec is empty.
#[test]
fn dispatcher_skips_system_pending_annotation() {
    let dir = fixture_dir();
    let inputs = vec![pending_ann(
        Tier::System,
        "/no/such/binary/anywhere-pending-system",
    )];
    let opts = DispatchOptions::default();
    let results = run_system(&inputs, &[], &opts, dir.path(), &TierCwds::default());
    assert!(
        results.is_empty(),
        "[system?] must be skipped at dispatch — got {} result(s): {:?}",
        results.len(),
        results,
    );
}

/// Same contract for `[judge?]`. The batched judge runner is not
/// spawned; `run_judge` returns `Ok(None)`.
#[test]
fn dispatcher_skips_judge_pending_annotation() {
    let template = RunnerTemplate::new("/no/such/runner-pending-judge {paths}");
    let inputs = vec![pending_ann(Tier::Judge, "rubrics/not-yet-written.md")];
    let opts = DispatchOptions::default();
    let result = run_judge(&inputs, &opts, &template).unwrap();
    assert!(
        result.is_none(),
        "[judge?] must be skipped at dispatch — got {result:?}",
    );
}

/// Regression: the pending filter must not over-fire — non-`?`
/// annotations with resolvable targets still dispatch and surface a
/// verdict. Pairs with `dispatcher_skips_check_pending_annotation`
/// to bound the filter in both directions.
#[test]
fn dispatcher_runs_non_pending_annotation_with_resolvable_target() {
    let dir = fixture_dir();
    let script = write_script(
        dir.path(),
        "non-pending.sh",
        "#!/bin/sh\nprintf '{\"pass\": true, \"evidence\": \"ran\"}\\n'\n",
    );
    let inputs = vec![ann(Tier::Check, &script)];
    let opts = DispatchOptions::default();
    let results = run_check(&inputs, &[], &opts, dir.path(), &TierCwds::default());
    assert_eq!(results.len(), 1, "non-pending [check] must still dispatch");
    let outcome = results.into_iter().next().unwrap().unwrap();
    assert!(outcome.verdict.pass);
    assert_eq!(outcome.verdict.evidence, "ran");
}

#[test]
fn check_tier_skips_annotations_with_non_check_tier() {
    let dir = fixture_dir();
    let script = write_script(
        dir.path(),
        "only-check.sh",
        "#!/bin/sh\nprintf '{\"pass\": true, \"evidence\": \"ok\"}\\n'\n",
    );

    let inputs = vec![
        ann(Tier::Check, &script),
        ann(Tier::Test, "crate::a::ignored"),
        ann(Tier::System, "/nope"),
        ann(Tier::Judge, "rubric"),
    ];
    let opts = DispatchOptions::default();
    let results = run_check(&inputs, &[], &opts, dir.path(), &TierCwds::default());
    assert_eq!(results.len(), 1, "only the [check] annotation dispatched");
}

#[test]
fn dispatcher_surfaces_spawn_failure_when_command_not_found() {
    let dir = fixture_dir();
    let inputs = vec![ann(
        Tier::Check,
        "/definitely/not/a/real/binary/anywhere-12345",
    )];
    let opts = DispatchOptions::default();
    let results = run_check(&inputs, &[], &opts, dir.path(), &TierCwds::default());
    let err = results.into_iter().next().unwrap().unwrap_err();
    assert!(matches!(err, DispatchError::Spawn { .. }));
}

#[test]
fn run_with_runners_groups_matched_into_one_batch_and_falls_back_for_unmatched() {
    let dir = fixture_dir();
    let runner = write_script(
        dir.path(),
        "json-lines.sh",
        "#!/bin/sh\nfor target in \"$@\"; do\n  printf '{\"target\":\"%s\",\"pass\":true,\"evidence\":\"batched\"}\\n' \"$target\"\ndone\n",
    );
    let fallback_script = write_script(
        dir.path(),
        "fallback.sh",
        "#!/bin/sh\nprintf '{\"pass\": true, \"evidence\": \"singleton\"}\\n'\n",
    );

    let spec = RunnerSpec::compile(
        "lines",
        Some(r"^lines::"),
        format!("{runner} {{targets}}"),
        "{name}",
        " ",
        BuiltinParser::JsonLines,
        None,
    )
    .unwrap();
    let inputs = vec![
        ann(Tier::Check, "lines::a"),
        ann(Tier::Check, &fallback_script),
        ann(Tier::Check, "lines::b"),
    ];
    let opts = DispatchOptions::default();
    let results = run_with_runners(&inputs, &[spec], &opts, dir.path(), &TierCwds::default());
    assert_eq!(results.len(), 3);

    let r0 = results[0].as_ref().unwrap();
    assert!(r0.verdict.pass);
    assert_eq!(r0.verdict.evidence, "batched");
    assert_eq!(r0.annotations[0].target, "lines::a");

    let r1 = results[1].as_ref().unwrap();
    assert!(r1.verdict.pass);
    assert_eq!(
        r1.verdict.evidence, "singleton",
        "unmatched annotation flows through run_single fallback"
    );

    let r2 = results[2].as_ref().unwrap();
    assert!(r2.verdict.pass);
    assert_eq!(r2.verdict.evidence, "batched");
    assert_eq!(r2.annotations[0].target, "lines::b");
}

#[test]
fn run_with_runners_first_match_wins_in_spec_order() {
    let dir = fixture_dir();
    let first = write_script(
        dir.path(),
        "first.sh",
        "#!/bin/sh\nfor t in \"$@\"; do printf '{\"target\":\"%s\",\"pass\":true,\"evidence\":\"first\"}\\n' \"$t\"; done\n",
    );
    let second = write_script(
        dir.path(),
        "second.sh",
        "#!/bin/sh\nfor t in \"$@\"; do printf '{\"target\":\"%s\",\"pass\":true,\"evidence\":\"second\"}\\n' \"$t\"; done\n",
    );

    let spec_a = RunnerSpec::compile(
        "first",
        Some(r"^test::"),
        format!("{first} {{targets}}"),
        "{name}",
        " ",
        BuiltinParser::JsonLines,
        None,
    )
    .unwrap();
    let spec_b = RunnerSpec::compile(
        "second",
        None,
        format!("{second} {{targets}}"),
        "{name}",
        " ",
        BuiltinParser::JsonLines,
        None,
    )
    .unwrap();
    let inputs = vec![ann(Tier::Check, "test::shared")];
    let opts = DispatchOptions::default();
    let results = run_with_runners(
        &inputs,
        &[spec_a, spec_b],
        &opts,
        dir.path(),
        &TierCwds::default(),
    );
    assert_eq!(results.len(), 1);
    let outcome = results[0].as_ref().unwrap();
    assert_eq!(
        outcome.verdict.evidence, "first",
        "first declared spec claims a target both specs match"
    );
}

#[test]
fn run_with_runners_dispatch_fails_targets_missing_from_batch_output() {
    let dir = fixture_dir();
    let runner = write_script(
        dir.path(),
        "partial.sh",
        "#!/bin/sh\nfirst=\"$1\"\nprintf '{\"target\":\"%s\",\"pass\":true,\"evidence\":\"ok\"}\\n' \"$first\"\n",
    );
    let spec = RunnerSpec::compile(
        "partial",
        None,
        format!("{runner} {{targets}}"),
        "{name}",
        " ",
        BuiltinParser::JsonLines,
        None,
    )
    .unwrap();
    let inputs = vec![ann(Tier::Check, "covered"), ann(Tier::Check, "missing")];
    let opts = DispatchOptions::default();
    let results = run_with_runners(&inputs, &[spec], &opts, dir.path(), &TierCwds::default());
    assert_eq!(results.len(), 2);
    let covered = results[0].as_ref().unwrap();
    assert!(covered.verdict.pass);
    let err = results[1].as_ref().unwrap_err();
    match err {
        DispatchError::MissingFromBatchOutput { runner, target } => {
            assert_eq!(runner, "partial");
            assert_eq!(target, "missing");
        }
        other => panic!("expected MissingFromBatchOutput, got {other:?}"),
    }
}

#[test]
fn run_with_runners_resolves_cwd_against_repo_root() {
    let dir = fixture_dir();
    let subdir_name = "nested";
    let nested = dir.path().join(subdir_name);
    std::fs::create_dir(&nested).unwrap();
    let probe = write_script(
        dir.path(),
        "pwd-probe.sh",
        "#!/bin/sh\nfor t in \"$@\"; do printf '{\"target\":\"%s\",\"pass\":true,\"evidence\":\"%s\"}\\n' \"$t\" \"$PWD\"; done\n",
    );
    let spec = RunnerSpec::compile(
        "probe",
        None,
        format!("{probe} {{targets}}"),
        "{name}",
        " ",
        BuiltinParser::JsonLines,
        Some(PathBuf::from(subdir_name)),
    )
    .unwrap();
    let inputs = vec![ann(Tier::Check, "x")];
    let opts = DispatchOptions::default();
    let results = run_with_runners(&inputs, &[spec], &opts, dir.path(), &TierCwds::default());
    let outcome = results[0].as_ref().unwrap();
    assert!(
        outcome.verdict.evidence.ends_with(subdir_name),
        "cwd should resolve under {} but got `{}`",
        dir.path().display(),
        outcome.verdict.evidence,
    );
}

#[test]
fn run_with_runners_falls_through_to_tier_default_when_runner_cwd_is_none() {
    let dir = fixture_dir();
    let subdir_name = "tier-default";
    std::fs::create_dir(dir.path().join(subdir_name)).unwrap();
    let probe = write_script(
        dir.path(),
        "tier-probe.sh",
        "#!/bin/sh\nfor t in \"$@\"; do printf '{\"target\":\"%s\",\"pass\":true,\"evidence\":\"%s\"}\\n' \"$t\" \"$PWD\"; done\n",
    );
    let spec = RunnerSpec::compile(
        "probe",
        None,
        format!("{probe} {{targets}}"),
        "{name}",
        " ",
        BuiltinParser::JsonLines,
        None,
    )
    .unwrap();
    let tier_cwds = TierCwds {
        check: Some(PathBuf::from(subdir_name)),
        ..TierCwds::default()
    };
    let inputs = vec![ann(Tier::Check, "x")];
    let opts = DispatchOptions::default();
    let results = run_with_runners(&inputs, &[spec], &opts, dir.path(), &tier_cwds);
    let outcome = results[0].as_ref().unwrap();
    assert!(
        outcome.verdict.evidence.ends_with(subdir_name),
        "runner cwd None falls back to tier default `{subdir_name}` but got `{}`",
        outcome.verdict.evidence,
    );
}

#[test]
fn run_with_runners_uses_repo_root_when_neither_runner_nor_tier_cwd_set() {
    let dir = fixture_dir();
    let probe = write_script(
        dir.path(),
        "root-probe.sh",
        "#!/bin/sh\nfor t in \"$@\"; do printf '{\"target\":\"%s\",\"pass\":true,\"evidence\":\"%s\"}\\n' \"$t\" \"$PWD\"; done\n",
    );
    let spec = RunnerSpec::compile(
        "probe",
        None,
        format!("{probe} {{targets}}"),
        "{name}",
        " ",
        BuiltinParser::JsonLines,
        None,
    )
    .unwrap();
    let inputs = vec![ann(Tier::Check, "x")];
    let opts = DispatchOptions::default();
    let results = run_with_runners(&inputs, &[spec], &opts, dir.path(), &TierCwds::default());
    let outcome = results[0].as_ref().unwrap();
    let canonical_root = std::fs::canonicalize(dir.path()).unwrap();
    let canonical_observed = std::fs::canonicalize(&outcome.verdict.evidence).unwrap();
    assert_eq!(
        canonical_observed, canonical_root,
        "neither runner nor tier cwd → spawn under repo root",
    );
}

#[test]
fn run_with_runners_tier_default_applies_to_unmatched_per_annotation_fallback() {
    let dir = fixture_dir();
    let subdir_name = "fallback-cwd";
    let nested = dir.path().join(subdir_name);
    std::fs::create_dir(&nested).unwrap();
    let probe_path = nested.join("probe.sh");
    fs::write(
        &probe_path,
        "#!/bin/sh\nprintf '{\"pass\": true, \"evidence\": \"%s\"}\\n' \"$PWD\"\n",
    )
    .unwrap();

    let spec = RunnerSpec::compile(
        "noclaim",
        Some("^never-matches"),
        "ignored",
        "{name}",
        " ",
        BuiltinParser::JsonLines,
        None,
    )
    .unwrap();
    let tier_cwds = TierCwds {
        check: Some(PathBuf::from(subdir_name)),
        ..TierCwds::default()
    };
    let inputs = vec![ann(Tier::Check, "sh probe.sh")];
    let opts = DispatchOptions::default();
    let results = run_with_runners(&inputs, &[spec], &opts, dir.path(), &tier_cwds);
    let outcome = results[0].as_ref().unwrap();
    assert!(
        outcome.verdict.evidence.contains(subdir_name),
        "unmatched annotation must inherit tier-default cwd `{subdir_name}` but got `{}`",
        outcome.verdict.evidence,
    );
}

/// `[system]` execution stays per-annotation, but the per-spawn cwd still
/// resolves via the `[runner.system] cwd = "..."` tier default per
/// `specs/gate.md` § Runners — the section carves `[system]` out only for
/// batching and input-query, never for cwd. A configured tier-default cwd
/// that the old `run_system(annotations, options)` signature could not see
/// must now reach the spawn.
#[test]
fn run_system_resolves_tier_default_cwd() {
    let dir = fixture_dir();
    let subdir_name = "sys-default-cwd";
    std::fs::create_dir(dir.path().join(subdir_name)).unwrap();
    let probe = write_script(
        dir.path(),
        "sys-tier-probe.sh",
        "#!/bin/sh\nprintf '{\"pass\": true, \"evidence\": \"%s\"}\\n' \"$PWD\"\n",
    );
    let tier_cwds = TierCwds {
        system: Some(PathBuf::from(subdir_name)),
        ..TierCwds::default()
    };
    let inputs = vec![ann(Tier::System, &probe)];
    let opts = DispatchOptions::default();
    let results = run_system(&inputs, &[], &opts, dir.path(), &tier_cwds);
    let outcome = results[0].as_ref().unwrap();
    assert!(
        outcome.verdict.evidence.ends_with(subdir_name),
        "system spawn must honour tier-default cwd `{subdir_name}` but got `{}`",
        outcome.verdict.evidence,
    );
}

/// A `[runner.system.<name>]` block that matches a `[system]` target owns
/// invocation construction. Execution stays per-annotation, so the
/// runner's command template is rendered once per matched annotation.
#[test]
fn run_system_renders_matched_runner_command_per_annotation() {
    let dir = fixture_dir();
    let counter = dir.path().join("system-runner-spawns.txt");
    fs::write(&counter, "").unwrap();
    let counter_path = counter.display();
    let runner = dir.path().join("system-runner.sh");
    fs::write(
        &runner,
        format!(
            "#!/usr/bin/env bash\nset -euo pipefail\ntarget=\"$1\"\nprintf 'x\\n' >> \"{counter_path}\"\nprintf '{{\"target\":\"%s\",\"pass\":true,\"evidence\":\"%s\"}}\\n' \"$target\" \"$target\"\n"
        ),
    )
    .unwrap();
    let spec = RunnerSpec::compile(
        "sys",
        Some(r"^sys:(\S+)$"),
        format!("bash {} {{targets}}", runner.display()),
        "{capture_1}",
        " ",
        BuiltinParser::JsonLines,
        None,
    )
    .unwrap();
    let inputs = vec![
        ann(Tier::System, "sys:alpha"),
        ann(Tier::System, "sys:beta"),
    ];
    let opts = DispatchOptions::default();
    let results = run_system(&inputs, &[spec], &opts, dir.path(), &TierCwds::default());
    assert_eq!(results.len(), 2);

    let first = results[0].as_ref().unwrap();
    assert!(first.verdict.pass);
    assert_eq!(first.verdict.evidence, "alpha");

    let second = results[1].as_ref().unwrap();
    assert!(second.verdict.pass);
    assert_eq!(second.verdict.evidence, "beta");

    let spawns = fs::read_to_string(&counter).unwrap().lines().count();
    assert_eq!(spawns, 2, "system runner must spawn once per annotation");
}

#[test]
fn run_system_resolves_matched_runner_cwd() {
    let dir = fixture_dir();
    let subdir_name = "sys-runner-cwd";
    std::fs::create_dir(dir.path().join(subdir_name)).unwrap();
    let runner = dir.path().join("system-runner-cwd.sh");
    fs::write(
        &runner,
        "#!/usr/bin/env bash\nset -euo pipefail\ntarget=\"$1\"\nprintf '{\"target\":\"%s\",\"pass\":true,\"evidence\":\"%s\"}\\n' \"$target\" \"$PWD\"\n",
    )
    .unwrap();
    let spec = RunnerSpec::compile(
        "sys",
        Some(r"^sys:(\S+)$"),
        format!("bash {} {{targets}}", runner.display()),
        "{capture_1}",
        " ",
        BuiltinParser::JsonLines,
        Some(PathBuf::from(subdir_name)),
    )
    .unwrap();
    let inputs = vec![ann(Tier::System, "sys:alpha")];
    let opts = DispatchOptions::default();
    let results = run_system(&inputs, &[spec], &opts, dir.path(), &TierCwds::default());
    let outcome = results[0].as_ref().unwrap();
    assert!(
        outcome.verdict.evidence.ends_with(subdir_name),
        "system spawn must honour matched-runner cwd `{subdir_name}` but got `{}`",
        outcome.verdict.evidence,
    );
}

#[test]
fn run_with_runners_libtest_json_maps_test_names_back_to_annotations() {
    let dir = fixture_dir();
    let runner = write_script(
        dir.path(),
        "libtest.sh",
        "#!/bin/sh\nfor t in \"$@\"; do printf '{\"type\":\"test\",\"event\":\"ok\",\"name\":\"%s\"}\\n' \"$t\"; done\n",
    );
    let spec = RunnerSpec::compile(
        "nextest",
        None,
        format!("{runner} {{targets}}"),
        "{name}",
        " ",
        BuiltinParser::LibtestJson,
        None,
    )
    .unwrap();
    let inputs = vec![
        ann(Tier::Test, "crate::a::one"),
        ann(Tier::Test, "crate::b::two"),
    ];
    let opts = DispatchOptions::default();
    let results = run_with_runners(&inputs, &[spec], &opts, dir.path(), &TierCwds::default());
    assert_eq!(results.len(), 2);
    for r in &results {
        let outcome = r.as_ref().unwrap();
        assert!(outcome.verdict.pass);
    }
}

#[test]
fn run_with_runners_exit_code_parser_shares_verdict_across_group() {
    let dir = fixture_dir();
    let runner = write_script(
        dir.path(),
        "exit-fail.sh",
        "#!/bin/sh\necho 'something went wrong' >&2\nexit 1\n",
    );
    let spec = RunnerSpec::compile(
        "raw",
        None,
        format!("{runner} {{targets}}"),
        "{name}",
        " ",
        BuiltinParser::ExitCode,
        None,
    )
    .unwrap();
    let inputs = vec![ann(Tier::Check, "a"), ann(Tier::Check, "b")];
    let opts = DispatchOptions::default();
    let results = run_with_runners(&inputs, &[spec], &opts, dir.path(), &TierCwds::default());
    assert_eq!(results.len(), 2);
    for r in &results {
        let outcome = r.as_ref().unwrap();
        assert!(!outcome.verdict.pass);
        assert!(outcome.verdict.evidence.contains("something went wrong"));
    }
}

/// `run_check` routes through the matched runner when one claims the
/// targets: a single stub spawn covers both annotations and the
/// `json-lines` parser maps per-target verdicts back to the original
/// annotations. This is the contract that collapses many walk checks
/// into one cargo invocation.
#[test]
fn run_check_batches_loom_walk_shaped_targets_through_one_runner_spawn() {
    let dir = fixture_dir();
    let counter = dir.path().join("walk-batch-spawns.txt");
    fs::write(&counter, "").unwrap();
    let counter_path = counter.display();
    let runner = write_script(
        dir.path(),
        "walk-batch.sh",
        &format!(
            "#!/bin/sh\nprintf 'x\\n' >> \"{counter_path}\"\nfor t in \"$@\"; do\n  printf '{{\"target\":\"%s\",\"pass\":true,\"evidence\":\"%s-ok\"}}\\n' \"$t\" \"$t\"\ndone\n"
        ),
    );
    let spec = RunnerSpec::compile(
        "loom-walk",
        Some(r"^cargo run -p loom-walk -- (\S+)$"),
        format!("{runner} {{targets}}"),
        "{capture_1}",
        " ",
        BuiltinParser::JsonLines,
        None,
    )
    .unwrap();
    let inputs = vec![
        ann(Tier::Check, "cargo run -p loom-walk -- alpha"),
        ann(Tier::Check, "cargo run -p loom-walk -- beta"),
    ];
    let opts = DispatchOptions::default();
    let results = run_check(&inputs, &[spec], &opts, dir.path(), &TierCwds::default());
    assert_eq!(results.len(), 2);

    let first = results[0].as_ref().unwrap();
    assert!(first.verdict.pass);
    assert_eq!(first.verdict.evidence, "alpha-ok");
    assert_eq!(
        first.annotations[0].target,
        "cargo run -p loom-walk -- alpha"
    );

    let second = results[1].as_ref().unwrap();
    assert!(second.verdict.pass);
    assert_eq!(second.verdict.evidence, "beta-ok");
    assert_eq!(
        second.annotations[0].target,
        "cargo run -p loom-walk -- beta"
    );

    let observed_subprocess_count = fs::read_to_string(&counter).unwrap().lines().count();
    assert_eq!(
        observed_subprocess_count, 1,
        "matched [check] runner batch should spawn once for both annotations",
    );
}

/// Mixed batch — annotations the runner regex claims go through one
/// batched spawn; everything else falls through to per-annotation
/// dispatch for non-walk `[check]` shapes like grep/bash.
#[test]
fn run_check_mixes_runner_batch_with_per_annotation_fallback() {
    let dir = fixture_dir();
    let runner = write_script(
        dir.path(),
        "walk-batch-mixed.sh",
        "#!/bin/sh\nfor t in \"$@\"; do\n  printf '{\"target\":\"%s\",\"pass\":true,\"evidence\":\"batched\"}\\n' \"$t\"\ndone\n",
    );
    let fallback = write_script(
        dir.path(),
        "fallback-grep.sh",
        "#!/bin/sh\nprintf '{\"pass\": true, \"evidence\": \"singleton\"}\\n'\n",
    );

    let spec = RunnerSpec::compile(
        "loom-walk",
        Some(r"^cargo run -p loom-walk -- (\S+)$"),
        format!("{runner} {{targets}}"),
        "{capture_1}",
        " ",
        BuiltinParser::JsonLines,
        None,
    )
    .unwrap();
    let inputs = vec![
        ann(Tier::Check, "cargo run -p loom-walk -- alpha"),
        ann(Tier::Check, &fallback),
        ann(Tier::Check, "cargo run -p loom-walk -- beta"),
    ];
    let opts = DispatchOptions::default();
    let results = run_check(&inputs, &[spec], &opts, dir.path(), &TierCwds::default());
    assert_eq!(results.len(), 3);

    let batched_a = results[0].as_ref().unwrap();
    assert_eq!(batched_a.verdict.evidence, "batched");
    assert_eq!(
        batched_a.annotations[0].target,
        "cargo run -p loom-walk -- alpha"
    );

    let fallback_result = results[1].as_ref().unwrap();
    assert_eq!(
        fallback_result.verdict.evidence, "singleton",
        "grep/bash-shaped [check] annotation flows through per-annotation fallback"
    );

    let batched_b = results[2].as_ref().unwrap();
    assert_eq!(batched_b.verdict.evidence, "batched");
    assert_eq!(
        batched_b.annotations[0].target,
        "cargo run -p loom-walk -- beta"
    );
}

/// Empty `specs` slice degrades cleanly to per-annotation spawn for
/// every `[check]` entry. Other-tier annotations in the input are
/// filtered out before dispatch.
#[test]
fn run_check_with_empty_specs_falls_back_to_per_annotation_for_every_target() {
    let dir = fixture_dir();
    let script = write_script(
        dir.path(),
        "per-ann.sh",
        "#!/bin/sh\nprintf '{\"pass\": true, \"evidence\": \"solo\"}\\n'\n",
    );

    let inputs = vec![
        ann(Tier::Check, &script),
        ann(Tier::System, "/ignored"),
        ann(Tier::Check, &script),
    ];
    let opts = DispatchOptions::default();
    let results = run_check(&inputs, &[], &opts, dir.path(), &TierCwds::default());
    assert_eq!(results.len(), 2, "non-Check annotations filtered out");
    for r in &results {
        let outcome = r.as_ref().unwrap();
        assert!(outcome.verdict.pass);
        assert_eq!(outcome.verdict.evidence, "solo");
    }
}

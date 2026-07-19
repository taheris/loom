#![allow(clippy::unwrap_used)]
//! Integration coverage for the integrity gate.
//!
//! Wires the annotation parser to the integrity check at the seam that
//! `loom gate check` exercises: a temp specs/ tree is parsed end-to-end,
//! the resulting annotations and criteria feed `check`, and the findings
//! are asserted against the spec's failure-output contract.

use std::fs;
use std::path::{Path, PathBuf};

use loom_gate::Tier;
use loom_gate::annotation::{parse, parse_content};
use loom_gate::integrity::{
    CommandResolver, DispatchPendingExecutor, FsCommandResolver, IntegrityFinding,
    PendingCommandExecutor, RustWorkspaceStubScanner, RustWorkspaceTestResolver, StubScanner,
    TestPathResolver, check, check_atomic_acceptance, check_forward, scan_workspace_pair,
};
use loom_gate::{Annotation, DispatchOptions, TierCwds};
use tempfile::tempdir;

fn write(dir: &Path, name: &str, content: &str) {
    fs::write(dir.join(name), content).unwrap();
}

struct AlwaysOkCommands;
impl CommandResolver for AlwaysOkCommands {
    fn resolves(&self, _: &str) -> bool {
        true
    }
}

struct NeverOkTests;
impl TestPathResolver for NeverOkTests {
    fn resolves(&self, _: &str) -> bool {
        false
    }
}

struct NoStubs;
impl StubScanner for NoStubs {
    fn is_stub(&self, _: &str) -> bool {
        false
    }
}

/// Executor whose `executes_zero` is hard-wired to the value supplied at
/// construction time, regardless of the command. Lets pending-modifier
/// tests pin a deterministic forward-resolution outcome without spawning
/// real subprocesses.
struct StubExec(bool);
impl PendingCommandExecutor for StubExec {
    fn executes_zero(&self, _: &Annotation) -> bool {
        self.0
    }
}

#[test]
fn parse_then_check_with_all_valid_annotations_yields_no_findings() {
    let dir = tempdir().unwrap();
    let specs = dir.path().join("specs");
    fs::create_dir_all(&specs).unwrap();
    let rubric = dir.path().join("rubric.md");
    // Source-clean under the judge collect harness (Direction 4): a comment
    // body defines no rubric functions, so collect mode emits `{"inputs":{}}`.
    fs::write(&rubric, "#!/usr/bin/env bash\n# rubric body\n").unwrap();

    write(
        &specs,
        "alpha.md",
        "## Success Criteria\n\
        \n\
        - one [check](cargo run -p w -- a)\n\
        - two [system](nix run .#test-loom)\n\
        - three [test](crate::a::it_works)\n\
        - four [judge](../rubric.md)\n",
    );

    let parsed = parse(&specs).unwrap();
    let cmds = AlwaysOkCommands;
    let tests = RustWorkspaceTestResolver::from_leaves(["it_works"]);
    let findings = check(
        &parsed.annotations,
        &[],
        &specs,
        &cmds,
        &tests,
        &NoStubs,
        &StubExec(false),
    );
    assert!(
        findings.is_empty(),
        "no findings expected, got {findings:?}"
    );
}

#[test]
fn fixture_with_broken_target_per_tier_flags_each_one() {
    let dir = tempdir().unwrap();
    let md = "\
## Success Criteria

- one [check](no-such-binary --do x)
- two [system](also-not-there --boot)
- three [test](crate::nowhere::missing)
- four [judge](does-not-exist.md)
";
    let parsed = parse_content(&PathBuf::from("specs/broken.md"), md);
    struct NoCommands;
    impl CommandResolver for NoCommands {
        fn resolves(&self, _: &str) -> bool {
            false
        }
    }
    let findings = check_forward(
        &parsed.annotations,
        &[],
        dir.path(),
        &NoCommands,
        &NeverOkTests,
        &NoStubs,
        &StubExec(false),
    );
    assert_eq!(findings.len(), 4, "all four annotations flagged");
    for finding in &findings {
        assert!(
            matches!(finding, IntegrityFinding::UnresolvedAnnotation { .. }),
            "finding is unresolved: {finding:?}"
        );
    }
}

#[test]
fn two_annotations_on_one_criterion_flags_atomic_acceptance() {
    let md = "\
## Success Criteria

- shared claim
  [test](crate::a::ok)
  [check](cargo run -p w -- ok)
";
    let parsed = parse_content(&PathBuf::from("specs/atomic.md"), md);
    let findings = check_atomic_acceptance(&parsed.annotations);
    assert_eq!(findings.len(), 1);
    match &findings[0] {
        IntegrityFinding::MultipleAnnotations { spec, count, .. } => {
            assert_eq!(spec, &PathBuf::from("specs/atomic.md"));
            assert_eq!(*count, 2);
        }
        other => panic!("expected MultipleAnnotations, got {other:?}"),
    }
}

#[test]
fn self_referential_check_annotation_resolves_against_integrity_gate_implementation() {
    let md = "\
## Success Criteria

- The integrity gate self-checks its own resolution logic
  [check](cargo run -p loom-gate -- integrity-check)
";
    let parsed = parse_content(&PathBuf::from("specs/gate.md"), md);

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = Path::new(manifest_dir)
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf();
    let resolver = FsCommandResolver::new(&workspace_root);

    let findings = check_forward(
        &parsed.annotations,
        &[],
        &workspace_root,
        &resolver,
        &NeverOkTests,
        &NoStubs,
        &StubExec(false),
    );
    assert!(
        findings.is_empty(),
        "self-referential [check] annotation's first token (`cargo`) resolves on PATH; \
         got findings: {findings:?}"
    );
}

#[test]
fn self_referential_judge_annotation_resolves_against_integrity_source_file() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let integrity_rs = manifest_dir.join("src/integrity.rs");
    assert!(
        integrity_rs.exists(),
        "fixture relies on integrity.rs existing at {}",
        integrity_rs.display()
    );

    let target = integrity_rs.to_string_lossy();
    let md = format!("## Success Criteria\n\n- Integrity gate impl exists [judge]({target})\n");
    let parsed = parse_content(&PathBuf::from("specs/gate.md"), &md);
    struct NoCommands;
    impl CommandResolver for NoCommands {
        fn resolves(&self, _: &str) -> bool {
            false
        }
    }
    let findings = check_forward(
        &parsed.annotations,
        &[],
        Path::new("/this/is/ignored-when-target-is-absolute"),
        &NoCommands,
        &NeverOkTests,
        &NoStubs,
        &StubExec(false),
    );
    assert!(
        findings.is_empty(),
        "self-referential [judge] annotation should resolve to integrity.rs; got {findings:?}"
    );
}

#[test]
fn check_flags_cargo_test_annotation_with_missing_test_name() {
    let dir = tempdir().unwrap();
    let specs = dir.path().join("specs");
    fs::create_dir_all(&specs).unwrap();

    write(
        &specs,
        "alpha.md",
        "## Success Criteria\n\
        \n\
        - resolved [check](cargo test -p loom-events --lib known_test)\n\
        - missing [check](cargo test -p loom-events --lib missing_test)\n\
        - whole-suite [check](cargo test -p loom-templates --test snapshots)\n",
    );

    let parsed = parse(&specs).unwrap();
    let cmds = AlwaysOkCommands;
    let tests = RustWorkspaceTestResolver::from_leaves(["known_test"]);
    let findings = check(
        &parsed.annotations,
        &[],
        &specs,
        &cmds,
        &tests,
        &NoStubs,
        &StubExec(false),
    );

    let cargo_findings: Vec<_> = findings
        .iter()
        .filter(|f| matches!(f, IntegrityFinding::UnresolvedCargoTestName { .. }))
        .collect();
    assert_eq!(
        cargo_findings.len(),
        1,
        "exactly the missing_test annotation should flag, got: {findings:?}"
    );
    match cargo_findings[0] {
        IntegrityFinding::UnresolvedCargoTestName { test_name, .. } => {
            assert_eq!(test_name, "missing_test");
        }
        other => panic!("expected UnresolvedCargoTestName, got {other:?}"),
    }
}

#[test]
fn synthetic_specs_dir_check_combines_both_directions() {
    let dir = tempdir().unwrap();
    let specs = dir.path().join("specs");
    fs::create_dir_all(&specs).unwrap();

    write(
        &specs,
        "good.md",
        "## Success Criteria\n\
        \n\
        - ok [test](crate::a::ok)\n",
    );
    write(
        &specs,
        "bad.md",
        "## Success Criteria\n\
        \n\
        - has two annotations\n  \
          [test](crate::a::ok)\n  \
          [check](cargo run)\n\
        - has broken target [judge](missing.md)\n",
    );

    let parsed = parse(&specs).unwrap();
    let cmds = AlwaysOkCommands;
    let tests = RustWorkspaceTestResolver::from_leaves(["ok"]);
    let findings = check(
        &parsed.annotations,
        &[],
        &specs,
        &cmds,
        &tests,
        &NoStubs,
        &StubExec(false),
    );

    assert!(
        findings
            .iter()
            .any(|f| matches!(f, IntegrityFinding::UnresolvedAnnotation { tier, .. } if *tier == loom_gate::Tier::Judge)),
        "judge unresolved finding present: {findings:?}"
    );
    assert!(
        findings
            .iter()
            .any(|f| matches!(f, IntegrityFinding::MultipleAnnotations { count: 2, .. })),
        "multiple-annotations finding present: {findings:?}"
    );
}

#[test]
fn end_to_end_specs_dir_check_combines_both_directions() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("loom-gate crate must live under the workspace crates directory");
    let specs = workspace_root.join("specs");
    let parsed = parse(&specs).expect("parse workspace specs");
    let commands = FsCommandResolver::new(workspace_root);
    let (tests, stubs) = scan_workspace_pair(workspace_root).expect("scan workspace tests");

    let mut findings = check_forward(
        &parsed.annotations,
        &[],
        workspace_root,
        &commands,
        &tests,
        &stubs,
        &StubExec(false),
    );
    findings.extend(check_atomic_acceptance(&parsed.annotations));

    assert!(
        findings.is_empty(),
        "workspace annotation corpus has unresolved, stub, stale-pending, or atomic-acceptance findings: {findings:#?}",
    );
}

fn write_stub_fixture(src: &Path) {
    fs::write(
        src,
        "#[test]\nfn stub_fixture() {\n    _pending_stub();\n}\n\n#[test]\nfn real_fixture() {\n    assert!(true);\n}\n",
    )
    .unwrap();
}

#[test]
fn stub_pointing_test_annotation_flags_via_workspace_scanner() {
    let dir = tempdir().unwrap();
    let specs = dir.path().join("specs");
    fs::create_dir_all(&specs).unwrap();
    let src = dir.path().join("src.rs");
    write_stub_fixture(&src);

    write(
        &specs,
        "fixture.md",
        "## Success Criteria\n\
        \n\
        - real [test](crate::a::real_fixture)\n\
        - stubbed [test](crate::a::stub_fixture)\n",
    );

    let parsed = parse(&specs).unwrap();
    let cmds = AlwaysOkCommands;
    let tests = RustWorkspaceTestResolver::scan(dir.path()).unwrap();
    let stubs = RustWorkspaceStubScanner::scan(dir.path()).unwrap();
    let findings = check(
        &parsed.annotations,
        &[],
        &specs,
        &cmds,
        &tests,
        &stubs,
        &StubExec(false),
    );

    let stub_findings: Vec<_> = findings
        .iter()
        .filter(|f| matches!(f, IntegrityFinding::StubTestFunction { .. }))
        .collect();
    assert_eq!(
        stub_findings.len(),
        1,
        "exactly one stub annotation flagged, got: {findings:?}"
    );
    match stub_findings[0] {
        IntegrityFinding::StubTestFunction { test_name, .. } => {
            assert_eq!(test_name, "stub_fixture");
        }
        other => panic!("expected StubTestFunction, got {other:?}"),
    }
}

struct NoCommands;
impl CommandResolver for NoCommands {
    fn resolves(&self, _: &str) -> bool {
        false
    }
}

#[test]
fn pending_marked_unresolved_target_yields_no_finding() {
    let dir = tempdir().unwrap();
    let md = "\
## Success Criteria

- a [check?](no-such-binary --do x)
- b [system?](also-not-there --boot)
- c [test?](crate::nowhere::missing)
- d [judge?](does-not-exist.md)
";
    let parsed = parse_content(&PathBuf::from("specs/pending.md"), md);
    let findings = check_forward(
        &parsed.annotations,
        &[],
        dir.path(),
        &NoCommands,
        &NeverOkTests,
        &NoStubs,
        &StubExec(false),
    );
    assert!(
        findings.is_empty(),
        "`?` + unresolved target must pass silently across every tier, got: {findings:?}"
    );
}

#[test]
fn pending_marked_resolved_target_yields_unneeded_pending_marker() {
    let dir = tempdir().unwrap();
    let md = "\
## Success Criteria

- a [check?](cargo run -p w -- alpha)
- b [system?](nix run .#test-loom)
";
    let parsed = parse_content(&PathBuf::from("specs/pending.md"), md);
    let findings = check_forward(
        &parsed.annotations,
        &[],
        dir.path(),
        &AlwaysOkCommands,
        &NeverOkTests,
        &NoStubs,
        &StubExec(true),
    );
    assert_eq!(
        findings.len(),
        2,
        "both pending annotations whose command exits 0 must flag: {findings:?}"
    );
    for f in &findings {
        assert!(
            matches!(f, IntegrityFinding::UnneededPendingMarker { .. }),
            "every finding must be UnneededPendingMarker: {f:?}"
        );
        assert!(
            f.is_push_gate_terminal(),
            "UnneededPendingMarker must be terminal at the push gate: {f:?}"
        );
    }
}

#[test]
fn pending_marked_stub_test_body_yields_no_finding() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("src.rs");
    write_stub_fixture(&src);

    let md = "\
## Success Criteria

- stubbed [test?](crate::a::stub_fixture)
";
    let parsed = parse_content(&PathBuf::from("specs/pending.md"), md);
    let tests = RustWorkspaceTestResolver::scan(dir.path()).unwrap();
    let stubs = RustWorkspaceStubScanner::scan(dir.path()).unwrap();
    let findings = check_forward(
        &parsed.annotations,
        &[],
        dir.path(),
        &NoCommands,
        &tests,
        &stubs,
        &StubExec(false),
    );
    assert!(
        findings.is_empty(),
        "[test?] over a stub body must pass silently: {findings:?}"
    );
}

#[test]
fn pending_marked_non_stub_test_body_yields_unneeded_pending_marker() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("src.rs");
    write_stub_fixture(&src);

    let md = "\
## Success Criteria

- real [test?](crate::a::real_fixture)
";
    let parsed = parse_content(&PathBuf::from("specs/pending.md"), md);
    let tests = RustWorkspaceTestResolver::scan(dir.path()).unwrap();
    let stubs = RustWorkspaceStubScanner::scan(dir.path()).unwrap();
    let findings = check_forward(
        &parsed.annotations,
        &[],
        dir.path(),
        &NoCommands,
        &tests,
        &stubs,
        &StubExec(false),
    );
    assert_eq!(findings.len(), 1, "non-stub body must flag: {findings:?}");
    match &findings[0] {
        IntegrityFinding::UnneededPendingMarker {
            spec, tier, target, ..
        } => {
            assert_eq!(spec, &PathBuf::from("specs/pending.md"));
            assert_eq!(*tier, Tier::Test);
            assert_eq!(target, "crate::a::real_fixture");
        }
        other => panic!("expected UnneededPendingMarker, got {other:?}"),
    }
}

#[test]
fn pending_modifier_does_not_suppress_atomic_acceptance_finding() {
    let md = "\
## Success Criteria

- shared claim
  [test?](crate::a::pending_one)
  [check?](cargo run -p w -- pending_two)
";
    let parsed = parse_content(&PathBuf::from("specs/pending.md"), md);
    let findings = check_atomic_acceptance(&parsed.annotations);
    assert_eq!(
        findings.len(),
        1,
        "atomic acceptance fires even when every annotation carries `?`: {findings:?}"
    );
    assert!(matches!(
        findings[0],
        IntegrityFinding::MultipleAnnotations { count: 2, .. }
    ));
}

#[test]
fn unneeded_pending_marker_is_terminal_at_push_gate() {
    let dir = tempdir().unwrap();
    let md = "\
## Success Criteria

- a [check?](cargo run -p w -- ok)
";
    let parsed = parse_content(&PathBuf::from("specs/pending.md"), md);
    let findings = check_forward(
        &parsed.annotations,
        &[],
        dir.path(),
        &AlwaysOkCommands,
        &NeverOkTests,
        &NoStubs,
        &StubExec(true),
    );
    assert_eq!(findings.len(), 1);
    assert!(
        findings[0].is_push_gate_terminal(),
        "UnneededPendingMarker must be push-gate-terminal (parity with \
         UnresolvedAnnotation and StubTestFunction): {findings:?}"
    );
}

/// End-to-end check that the production [`DispatchPendingExecutor`]
/// realises the spec contract: a `[check?]` whose command actually
/// exits 0 (here `true`) emits `UnneededPendingMarker` — the marker is
/// stale, the implementer must drop the `?`.
#[test]
fn pending_check_command_exit_zero_via_subprocess_emits_unneeded_pending_marker() {
    let dir = tempdir().unwrap();
    let md = "\
## Success Criteria

- a [check?](true)
";
    let parsed = parse_content(&PathBuf::from("specs/pending.md"), md);
    let executor = DispatchPendingExecutor::new(
        &[],
        DispatchOptions::default(),
        dir.path(),
        TierCwds::default(),
    );
    let findings = check_forward(
        &parsed.annotations,
        &[],
        dir.path(),
        &AlwaysOkCommands,
        &NeverOkTests,
        &NoStubs,
        &executor,
    );
    assert_eq!(
        findings.len(),
        1,
        "`true` exits 0 → UnneededPendingMarker must fire: {findings:?}"
    );
    assert!(
        matches!(findings[0], IntegrityFinding::UnneededPendingMarker { .. }),
        "got {:?}",
        findings[0]
    );
}

/// End-to-end check covering the broadened spec contract: a `[check?]`
/// whose first token resolves on PATH but whose full command exits
/// non-zero (the assertion-pending case) silently passes. The narrow
/// first-token-on-PATH check this replaced would have fired a stale
/// `UnneededPendingMarker` here.
#[test]
fn pending_check_command_exit_nonzero_via_subprocess_passes_silently() {
    let dir = tempdir().unwrap();
    let md = "\
## Success Criteria

- a [check?](false)
";
    let parsed = parse_content(&PathBuf::from("specs/pending.md"), md);
    let executor = DispatchPendingExecutor::new(
        &[],
        DispatchOptions::default(),
        dir.path(),
        TierCwds::default(),
    );
    let findings = check_forward(
        &parsed.annotations,
        &[],
        dir.path(),
        &AlwaysOkCommands,
        &NeverOkTests,
        &NoStubs,
        &executor,
    );
    assert!(
        findings.is_empty(),
        "`false` exits non-zero → silent pass (assertion still pending): {findings:?}"
    );
}

/// End-to-end check that an assertion-pending `[check?](grep -q …)`
/// targeting a real file whose contents do not yet contain the asserted
/// symbol passes silently. The narrow first-token-on-PATH check would
/// have falsely fired `UnneededPendingMarker` because `grep` resolves;
/// the broadened subprocess-execution check sees grep's non-zero exit
/// and honors the modifier.
#[test]
fn pending_check_assertion_grep_with_missing_symbol_passes_silently() {
    let dir = tempdir().unwrap();
    let target_file = dir.path().join("source.rs");
    fs::write(&target_file, "fn placeholder() {}\n").unwrap();

    let md = format!(
        "## Success Criteria\n\n- a [check?](grep -q 'pub enum BadWalk' {})\n",
        target_file.display()
    );
    let parsed = parse_content(&PathBuf::from("specs/pending.md"), &md);
    let executor = DispatchPendingExecutor::new(
        &[],
        DispatchOptions::default(),
        dir.path(),
        TierCwds::default(),
    );
    let findings = check_forward(
        &parsed.annotations,
        &[],
        dir.path(),
        &AlwaysOkCommands,
        &NeverOkTests,
        &NoStubs,
        &executor,
    );
    assert!(
        findings.is_empty(),
        "grep over a file missing the asserted symbol exits non-zero → silent pass: {findings:?}"
    );
}

/// Symmetric to the assertion-pending case above: once the asserted
/// symbol lands in the target file, `grep -q` exits 0 and
/// `UnneededPendingMarker` fires — co-incidence between *"target now
/// resolves"* and *"marker now removed"* per the spec's self-cleaning
/// contract.
#[test]
fn pending_check_assertion_grep_with_present_symbol_emits_unneeded_pending_marker() {
    let dir = tempdir().unwrap();
    let target_file = dir.path().join("source.rs");
    fs::write(
        &target_file,
        "pub enum BadWalk { Concern, FindingsWithoutConcern }\n",
    )
    .unwrap();

    let md = format!(
        "## Success Criteria\n\n- a [check?](grep -q 'pub enum BadWalk' {})\n",
        target_file.display()
    );
    let parsed = parse_content(&PathBuf::from("specs/pending.md"), &md);
    let executor = DispatchPendingExecutor::new(
        &[],
        DispatchOptions::default(),
        dir.path(),
        TierCwds::default(),
    );
    let findings = check_forward(
        &parsed.annotations,
        &[],
        dir.path(),
        &AlwaysOkCommands,
        &NeverOkTests,
        &NoStubs,
        &executor,
    );
    assert_eq!(
        findings.len(),
        1,
        "grep finds the asserted symbol → UnneededPendingMarker must fire: {findings:?}"
    );
    assert!(matches!(
        findings[0],
        IntegrityFinding::UnneededPendingMarker { .. }
    ));
}

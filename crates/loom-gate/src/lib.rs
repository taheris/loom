//! `gate` — annotation dispatch, status cache, integrity gate.
//!
//! Owns the runtime that `loom gate <subcommand>` invokes. Walks the
//! consumer's `specs/*.md` for `[tier](target)` annotations (per
//! `docs/spec-conventions.md`), dispatches each annotation to its verifier
//! under the rules in `specs/gate.md`, persists per-criterion verdicts
//! in a sqlite-backed status cache, and self-checks the annotation set via
//! the integrity gate.

pub mod annotation;
pub mod cache;
pub mod dispatch;
pub mod gate_outcome;
pub mod inputs;
pub mod integrity;
pub mod marker;
pub mod pre_commit;
pub mod runner;
pub mod scope;

pub use annotation::{Annotation, Criterion, ParsedSpecs, Tier};
pub use cache::{
    AnnotationHealth, BrokenAnnotation, CacheError, CacheRow, FailingCriterion, Report, SpecReport,
    StaleAnnotation, StaleRun, StatusCache, TierSummary, Verdict, render_from_rows, render_report,
    row_for,
};
pub use dispatch::{
    DispatchError, DispatchOptions, DispatchOutcome, EmptyScope, SKIP_EXIT_CODE, TestScope,
    TierCwds, VerifierVerdict, run_check, run_judge, run_system, run_test, run_test_in,
    run_with_runners,
};
pub use gate_outcome::{
    GateFail, GateFailReason, GateOutcome, GatePhase, GateRun, GateRunStatus, GateSuccess,
    HandoffEvidence, HookCoverage, LoopOutcome, MoleculeState, NoGateReason, PrePushCoverage,
    ReviewedScope, VerifiedScope, append_gate_run_lifecycle_events, parse_gate_runs_from_jsonl,
};
pub use inputs::{InputQueryProbe, InputResolver, InputsError, VerifierInputs, filter_by_files};
pub use integrity::{
    CommandResolver, DispatchPendingExecutor, FsCommandResolver, IntegrityError, IntegrityFinding,
    PendingCommandExecutor, RustWorkspaceStubScanner, RustWorkspaceTestResolver, StubScanner,
    TestPathResolver, check_inputs_protocol, compose_clarify_options, is_missing_binary_target,
};
pub use marker::{
    MARKER_PATH, MarkerError, MarkerProof, MarkerValidationRequest, MintError, verify_marker,
    verify_marker_for_hook,
};
pub use pre_commit::{
    ConfigError as PreCommitConfigError, pre_push_hook_coverage_from_config,
    pre_push_hook_coverage_from_path,
};
pub use runner::{
    BuiltinParser, MatchedAnnotation, ParsedVerdict, RunnerError, RunnerGroup, RunnerKind,
    RunnerSpec, RunnerTemplate, check_zero_match, group_by_runner, parse_runner_output,
};
pub use scope::{CargoMetadataScope, ScopeError};

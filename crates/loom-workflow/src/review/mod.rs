//! LLM inspection and review verdicts.
//!
//! CLI inspection invokes one reviewer session. Only a full diff review consuming
//! matching verified evidence can emit push-gate review evidence. Publication
//! belongs to the loop workflow, never to inspection or its scope/context metadata.

mod context;
mod error;
mod finding;
mod fixup;
mod inspection;
mod iteration;
mod phase_verdict;
mod production;
mod recovery;
mod runner;
mod verdict;
mod verify_fail;
mod workspace_validator;

pub use context::{
    ReviewContextInputs, beads_summary, build_review_context, default_profile_for_spec,
    load_review_sources, load_review_sources_for_lane,
};
pub use error::ReviewError;
pub use finding::{
    ConcernToken, DispatchScope, Finding, FindingParseError, FindingRoute, FindingTarget,
    FindingValidator, LOOM_FINDING_PREFIX, RawFinding, ScopeKind, TargetKind, TerminalSurface,
    WalkOutput, WalkOutputError, parse_walk_output,
};
pub use fixup::{FixupContext, FixupOutcome, FixupRequest, spawn_fixup_bead};
pub use iteration::{DEFAULT_MAX_ITERATIONS, IterationCap};
pub use loom_templates::previous_failure::BadWalk;
pub use loom_templates::review::ReviewLane;
pub use phase_verdict::{
    GateInputs, PhaseKind, PhaseVerdict, RecoveryCause, ReviewConcern, decide, decide_for_phase,
};
pub use production::{AcceptAllFindingValidator, ProductionReviewController};
pub use recovery::{
    RETRY_EXHAUSTED_CAUSE, RecoveryResolution, cause_to_previous_failure,
    concern_label_from_findings, resolve_recovery,
};
pub use runner::{ReviewController, ReviewOutcome, ReviewResult, review_loop};
pub use verdict::{BeadSnapshot, PushGateRefuseCause, ReviewVerdict, diff_new_bead_ids};
pub use verify_fail::{
    PREVIOUS_FAILURE_BUDGET, STDERR_TAIL_LINES, VerifyFailure, format_previous_failure,
};
pub use workspace_validator::WorkspaceFindingValidator;

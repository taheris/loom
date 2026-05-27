//! `loom review` — LLM-judged review + push gate.
//!
//! Implements the review-gate semantics defined in
//! `specs/gate.md` ("Per-diff stage checks") on top of
//! `loom-driver`'s typed surface and `templates`' `review.md` template.
//! The gate:
//!
//! 1. snapshots beads carrying `spec:<label>` (`pre`);
//! 2. renders [`ReviewContext`](loom_templates::review::ReviewContext), spawns
//!    `wrapix spawn --spawn-config <file> --stdio`, drives an
//!    [`AgentBackend`](loom_driver::agent::AgentBackend) and tees the
//!    [`AgentEvent`](loom_driver::agent::AgentEvent) stream into the
//!    terminal renderer + per-bead JSONL log;
//! 3. snapshots beads again, computes new bead IDs and clarify membership;
//! 4. branches: clean → `git push` + `beads-push`; clarify → stop;
//!    fix-up + under cap → `exec loom run`; fix-up + at cap → escalate the
//!    newest fix-up bead to `loom:clarify`.
//!
//! `loom run`'s molecule-complete handoff (`exec_review` in [`super::run`])
//! is wired by the binary to invoke this module.

mod context;
mod error;
mod fixup;
mod iteration;
mod phase_verdict;
mod production;
mod recovery;
mod runner;
mod verdict;
mod verify_fail;

pub use context::{ReviewContextInputs, beads_summary, build_review_context, load_review_sources};
pub use error::ReviewError;
pub use fixup::{FixupContext, FixupOutcome, FixupRequest, spawn_fixup_bead};
pub use iteration::{DEFAULT_MAX_ITERATIONS, IterationCap};
pub use loom_templates::review::ReviewLane;
pub use phase_verdict::{
    GateInputs, PhaseVerdict, RecoveryCause, ReviewConcern, ReviewFlag, decide, parse_review_flag,
};
pub use production::ProductionReviewController;
pub use recovery::{
    RETRY_EXHAUSTED_CAUSE, RecoveryResolution, cause_to_previous_failure, resolve_recovery,
};
pub use runner::{ReviewController, ReviewOutcome, ReviewResult, review_loop};
pub use verdict::{BeadSnapshot, PushGateRefuseCause, ReviewVerdict, diff_new_bead_ids};
pub use verify_fail::{
    PREVIOUS_FAILURE_BUDGET, REVIEW_NOTES_BUDGET, STDERR_TAIL_LINES, VerifyFailure,
    format_previous_failure,
};

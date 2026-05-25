//! `loom todo` — spec-to-beads decomposition.
//!
//! Implements four-tier detection (per-spec cursor fan-out) via
//! `compute_spec_diff`.
//!
//! - Tier 1 (`diff`): a molecule with `base_commit` exists → widen to
//!   `git diff <base> HEAD -- specs/` and apply per-spec cursor fan-out.
//! - Tier 2 (`tasks`): molecule exists without `base_commit` → fall back to
//!   LLM comparison against existing task descriptions.
//! - Tier 3 (`README discovery`): no molecule in state → look up molecule ID
//!   in the pinned-context file. The caller validates via `bd show` and
//!   threads the result back into [`compute_spec_diff`] as a synthetic
//!   tier-2 input. Discovery itself lives outside this module.
//! - Tier 4 (`new`): nothing → full spec decomposition.

mod context;
mod error;
mod exit;
mod production;
mod runner;
mod tier;

pub use context::{TemplateBaseFields, TodoTemplateContext, build_template_context};
pub use error::TodoError;
pub use exit::{ExitSignal, parse_exit_signal};
pub use production::ProductionTodoController;
pub use runner::{TodoController, TodoSummary, run};
pub use tier::{
    DiffCandidate, GitDiffSource, MoleculeState, TierDecision, TierInputs, compute_spec_diff,
};

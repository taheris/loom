//! `loom todo` — spec-to-beads decomposition.
//!
//! Resolves the changed-spec roster, prepares or reuses the work epic, and
//! renders the unified typed todo prompt.
//!
//! Touched-set discovery (see [`touched_specs`]) walks every spec whose
//! markdown differs from `HEAD` in the working tree and renders each diff
//! into the prompt, so the agent fans out across anchor + sibling specs in
//! one decomposition pass.

mod context;
mod criterion_status;
mod error;
mod exit;
mod fanout;
mod production;
mod resolve;
mod runner;
mod touched;

pub use context::{
    FingerprintSpecInput, TemplateBaseFields, build_template_context, changed_spec_context,
    implementation_notes_context, spec_epic_context, todo_fingerprint,
};
pub use criterion_status::{build_criterion_status, criterion_id_for, criterion_text_for_line};
pub use error::TodoError;
pub use exit::{ExitSignal, parse_exit_signal};
pub use fanout::{FanoutOutcome, SpecResolution, classify_touched_set, render_collision_options};
pub use production::ProductionTodoController;
pub use resolve::{ResolverOutcome, resolve_molecule};
pub use runner::{TodoController, TodoSummary, run};
pub use touched::{TouchedSpec, render_fanout_block, touched_specs};

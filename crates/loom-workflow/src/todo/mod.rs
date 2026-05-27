//! `loom todo` — spec-to-beads decomposition.
//!
//! Resolves "the active molecule for spec X" via a single
//! `bd find --type=epic --label=spec:<X> --status=open` query (see
//! [`resolve_molecule`]). Three outcomes — `Existing`, `None`, and
//! `InvariantViolation` — capture every shape the loop must handle.
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

pub use context::{TemplateBaseFields, TodoTemplateContext, build_template_context};
pub use criterion_status::build_criterion_status;
pub use error::TodoError;
pub use exit::{ExitSignal, parse_exit_signal};
pub use fanout::{FanoutOutcome, SpecResolution, classify_touched_set, render_collision_options};
pub use production::ProductionTodoController;
pub use resolve::{ResolverOutcome, resolve_molecule};
pub use runner::{TodoController, TodoSummary, run};
pub use touched::{TouchedSpec, render_fanout_block, touched_specs};

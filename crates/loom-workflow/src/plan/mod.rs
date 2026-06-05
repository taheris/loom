//! `loom plan` — interactive spec interview.
//!
//! `plan` is the exception to Loom's JSONL-driven workflow. The interview is
//! a human-in-the-loop terminal session, so loom shells out to interactive
//! `wrapix run` (TTY attached) rather than `wrapix spawn --stdio`. There
//! is no subprocess capture, no JSONL parsing, and no event tee.
//!
//! `[phase.plan]` selects both the profile image and agent command used for
//! the interactive shell-out. A Pi sandbox runs `pi`; a Claude sandbox runs
//! `claude`.
//!
//! Flow per `specs/harness.md`:
//!
//! 1. parse `-n <label>` (new) or `-u <label>` (update) into [`PlanMode`];
//! 2. acquire `<label>.lock` for the duration of the call;
//! 3. render `plan_new.md` or `plan_update.md` via Askama into a typed
//!    prompt body ([`prompt::render_prompt`]);
//! 4. exec `wrapix run <workspace> <agent command> ... <prompt>` with stdio
//!    inherited so the configured agent attaches to the user's terminal
//!    ([`command::build_wrapix_argv`]);
//! 5. after the interactive session exits, parse the resulting spec markdown
//!    for `## Companions` and replace the companion rows for `label` in the
//!    state DB ([`companions::reconcile_companions`]);
//! 6. set `current_spec = label` in the state DB so subsequent commands
//!    (`loom todo`, `loom loop`, `loom spec`) resolve the spec without a
//!    separate `loom use` call.
//!
//! Hidden specs are deliberately unsupported — keeping a spec out of git is
//! covered by `.git/info/exclude` (see *Out of Scope* in the harness spec).

mod args;
mod command;
mod companions;
mod error;
mod prompt;
mod runner;

pub use args::{PlanMode, parse_mode};
pub use command::{WRAPIX_BIN, build_wrapix_argv};
pub use companions::{CompanionReconciliation, reconcile_companions};
pub use error::PlanError;
pub use prompt::{PlanPromptInputs, render_prompt};
pub use runner::{
    PlanOpts, PlanReport, WRAPIX_DEFAULT_IMAGE_REF, WRAPIX_DEFAULT_IMAGE_SOURCE, run,
};

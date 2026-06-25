//! `loom plan` — interactive spec interview.
//!
//! `plan` is the exception to Loom's JSONL-driven workflow. The interview is
//! a human-in-the-loop terminal session, so loom shells out to interactive
//! `wrix run` (TTY attached) rather than `wrix spawn --stdio`. There is no
//! subprocess capture, no JSONL parsing, and no event tee.
//!
//! `[phase.plan]` selects both the profile image and agent command used for
//! the interactive shell-out. Claude runs with its per-session compact hook;
//! Pi fails fast until a controlled interactive re-pin bridge exists.
//!
//! Flow per `specs/harness.md`:
//!
//! 1. parse optional positional labels into typed anchor labels;
//! 2. acquire `plan.lock` for the duration of the call;
//! 3. render `plan.md` via Askama into a typed prompt body;
//! 4. exec `wrix run <workspace> <agent command> ... <prompt>` with stdio
//!    inherited so the configured agent attaches to the user's terminal.
//!
//! Hidden specs are deliberately unsupported — keeping a spec out of git is
//! covered by `.git/info/exclude` (see *Out of Scope* in the harness spec).

mod args;
mod command;
mod error;
mod prompt;
mod runner;

pub use args::parse_anchor_labels;
pub use command::{WRIX_BIN, build_wrix_argv};
pub use error::PlanError;
pub use prompt::{PlanPromptInputs, render_prompt};
pub use runner::{PlanOpts, PlanReport, WRIX_DEFAULT_IMAGE_REF, WRIX_DEFAULT_IMAGE_SOURCE, run};

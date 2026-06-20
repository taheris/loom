use std::path::PathBuf;

use displaydoc::Display;
use loom_driver::agent::ProtocolError;
use loom_driver::bd::BdError;
use loom_driver::git::GitError;
use loom_driver::identifier::ParseBeadIdError;
use loom_driver::profile_manifest::ProfileError;
use loom_driver::state::CacheError;
use thiserror::Error;

/// Errors raised by the `loom todo` driver.
#[derive(Debug, Display, Error)]
pub enum TodoError {
    /// the `--since {commit}` override does not refer to a reachable commit
    InvalidSinceCommit { commit: String },

    /// multiple open epics found for spec `{label}`: {ids}; close all but one before re-running
    InvariantViolation { label: String, ids: String },

    /// spec index is inconsistent: {detail}
    SpecIndex { detail: String },

    /// duplicate loom:spec epics found for spec `{label}`: {ids}; close or relabel all but one before re-running
    DuplicateSpecEpics { label: String, ids: String },

    /// existing spec epic `{epic_id}` for `{label}` lacks `loom.todo_cursor`; repair with `bd update {epic_id} --set-metadata loom.todo_cursor=<sha>`
    MissingSpecCursor { label: String, epic_id: String },

    /// spec epic `{epic_id}` for `{label}` has invalid `loom.todo_cursor` `{cursor}`: {reason}
    InvalidSpecCursor {
        label: String,
        epic_id: String,
        cursor: String,
        reason: String,
    },

    /// pending loom:todo epic conflict across {ids}; resolve before re-running:
    /// {diagnostic}
    PendingTodoEpicConflict { ids: String, diagnostic: String },

    /// no specs changed since their durable `loom.todo_cursor`; no todo agent was run
    NoChangedSpecs,

    /// todo success was observed before deterministic preflight completed
    TodoSuccessWithoutPreflight,

    /// `loom todo` success must be `LOOM_TODO: <json>`; generic LOOM_COMPLETE/LOOM_NOOP are wrong-phase markers
    GenericTodoMarker,

    /// LOOM_TODO validation failed: {detail}
    TodoValidation { detail: String },

    /// todo agent supplied no terminal signal — expected a final `LOOM_TODO: <json>` success payload or a worker self-report marker
    MissingExitSignal,

    /// agent reported LOOM_BLOCKED: {reason}
    AgentBlocked { reason: String },

    /// could not read spec file at {path}
    ReadSpec {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// rendering the prompt template failed
    Render(#[from] askama::Error),

    /// io operation failed during `loom todo`
    Io(#[from] std::io::Error),

    /// multi-spec fan-out collision; `loom:clarify` bead {clarify_id} created — resolve via `loom inbox`
    MultiSpecCollision { clarify_id: String },

    /// agent reported productive completion for spec `{label}` but minted no implementation beads despite {notes_remaining} note(s) remaining — either re-run after `loom note clear {label}` if the notes are obsolete, or investigate why the agent skipped fan-out (see logs/{label}/todo-*.jsonl)
    ProductiveCompletionWithoutFanout {
        label: String,
        notes_remaining: usize,
    },

    /// agent backend protocol failure during `loom todo`
    Protocol(#[from] ProtocolError),

    /// cache-db read/write failure during `loom todo`
    State(#[from] CacheError),

    /// profile-image manifest dispatch failed during `loom todo`
    Profile(#[from] ProfileError),

    /// bd client failure during `loom todo`
    Bd(#[from] BdError),

    /// git operation failed during `loom todo`
    Git(#[from] GitError),

    /// invalid work epic id `{id}` returned during `loom todo`
    InvalidWorkEpic {
        id: String,
        #[source]
        source: ParseBeadIdError,
    },

    /// spec → molecule resolution failed during `loom todo`
    Resolve(#[from] crate::resolve::ResolveError),

    /// skill resolution failed during `loom todo`
    Skill(#[from] crate::skill::SkillError),
}

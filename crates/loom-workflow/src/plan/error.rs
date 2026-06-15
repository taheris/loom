use std::io;
use std::path::PathBuf;

use displaydoc::Display;
use thiserror::Error;

use loom_driver::lock::LockError;
use loom_driver::profile_manifest::ProfileError;
use loom_driver::state::StateError;

/// Failures raised by [`super::run`] and the helpers it composes.
#[derive(Debug, Display, Error)]
pub enum PlanError {
    /// invalid plan anchor label `{label}`: expected lowercase ASCII kebab-case
    InvalidAnchorLabel { label: String },

    /// failed to read pinned-context file at {path}
    ReadPinnedContext {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// askama template render failed while running `loom plan`
    Render(#[from] askama::Error),

    /// lock acquisition failed while running `loom plan`
    Lock(#[from] LockError),

    /// state-db operation failed while running `loom plan`
    State(#[from] StateError),

    /// profile-image manifest lookup failed while resolving the plan phase
    Profile(#[from] ProfileError),

    /// agent-selection failed for `[phase.plan]`
    AgentSelection(#[from] loom_driver::config::AgentSelectionError),

    /// direct backend cannot run interactive `loom plan`
    DirectInteractive,

    /// failed to spawn `wrix run`
    Spawn {
        #[source]
        source: io::Error,
    },

    /// `wrix run` exited with status {status}
    WrixExit { status: String },
}

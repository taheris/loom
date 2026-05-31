use displaydoc::Display;
use thiserror::Error;

use loom_driver::agent::ProtocolError;
use loom_driver::bd::BdError;
use loom_driver::logging::LogError;
use loom_driver::profile_manifest::ProfileError;
use loom_driver::state::StateError;

use crate::spec::SpecError;

/// Errors raised by the `loom review` driver.
#[derive(Debug, Display, Error)]
pub enum ReviewError {
    /// agent backend protocol failure during `loom review`
    Protocol(#[from] ProtocolError),

    /// bd CLI failure during `loom review`
    Bd(#[from] BdError),

    /// rendering the review.md template failed
    Render(#[from] askama::Error),

    /// log sink failure during `loom review`
    Log(#[from] LogError),

    /// io operation failed during `loom review`
    Io(#[from] std::io::Error),

    /// reviewer agent did not emit LOOM_COMPLETE: {0}
    ReviewIncomplete(String),

    /// `git push` failed: {0}
    GitPushFailed(String),

    /// `beads-push` failed after `git push` succeeded: {0}
    BeadsPushFailed(String),

    /// detached HEAD — refuse to push
    DetachedHead,

    /// `loom loop` handoff for auto-iteration failed: {0}
    RunHandoff(String),

    /// state-db read/write failure during `loom review`
    State(#[from] StateError),

    /// profile-image manifest dispatch failed during `loom review`
    Profile(#[from] ProfileError),

    /// no active molecule for spec {0} — run `loom todo` before `loom review`
    NoActiveMolecule(String),

    /// failed to load `[test]`/`[judge]` sources for review prompt
    Spec(#[from] SpecError),

    /// spec → molecule resolution failed during `loom review`
    Resolve(#[from] crate::resolve::ResolveError),
}

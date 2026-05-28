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
    /// agent backend protocol failure: {0}
    Protocol(#[from] ProtocolError),

    /// bd CLI failure: {0}
    Bd(#[from] BdError),

    /// rendering the review.md template failed: {0}
    Render(#[from] askama::Error),

    /// log sink failure: {0}
    Log(#[from] LogError),

    /// io operation failed: {0}
    Io(#[from] std::io::Error),

    /// reviewer agent did not emit LOOM_COMPLETE: {0}
    ReviewIncomplete(String),

    /// reviewer emitted `LOOM_CONCERN: {token} -- {reason}` but minted no fix-up / clarify / blocked beads — protocol violation, see crates/loom-templates/templates/review.md § "Creating Fix-Up Beads"
    ConcernWithoutBeadDeltas { token: String, reason: String },

    /// `git push` failed: {0}
    GitPushFailed(String),

    /// `beads-push` failed after `git push` succeeded: {0}
    BeadsPushFailed(String),

    /// detached HEAD — refuse to push
    DetachedHead,

    /// `loom loop` handoff for auto-iteration failed: {0}
    RunHandoff(String),

    /// state-db read/write failure: {0}
    State(#[from] StateError),

    /// profile-image manifest dispatch failed: {0}
    Profile(#[from] ProfileError),

    /// no active molecule for spec {0} — run `loom todo` before `loom review`
    NoActiveMolecule(String),

    /// failed to load `[test]`/`[judge]` sources for review prompt: {0}
    Spec(#[from] SpecError),

    /// spec → molecule resolution failed: {0}
    Resolve(#[from] crate::resolve::ResolveError),
}

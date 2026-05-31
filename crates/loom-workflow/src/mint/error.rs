//! Errors raised by the mint pipeline.
//!
//! Variants stay narrow so the orchestrator in `mint::mod` can map them
//! to per-batch [`crate::mint::BatchOutcome::Refused`] /
//! [`crate::mint::BatchOutcome::Errored`] outcomes rather than aborting
//! the whole run. The dedup-multi-open structural violation is
//! constructed inline at the batch boundary (no `From` impl needed
//! since the bd-list query returns a typed bead vector), so this enum
//! only carries the failure modes that traverse multiple layers.

use displaydoc::Display;
use thiserror::Error;

use loom_driver::bd::BdError;
use loom_driver::identifier::ParseBeadIdError;

use crate::resolve::ResolveError;

#[derive(Debug, Display, Error)]
pub enum MintError {
    /// bd CLI failure while minting findings
    Bd(#[from] BdError),

    /// spec → epic resolution failed while minting findings
    Resolve(#[from] ResolveError),

    /// lead epic id `{molecule}` is not a valid bead id
    InvalidParentId {
        molecule: String,
        #[source]
        source: ParseBeadIdError,
    },
}

//! Errors raised by the mint pipeline.
//!
//! Variants are kept narrow so callers (the orchestrator in `mint::mod`)
//! can pattern-match the structural-violation cases — dedup multi-open
//! and lead-epic multi-open — and surface them as per-finding [`crate::mint::FindingOutcome::Refused`]
//! rather than aborting the whole run.

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

    /// structural violation: {count} open beads share label loom:mint:{fingerprint} (ids: {ids}); close all but one before re-running
    DuplicateMintLabel {
        fingerprint: String,
        count: usize,
        ids: String,
    },

    /// lead epic id `{molecule}` is not a valid bead id
    InvalidParentId {
        molecule: String,
        #[source]
        source: ParseBeadIdError,
    },
}

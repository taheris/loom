//! Domain and protocol identifier newtypes.
//!
//! Each identifier lives in its own submodule so the family layout stays
//! flat and additive (NF-5: nested module structure, no central `types.rs`).
//!
//! Each newtype is hand-written (no shared macro) so per-identifier parse
//! rules can be enforced at construction. `From` / `Into` are intentionally
//! NOT derived (NF-8) — values enter through fallible construction so parsing
//! logic cannot be bypassed.

mod bead;
mod molecule;
mod profile;
mod request;
mod session;
mod spec;
mod tool_call;

pub use bead::{BeadId, ParseBeadIdError};
pub use molecule::{MoleculeId, ParseMoleculeIdError};
pub use profile::{ParseProfileNameError, ProfileName};
pub use request::{ParseRequestIdError, RequestId};
pub use session::{ParseSessionIdError, SessionId};
pub use spec::{ParseSpecLabelError, SpecLabel};
pub use tool_call::{ParseToolCallIdError, ToolCallId};

//! Domain and protocol identifier newtypes.
//!
//! Each identifier lives in its own submodule so the family layout stays
//! flat and additive (NF-5: nested module structure, no central `types.rs`).
//!
//! Each newtype is hand-written (no shared macro) so per-identifier parse
//! rules can be enforced at construction. `From` / `Into` are intentionally
//! NOT derived (NF-8) — values must enter the newtype through `new()` so
//! parsing logic cannot be bypassed.

mod bead;
mod molecule;
mod profile;
mod request;
mod session;
mod spec;
mod tool_call;

pub use bead::{BeadId, ParseBeadIdError};
pub use molecule::MoleculeId;
pub use profile::ProfileName;
pub use request::RequestId;
pub use session::SessionId;
pub use spec::SpecLabel;
pub use tool_call::ToolCallId;

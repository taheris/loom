//! Authenticated profile discovery and selected image provisioning.
//!
//! The driver discovers a recipe-only catalogue from the clean, signature-verified
//! published operator source. Only the requested profile/runtime is materialized
//! and rooted. Image-owned native readiness remains separate from verification
//! acceptance. No environment variable supplies profile selection metadata.

mod catalogue;
mod error;
mod manifest;
mod provision;

pub use error::ProfileError;
pub use manifest::{ImageEntry, NativePreflight, ProfileImageManifest};
pub use provision::{ProvisionError, bootstrap_loop};

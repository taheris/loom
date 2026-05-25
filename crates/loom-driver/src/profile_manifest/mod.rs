//! Profile-image manifest parser.
//!
//! The manifest is a JSON file produced by `mkProfileImages` at flake-build
//! time mapping each profile name to the podman ref + Nix store path needed
//! to spawn its image. Loom reads the manifest path from
//! `LOOM_PROFILES_MANIFEST` once at startup, parses it into a typed
//! `BTreeMap<ProfileName, ImageEntry>`, and looks each bead's profile label up
//! against it at dispatch time. See `specs/harness.md` § Profile-Image
//! Manifest.

mod error;
mod manifest;

pub use error::ProfileError;
pub use manifest::{ENV_VAR, ImageEntry, ProfileImageManifest};

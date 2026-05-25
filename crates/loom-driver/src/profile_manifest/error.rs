use std::io;
use std::path::PathBuf;

use displaydoc::Display;
use thiserror::Error;

use crate::identifier::ProfileName;

/// Errors raised while resolving the profile-image manifest.
///
/// Loom reads the manifest path from `LOOM_PROFILES_MANIFEST` at startup and
/// must fail fast — there is no implicit search path or fallback default. The
/// variants mirror the four boundary failure modes:
/// env unset, file missing, file malformed, and bead asks for a profile the
/// manifest does not declare.
#[derive(Debug, Display, Error)]
pub enum ProfileError {
    /// LOOM_PROFILES_MANIFEST is not set
    ManifestEnvUnset,

    /// profile-image manifest not found at {path}
    ManifestNotFound {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// profile-image manifest at {path} is malformed
    ManifestMalformed {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    /// profile {name} is not declared in the manifest at {manifest_path}
    UnknownProfile {
        name: ProfileName,
        manifest_path: PathBuf,
    },
}

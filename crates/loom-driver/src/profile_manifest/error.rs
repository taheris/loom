use std::io;
use std::path::PathBuf;

use displaydoc::Display;
use thiserror::Error;

use crate::agent::AgentRuntime;
use crate::identifier::ProfileName;

/// Errors raised while resolving the profile-image manifest.
///
/// Loom reads the manifest path from `LOOM_PROFILES_MANIFEST` at startup and
/// must fail fast — there is no implicit search path or fallback default.
#[derive(Debug, Display, Error)]
pub enum ProfileError {
    /// LOOM_PROFILES_MANIFEST is not set; spawn-bound commands require a
    /// profile-image manifest (enter `nix develop`, use `nix run .#loom-wrix`,
    /// or export LOOM_PROFILES_MANIFEST)
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

    /// runtime {runtime} is not declared for profile {profile} in the manifest at {manifest_path}; declared runtimes: {declared_runtimes:?}
    UnknownRuntimeForProfile {
        profile: ProfileName,
        runtime: AgentRuntime,
        declared_runtimes: Vec<AgentRuntime>,
        manifest_path: PathBuf,
    },

    /// manifest entry for profile {profile} is keyed by runtime {runtime_key} but declares runtime {entry_runtime} at {manifest_path}
    RuntimeMetadataMismatch {
        profile: ProfileName,
        runtime_key: AgentRuntime,
        entry_runtime: AgentRuntime,
        manifest_path: PathBuf,
    },
}

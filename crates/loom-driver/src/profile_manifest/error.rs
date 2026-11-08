use std::io;
use std::path::PathBuf;

use displaydoc::Display;
use thiserror::Error;

use crate::agent::AgentRuntime;
use crate::identifier::ProfileName;

/// Errors raised while resolving the profile-image manifest.
///
/// Discovery requires authenticated published operator source; malformed metadata
/// or unavailable selected outputs fail closed.
#[expect(
    clippy::doc_markdown,
    reason = "displaydoc fields are format placeholders; backticks would change the generated error text"
)]
#[derive(Debug, Display, Error)]
pub enum ProfileError {
    /// invalid profile catalogue: {reason}
    Catalogue { reason: &'static str },

    /// selected profile requires explicit authenticated provisioning
    NotProvisioned,

    /// selected profile provisioning failed
    Provision(#[source] Box<super::provision::ProvisionError>),

    /// profile-image manifest not found at {path}
    ManifestNotFound {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// profile-image manifest at {path} is malformed; regenerate it with Loom's mkProfileManifest (ref, source, source_kind and profile_config are required)
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

    /// manifest entry for profile {profile} runtime {runtime} cannot produce a valid spawn config at {manifest_path}: {reason}
    InvalidSpawnConfig {
        profile: ProfileName,
        runtime: AgentRuntime,
        reason: String,
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

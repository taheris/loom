use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agent::AgentRuntime;
use crate::identifier::ProfileName;

use super::error::ProfileError;

/// Environment variable that points at the profile-image manifest produced by
/// `wrix.lib.${system}.mkProfileImages` at flake-build time.
pub const ENV_VAR: &str = "LOOM_PROFILES_MANIFEST";

/// One manifest entry: the podman ref to spawn, the Nix store path of the
/// image archive that materializes it, and (when produced by modern wrix)
/// the content-digest file used to skip redundant image loads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageEntry {
    /// Podman ref (e.g. `localhost/wrix-rust-pi:abc123`) handed to `podman run`.
    #[serde(rename = "ref")]
    pub r#ref: String,
    /// Nix store path of the image archive handed to the launcher install step.
    pub source: PathBuf,
    /// Optional Nix store path containing the image content digest. The wrix
    /// launcher uses this for content-addressed image-cache preflight so tag
    /// changes do not force re-streaming identical layer tarballs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<PathBuf>,
}

/// Parsed profile-image manifest keyed by workspace profile, then runtime.
#[derive(Debug, Clone)]
pub struct ProfileImageManifest {
    entries: BTreeMap<ProfileName, BTreeMap<AgentRuntime, ImageEntry>>,
    manifest_path: PathBuf,
}

impl ProfileImageManifest {
    /// Read the manifest path from `LOOM_PROFILES_MANIFEST` and parse it.
    pub fn from_env() -> Result<Self, ProfileError> {
        let raw = std::env::var_os(ENV_VAR).ok_or(ProfileError::ManifestEnvUnset)?;
        Self::from_path(Path::new(&raw))
    }

    /// Parse a manifest from `path`. Read errors map to
    /// [`ProfileError::ManifestNotFound`]; JSON-shape errors map to
    /// [`ProfileError::ManifestMalformed`].
    pub fn from_path(path: &Path) -> Result<Self, ProfileError> {
        let bytes = std::fs::read(path).map_err(|source| ProfileError::ManifestNotFound {
            path: path.to_path_buf(),
            source,
        })?;
        let entries: BTreeMap<ProfileName, BTreeMap<AgentRuntime, ImageEntry>> =
            serde_json::from_slice(&bytes).map_err(|source| ProfileError::ManifestMalformed {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(Self {
            entries,
            manifest_path: path.to_path_buf(),
        })
    }

    /// Look up a profile/runtime image entry.
    pub fn lookup(
        &self,
        profile: &ProfileName,
        runtime: AgentRuntime,
    ) -> Result<&ImageEntry, ProfileError> {
        let runtimes = self
            .entries
            .get(profile)
            .ok_or_else(|| ProfileError::UnknownProfile {
                name: profile.clone(),
                manifest_path: self.manifest_path.clone(),
            })?;
        runtimes
            .get(&runtime)
            .ok_or_else(|| ProfileError::UnknownRuntimeForProfile {
                profile: profile.clone(),
                runtime,
                declared_runtimes: runtimes.keys().copied().collect(),
                manifest_path: self.manifest_path.clone(),
            })
    }

    /// Disk path the manifest was loaded from.
    pub fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }

    /// Profile names the manifest declares, in `BTreeMap` key order.
    pub fn declared_profiles(&self) -> impl ExactSizeIterator<Item = &ProfileName> {
        self.entries.keys()
    }

    /// Runtime names declared for `profile`, in `BTreeMap` key order.
    pub fn declared_runtimes(
        &self,
        profile: &ProfileName,
    ) -> Option<impl ExactSizeIterator<Item = AgentRuntime> + '_> {
        self.entries
            .get(profile)
            .map(|runtimes| runtimes.keys().copied())
    }

    /// Number of declared profiles (used by tests and the rebuild summary).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the manifest declares no profiles.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Result, anyhow};

    fn write_manifest(dir: &Path, body: &str) -> Result<PathBuf> {
        let path = dir.join("profile-images.json");
        std::fs::write(&path, body)?;
        Ok(path)
    }

    #[test]
    fn from_path_parses_well_formed_manifest() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let body = r#"{
          "base": {
            "claude": { "ref": "localhost/wrix-base-claude:abc", "source": "/nix/store/aaa-image-base-claude" },
            "pi": { "ref": "localhost/wrix-base-pi:def", "source": "/nix/store/bbb-image-base-pi" }
          },
          "rust": {
            "direct": { "ref": "localhost/wrix-rust-direct:ghi", "source": "/nix/store/ccc-image-rust-direct" }
          }
        }"#;
        let path = write_manifest(dir.path(), body)?;
        let manifest = ProfileImageManifest::from_path(&path)?;
        assert_eq!(manifest.len(), 2);
        assert_eq!(manifest.manifest_path(), path.as_path());
        let base_pi = manifest.lookup(&ProfileName::new("base"), AgentRuntime::Pi)?;
        assert_eq!(base_pi.r#ref, "localhost/wrix-base-pi:def");
        assert_eq!(
            base_pi.source,
            PathBuf::from("/nix/store/bbb-image-base-pi")
        );
        assert_eq!(base_pi.digest, None);
        let rust_direct = manifest.lookup(&ProfileName::new("rust"), AgentRuntime::Direct)?;
        assert_eq!(rust_direct.r#ref, "localhost/wrix-rust-direct:ghi");
        Ok(())
    }

    #[test]
    fn from_path_parses_optional_digest_path() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let body = r#"{
          "rust": {
            "pi": {
              "ref": "localhost/wrix-rust-pi:def",
              "source": "/nix/store/bbb-image-rust-pi",
              "digest": "/nix/store/ddd-image-digest"
            }
          }
        }"#;
        let path = write_manifest(dir.path(), body)?;
        let manifest = ProfileImageManifest::from_path(&path)?;
        let rust = manifest.lookup(&ProfileName::new("rust"), AgentRuntime::Pi)?;
        assert_eq!(
            rust.digest,
            Some(PathBuf::from("/nix/store/ddd-image-digest"))
        );
        Ok(())
    }

    #[test]
    fn from_path_rejects_unknown_runtime_before_lookup() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = write_manifest(
            dir.path(),
            r#"{ "base": { "gpt": { "ref": "r", "source": "/s" } } }"#,
        )?;
        let err = match ProfileImageManifest::from_path(&path) {
            Err(e) => e,
            Ok(_) => return Err(anyhow!("expected malformed-manifest error")),
        };
        if let ProfileError::ManifestMalformed {
            path: errored_path, ..
        } = err
        {
            assert_eq!(errored_path, path);
            Ok(())
        } else {
            Err(anyhow!("expected ManifestMalformed, got {err:?}"))
        }
    }

    #[test]
    fn from_path_missing_file_returns_manifest_not_found() -> Result<()> {
        let missing = Path::new("/does/not/exist.json");
        let err = match ProfileImageManifest::from_path(missing) {
            Err(e) => e,
            Ok(_) => return Err(anyhow!("expected error for missing manifest")),
        };
        if let ProfileError::ManifestNotFound { path, .. } = err {
            assert_eq!(path, missing);
            Ok(())
        } else {
            Err(anyhow!("expected ManifestNotFound, got {err:?}"))
        }
    }

    #[test]
    fn from_path_malformed_json_returns_manifest_malformed() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = write_manifest(dir.path(), "{ not json")?;
        let err = match ProfileImageManifest::from_path(&path) {
            Err(e) => e,
            Ok(_) => return Err(anyhow!("expected malformed-json error")),
        };
        if let ProfileError::ManifestMalformed {
            path: errored_path, ..
        } = err
        {
            assert_eq!(errored_path, path);
            Ok(())
        } else {
            Err(anyhow!("expected ManifestMalformed, got {err:?}"))
        }
    }

    #[test]
    fn declared_profiles_yields_keys_in_btreemap_order() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let body = r#"{
          "rust":   { "pi": { "ref": "r1", "source": "/s1" } },
          "base":   { "pi": { "ref": "r2", "source": "/s2" } },
          "python": { "pi": { "ref": "r3", "source": "/s3" } }
        }"#;
        let path = write_manifest(dir.path(), body)?;
        let manifest = ProfileImageManifest::from_path(&path)?;
        let names: Vec<&str> = manifest
            .declared_profiles()
            .map(ProfileName::as_str)
            .collect();
        assert_eq!(names, vec!["base", "python", "rust"]);
        Ok(())
    }

    #[test]
    fn lookup_unknown_profile_carries_manifest_path() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let body = r#"{ "base": { "pi": { "ref": "r", "source": "/s" } } }"#;
        let path = write_manifest(dir.path(), body)?;
        let manifest = ProfileImageManifest::from_path(&path)?;
        let err = match manifest.lookup(&ProfileName::new("rust"), AgentRuntime::Pi) {
            Err(e) => e,
            Ok(_) => return Err(anyhow!("expected unknown-profile error")),
        };
        if let ProfileError::UnknownProfile {
            name,
            manifest_path,
        } = err
        {
            assert_eq!(name.as_str(), "rust");
            assert_eq!(manifest_path, path);
            Ok(())
        } else {
            Err(anyhow!("expected UnknownProfile, got {err:?}"))
        }
    }

    #[test]
    fn lookup_missing_runtime_for_profile_carries_profile_and_runtime() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let body = r#"{
          "rust": {
            "claude": { "ref": "r1", "source": "/s1" },
            "pi": { "ref": "r2", "source": "/s2" }
          }
        }"#;
        let path = write_manifest(dir.path(), body)?;
        let manifest = ProfileImageManifest::from_path(&path)?;
        let err = match manifest.lookup(&ProfileName::new("rust"), AgentRuntime::Direct) {
            Err(e) => e,
            Ok(_) => return Err(anyhow!("expected unknown-runtime error")),
        };
        if let ProfileError::UnknownRuntimeForProfile {
            profile,
            runtime,
            declared_runtimes,
            manifest_path,
        } = err
        {
            assert_eq!(profile.as_str(), "rust");
            assert_eq!(runtime, AgentRuntime::Direct);
            assert_eq!(
                declared_runtimes,
                vec![AgentRuntime::Pi, AgentRuntime::Claude]
            );
            assert_eq!(manifest_path, path);
            Ok(())
        } else {
            Err(anyhow!("expected UnknownRuntimeForProfile, got {err:?}"))
        }
    }
}

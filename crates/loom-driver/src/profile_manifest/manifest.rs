use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
    /// Podman ref (e.g. `localhost/wrix-rust:abc123`) handed to `podman run`.
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

/// Parsed profile-image manifest: the typed `BTreeMap<ProfileName, ImageEntry>`
/// described in `specs/harness.md` § Profile-Image Manifest.
///
/// Constructed once at loom startup (from `LOOM_PROFILES_MANIFEST` or an
/// explicit path) and held immutably for the rest of the process lifetime.
#[derive(Debug, Clone)]
pub struct ProfileImageManifest {
    entries: BTreeMap<ProfileName, ImageEntry>,
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
        let raw: BTreeMap<String, ImageEntry> =
            serde_json::from_slice(&bytes).map_err(|source| ProfileError::ManifestMalformed {
                path: path.to_path_buf(),
                source,
            })?;
        let entries = raw
            .into_iter()
            .map(|(name, entry)| (ProfileName::new(name), entry))
            .collect();
        Ok(Self {
            entries,
            manifest_path: path.to_path_buf(),
        })
    }

    /// Look up `name`. Missing keys produce a typed
    /// [`ProfileError::UnknownProfile`] carrying the manifest path so callers
    /// can surface it in error messages without re-threading the path.
    pub fn lookup(&self, name: &ProfileName) -> Result<&ImageEntry, ProfileError> {
        self.entries
            .get(name)
            .ok_or_else(|| ProfileError::UnknownProfile {
                name: name.clone(),
                manifest_path: self.manifest_path.clone(),
            })
    }

    /// Disk path the manifest was loaded from.
    pub fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }

    /// Profile names the manifest declares, in `BTreeMap` key order. The
    /// run-loop's `unknown-profile` blocked-cause path surfaces this set
    /// in the operator-facing note so the human can relabel the bead to a
    /// declared profile without re-reading the manifest.
    pub fn declared_profiles(&self) -> impl ExactSizeIterator<Item = &ProfileName> {
        self.entries.keys()
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
          "base":   { "ref": "localhost/wrix-base:abc",   "source": "/nix/store/aaa-image-base" },
          "rust":   { "ref": "localhost/wrix-rust:def",   "source": "/nix/store/bbb-image-rust" },
          "python": { "ref": "localhost/wrix-python:ghi", "source": "/nix/store/ccc-image-python" }
        }"#;
        let path = write_manifest(dir.path(), body)?;
        let manifest = ProfileImageManifest::from_path(&path)?;
        assert_eq!(manifest.len(), 3);
        assert_eq!(manifest.manifest_path(), path.as_path());
        let rust = manifest.lookup(&ProfileName::new("rust"))?;
        assert_eq!(rust.r#ref, "localhost/wrix-rust:def");
        assert_eq!(rust.source, PathBuf::from("/nix/store/bbb-image-rust"));
        assert_eq!(rust.digest, None);
        Ok(())
    }

    #[test]
    fn from_path_parses_optional_digest_path() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let body = r#"{
          "rust": {
            "ref": "localhost/wrix-rust:def",
            "source": "/nix/store/bbb-image-rust",
            "digest": "/nix/store/ddd-image-digest"
          }
        }"#;
        let path = write_manifest(dir.path(), body)?;
        let manifest = ProfileImageManifest::from_path(&path)?;
        let rust = manifest.lookup(&ProfileName::new("rust"))?;
        assert_eq!(
            rust.digest,
            Some(PathBuf::from("/nix/store/ddd-image-digest"))
        );
        Ok(())
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

    /// `declared_profiles` walks the underlying `BTreeMap` in key order so
    /// the operator-facing `unknown-profile` blocked-cause note renders
    /// the declared set deterministically across runs.
    #[test]
    fn declared_profiles_yields_keys_in_btreemap_order() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let body = r#"{
          "rust":   { "ref": "r1", "source": "/s1" },
          "base":   { "ref": "r2", "source": "/s2" },
          "python": { "ref": "r3", "source": "/s3" }
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
        let body = r#"{ "base": { "ref": "r", "source": "/s" } }"#;
        let path = write_manifest(dir.path(), body)?;
        let manifest = ProfileImageManifest::from_path(&path)?;
        let err = match manifest.lookup(&ProfileName::new("rust")) {
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
}

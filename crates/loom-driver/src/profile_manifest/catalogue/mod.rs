//! Recipe-only discovery; no catalogue entry is a ready image.

#[cfg(test)]
mod tests;

use super::{ImageEntry, ProfileError};
use crate::{
    agent::{AgentRuntime, ImageSourceKind},
    identifier::ProfileName,
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub path: PathBuf,
    pub derivation: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub r#ref: String,
    pub source_kind: ImageSourceKind,
    pub realization: Recipe,
}

#[derive(Debug, Clone)]
pub struct Catalogue {
    pub path: PathBuf,
    pub profiles: BTreeMap<ProfileName, BTreeMap<AgentRuntime, Entry>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    schema: u32,
    profiles: BTreeMap<ProfileName, BTreeMap<AgentRuntime, Entry>>,
}

pub fn store_file(path: &Path) -> bool {
    path.parent() == Some(Path::new("/nix/store"))
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.len() > 33
                    && name.as_bytes()[32] == b'-'
                    && name.as_bytes()[..32]
                        .iter()
                        .all(|byte| b"0123456789abcdfghijklmnpqrsvwxyz".contains(byte))
            })
}

impl Catalogue {
    pub fn from_path(path: &Path) -> Result<Self, ProfileError> {
        let bytes = std::fs::read(path).map_err(|source| ProfileError::ManifestNotFound {
            path: path.to_path_buf(),
            source,
        })?;
        let raw: Raw =
            serde_json::from_slice(&bytes).map_err(|source| ProfileError::ManifestMalformed {
                path: path.to_path_buf(),
                source,
            })?;
        if raw.schema != 1 || raw.profiles.values().any(BTreeMap::is_empty) {
            return Err(ProfileError::Catalogue {
                reason: "unsupported schema or empty runtime declaration",
            });
        }
        for entry in raw.profiles.values().flat_map(BTreeMap::values) {
            if entry.r#ref.trim().is_empty()
                || !store_file(&entry.realization.path)
                || !store_file(&entry.realization.derivation)
                || entry
                    .realization
                    .path
                    .extension()
                    .is_some_and(|ext| ext == "drv")
                || entry
                    .realization
                    .derivation
                    .extension()
                    .is_none_or(|ext| ext != "drv")
            {
                return Err(ProfileError::Catalogue {
                    reason: "invalid reference or immutable recipe/output paths",
                });
            }
        }
        Ok(Self {
            path: path.to_path_buf(),
            profiles: raw.profiles,
        })
    }
}

impl Entry {
    pub fn check(&self, selected: &ImageEntry) -> Result<(), ProfileError> {
        if self.r#ref != selected.r#ref
            || self.source_kind != selected.source_kind
            || !store_file(&selected.source)
            || !store_file(&selected.profile_config)
            || !selected.source.exists()
            || !selected.profile_config.is_file()
        {
            return Err(ProfileError::Catalogue {
                reason: "realized manifest does not match the selected catalogue entry or its payloads are unavailable",
            });
        }
        Ok(())
    }
}

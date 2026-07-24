use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use displaydoc::Display;
use loom_events::identifier::ProfileName;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::identity::SkillName;

/// Provenance class for a parsed skill candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
    BuiltIn,
    Workspace,
    Configured,
    Override,
}

/// Source package shape used to resolve skill-local relative references.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceShape {
    Package,
    LooseFile,
}

/// Blake3 content hash for a skill source document.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceHash(String);

impl SourceHash {
    pub fn new(value: impl Into<String>) -> Result<Self, ParseSourceHashError> {
        value.into().parse()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SourceHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for SourceHash {
    type Err = ParseSourceHashError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != blake3::OUT_LEN * 2
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ParseSourceHashError::Invalid {
                value: value.to_owned(),
            });
        }
        Ok(Self(value.to_owned()))
    }
}

impl Serialize for SourceHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SourceHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// Skill source hash parse failures.
#[derive(Debug, Clone, PartialEq, Eq, Display, Error)]
pub enum ParseSourceHashError {
    /// invalid skill source hash `{value}`: expected 64 lowercase hexadecimal characters
    Invalid { value: String },
}

/// Parsed source identity and path provenance for a skill document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillProvenance {
    pub source: SkillSource,
    pub shape: SourceShape,
    pub document_path: PathBuf,
    pub base_dir: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tuning_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built_in_bundle: Option<ProfileName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built_in_name: Option<SkillName>,
    pub source_hash: SourceHash,
}

impl SkillProvenance {
    pub fn package(
        source: SkillSource,
        document_path: impl Into<PathBuf>,
        tuning_path: Option<PathBuf>,
        markdown: &str,
    ) -> Self {
        let document_path = document_path.into();
        let base_dir = base_dir_for(&document_path);
        Self {
            source,
            shape: SourceShape::Package,
            document_path,
            base_dir,
            tuning_path,
            built_in_bundle: None,
            built_in_name: None,
            source_hash: content_hash(markdown),
        }
    }

    pub fn loose_file(
        source: SkillSource,
        document_path: impl Into<PathBuf>,
        markdown: &str,
    ) -> Self {
        let document_path = document_path.into();
        let base_dir = base_dir_for(&document_path);
        Self {
            source,
            shape: SourceShape::LooseFile,
            document_path,
            base_dir,
            tuning_path: None,
            built_in_bundle: None,
            built_in_name: None,
            source_hash: content_hash(markdown),
        }
    }

    pub fn built_in(
        bundle: ProfileName,
        name: SkillName,
        document_path: impl Into<PathBuf>,
        markdown: &str,
    ) -> Self {
        let document_path = document_path.into();
        let base_dir = base_dir_for(&document_path);
        Self {
            source: SkillSource::BuiltIn,
            shape: SourceShape::Package,
            document_path,
            base_dir,
            tuning_path: None,
            built_in_bundle: Some(bundle),
            built_in_name: Some(name),
            source_hash: content_hash(markdown),
        }
    }
}

fn base_dir_for(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) => parent.to_path_buf(),
        None => PathBuf::new(),
    }
}

fn content_hash(markdown: &str) -> SourceHash {
    SourceHash(blake3::hash(markdown.as_bytes()).to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_hash_rejects_malformed_values_at_deserialization_boundary() {
        let valid = blake3::hash(b"skill").to_hex().to_string();
        assert_eq!(SourceHash::new(&valid).expect("valid hash").as_str(), valid);
        for malformed in ["hash".to_owned(), "A".repeat(64), "g".repeat(64)] {
            assert!(SourceHash::new(&malformed).is_err(), "{malformed}");
        }
        let error = serde_json::from_str::<SourceHash>(r#""not-a-hash""#)
            .expect_err("deserialization validates");
        assert!(error.to_string().contains("invalid skill source hash"));
    }
}

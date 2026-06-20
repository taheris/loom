use std::path::{Path, PathBuf};

use loom_events::identifier::ProfileName;
use serde::{Deserialize, Serialize};

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
    pub source_hash: String,
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

fn content_hash(markdown: &str) -> String {
    blake3::hash(markdown.as_bytes()).to_hex().to_string()
}

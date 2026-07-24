use std::path::PathBuf;

use displaydoc::Display;
use loom_events::identifier::ProfileName;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::identity::{
    ParsePhaseNameError, ParseSkillDescriptionError, ParseSkillNameError, PhaseName,
    SkillDescription, SkillName,
};
use crate::source::SkillProvenance;

/// Raw path selected by discovery before the document bytes are read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawSkillPath {
    pub path: PathBuf,
    pub provenance: SkillProvenance,
}

/// Raw Markdown plus source provenance before frontmatter parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawSkillDocument {
    markdown: String,
    provenance: SkillProvenance,
}

impl RawSkillDocument {
    pub fn new(markdown: impl Into<String>, provenance: SkillProvenance) -> Self {
        Self {
            markdown: markdown.into(),
            provenance,
        }
    }

    pub fn markdown(&self) -> &str {
        &self.markdown
    }

    pub fn provenance(&self) -> &SkillProvenance {
        &self.provenance
    }
}

/// Frontmatter fields as written before Loom-owned fields are typed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawSkillFrontmatter {
    #[serde(skip)]
    pub present: bool,
    pub name: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub metadata: RawSkillMetadata,
}

/// Raw metadata namespace carried alongside Loom metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawSkillMetadata {
    #[serde(default)]
    pub loom: RawLoomMetadata,
}

/// Raw Loom-owned filter metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawLoomMetadata {
    #[serde(default, deserialize_with = "deserialize_string_list")]
    pub phases: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_list")]
    pub profiles: Vec<String>,
}

/// Frontmatter fields Loom interprets for a registered skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillFrontmatter {
    pub name: SkillName,
    pub description: SkillDescription,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<SkillMetadata>,
}

impl SkillFrontmatter {
    pub fn from_raw(raw: &RawSkillFrontmatter) -> Result<Self, FrontmatterError> {
        if !raw.present {
            return Err(FrontmatterError::MissingFrontmatter);
        }
        let Some(name) = raw.name.as_deref().filter(|value| !value.is_empty()) else {
            return Err(FrontmatterError::MissingName);
        };
        let Some(description) = raw.description.as_deref().filter(|value| !value.is_empty()) else {
            return Err(FrontmatterError::MissingDescription);
        };
        let name = name
            .parse()
            .map_err(|source| FrontmatterError::InvalidName { source })?;
        let description = description
            .parse()
            .map_err(|source| FrontmatterError::InvalidDescription { source })?;
        let metadata = SkillMetadata::from_raw(&raw.metadata)?;
        Ok(Self {
            name,
            description,
            metadata,
        })
    }
}

/// Agent-compatible metadata namespace carried alongside Loom metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loom: Option<LoomMetadata>,
}

impl SkillMetadata {
    fn from_raw(raw: &RawSkillMetadata) -> Result<Option<Self>, FrontmatterError> {
        let loom = LoomMetadata::from_raw(&raw.loom)?;
        Ok(loom.map(|loom| Self { loom: Some(loom) }))
    }
}

/// Loom-owned filters for phase/profile applicability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoomMetadata {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<PhaseName>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<ProfileName>,
}

impl LoomMetadata {
    fn from_raw(raw: &RawLoomMetadata) -> Result<Option<Self>, FrontmatterError> {
        if raw.phases.is_empty() && raw.profiles.is_empty() {
            return Ok(None);
        }
        let phases = raw
            .phases
            .iter()
            .map(|phase| {
                phase
                    .parse()
                    .map_err(|source| FrontmatterError::InvalidPhase { source })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let profiles = raw
            .profiles
            .iter()
            .map(|profile| {
                profile
                    .parse()
                    .map_err(|_| FrontmatterError::InvalidProfile {
                        value: profile.clone(),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(Self { phases, profiles }))
    }
}

/// Parsed Markdown skill document after frontmatter extraction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillDocument {
    raw_frontmatter: RawSkillFrontmatter,
    body: String,
    markdown: String,
    provenance: SkillProvenance,
}

impl SkillDocument {
    pub fn parse(raw: RawSkillDocument) -> Result<Self, DocumentError> {
        let (frontmatter, body) = split_frontmatter(raw.markdown())?;
        Ok(Self {
            raw_frontmatter: frontmatter,
            body,
            markdown: raw.markdown,
            provenance: raw.provenance,
        })
    }

    pub fn raw_frontmatter(&self) -> &RawSkillFrontmatter {
        &self.raw_frontmatter
    }

    pub fn typed_frontmatter(&self) -> Result<SkillFrontmatter, FrontmatterError> {
        SkillFrontmatter::from_raw(&self.raw_frontmatter)
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn markdown(&self) -> &str {
        &self.markdown
    }

    pub fn provenance(&self) -> &SkillProvenance {
        &self.provenance
    }
}

/// Markdown/frontmatter parse failures before typed identity extraction.
#[derive(Debug, Display, Error)]
pub enum DocumentError {
    /// frontmatter opening marker has no closing marker
    UnterminatedFrontmatter,
    /// frontmatter is not valid YAML
    InvalidFrontmatter(#[source] serde_yaml::Error),
}

/// Typed frontmatter extraction failures.
#[derive(Debug, Display, Error)]
pub enum FrontmatterError {
    /// skill frontmatter is missing
    MissingFrontmatter,
    /// skill frontmatter is missing required `name`
    MissingName,
    /// skill frontmatter is missing required `description`
    MissingDescription,
    /// invalid skill name
    InvalidName {
        #[source]
        source: ParseSkillNameError,
    },
    /// invalid skill description
    InvalidDescription {
        #[source]
        source: ParseSkillDescriptionError,
    },
    /// invalid phase filter
    InvalidPhase {
        #[source]
        source: ParsePhaseNameError,
    },
    /// invalid profile filter `{value}`
    InvalidProfile { value: String },
}

fn split_frontmatter(markdown: &str) -> Result<(RawSkillFrontmatter, String), DocumentError> {
    let mut lines = markdown.split_inclusive('\n');
    let Some(first_line) = lines.next() else {
        return Ok((RawSkillFrontmatter::default(), String::new()));
    };
    if trim_line_end(first_line) != "---" {
        return Ok((RawSkillFrontmatter::default(), markdown.to_string()));
    }

    let mut offset = first_line.len();
    let frontmatter_start = offset;
    for line in lines {
        let line_start = offset;
        offset = offset.saturating_add(line.len());
        let marker = trim_line_end(line);
        if marker == "---" || marker == "..." {
            let raw = parse_raw_frontmatter(&markdown[frontmatter_start..line_start])?;
            return Ok((raw, markdown[offset..].to_string()));
        }
    }
    Err(DocumentError::UnterminatedFrontmatter)
}

fn trim_line_end(line: &str) -> &str {
    line.trim_end_matches(['\r', '\n'])
}

fn parse_raw_frontmatter(block: &str) -> Result<RawSkillFrontmatter, DocumentError> {
    let mut raw = serde_yaml::from_str::<RawSkillFrontmatter>(block)
        .map_err(DocumentError::InvalidFrontmatter)?;
    raw.present = true;
    Ok(raw)
}

fn deserialize_string_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringList {
        One(String),
        Many(Vec<String>),
    }

    match Option::<StringList>::deserialize(deserializer)? {
        None => Ok(Vec::new()),
        Some(StringList::One(value)) => Ok(vec![value]),
        Some(StringList::Many(values)) => Ok(values),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{SkillSource, SourceHash, SourceShape};

    fn raw(markdown: &str) -> RawSkillDocument {
        RawSkillDocument::new(
            markdown,
            SkillProvenance {
                source: SkillSource::Workspace,
                shape: SourceShape::LooseFile,
                document_path: PathBuf::from("skill.md"),
                base_dir: PathBuf::new(),
                tuning_path: None,
                built_in_bundle: None,
                built_in_name: None,
                source_hash: SourceHash::new(
                    blake3::hash(markdown.as_bytes()).to_hex().to_string(),
                )
                .expect("valid source hash"),
            },
        )
    }

    #[test]
    fn frontmatter_extracts_typed_identity_and_filters() {
        let document = SkillDocument::parse(raw(r#"---
name: rust-review
description: Use when reviewing Rust changes.
metadata:
  loom:
    phases: ["loop", "gate.review"]
    profiles:
      - rust
---
Body
"#))
        .expect("frontmatter parses");
        let frontmatter = document.typed_frontmatter().expect("typed frontmatter");
        assert_eq!(frontmatter.name.as_str(), "rust-review");
        assert_eq!(
            frontmatter.description.as_str(),
            "Use when reviewing Rust changes."
        );
        let loom = frontmatter
            .metadata
            .expect("metadata")
            .loom
            .expect("loom metadata");
        assert_eq!(loom.phases.len(), 2);
        assert_eq!(loom.profiles[0].as_str(), "rust");
    }

    #[test]
    fn yaml_frontmatter_supports_escaped_and_folded_scalars() {
        let document = SkillDocument::parse(raw(
            "---\nname: rust-review\ndescription: >-\n  Use when reviewing \\\"quoted\\\" Rust changes.\nmetadata:\n  loom:\n    phases: loop\n---\nBody\n",
        ))
        .expect("valid YAML parses");
        let frontmatter = document.typed_frontmatter().expect("typed frontmatter");
        assert_eq!(
            frontmatter.description.as_str(),
            "Use when reviewing \\\"quoted\\\" Rust changes."
        );
        assert_eq!(
            frontmatter
                .metadata
                .expect("metadata")
                .loom
                .expect("loom metadata")
                .phases[0]
                .as_str(),
            "loop"
        );
    }

    #[test]
    fn malformed_yaml_is_a_document_error_even_in_unknown_fields() {
        let error = SkillDocument::parse(raw(
            "---\nname: rust-review\ndescription: Valid description.\nunknown: [unterminated\n---\nBody\n",
        ))
        .expect_err("malformed YAML rejects");
        assert!(matches!(error, DocumentError::InvalidFrontmatter(_)));
    }

    #[test]
    fn missing_frontmatter_is_typed_error() {
        let document = SkillDocument::parse(raw("# no frontmatter\n")).expect("markdown parses");
        assert!(matches!(
            document.typed_frontmatter(),
            Err(FrontmatterError::MissingFrontmatter)
        ));
    }
}

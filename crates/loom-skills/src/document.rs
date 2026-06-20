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
    pub present: bool,
    pub name: Option<String>,
    pub description: Option<String>,
    pub metadata: RawSkillMetadata,
}

/// Raw metadata namespace carried alongside Loom metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawSkillMetadata {
    pub loom: RawLoomMetadata,
}

/// Raw Loom-owned filter metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawLoomMetadata {
    pub phases: Vec<String>,
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

#[derive(Debug, Clone)]
struct StackEntry {
    indent: usize,
    key: String,
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
            let raw = parse_raw_frontmatter(&markdown[frontmatter_start..line_start]);
            return Ok((raw, markdown[offset..].to_string()));
        }
    }
    Err(DocumentError::UnterminatedFrontmatter)
}

fn trim_line_end(line: &str) -> &str {
    line.trim_end_matches(['\r', '\n'])
}

fn parse_raw_frontmatter(block: &str) -> RawSkillFrontmatter {
    let mut raw = RawSkillFrontmatter {
        present: true,
        ..RawSkillFrontmatter::default()
    };
    let mut stack = Vec::new();
    for raw_line in block.lines() {
        let line = raw_line.trim_end();
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len().saturating_sub(trimmed.len());
        while stack
            .last()
            .is_some_and(|entry: &StackEntry| indent <= entry.indent)
        {
            stack.pop();
        }
        if let Some(item) = trimmed.strip_prefix("- ") {
            apply_sequence_item(&mut raw, &stack, item.trim());
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        apply_key_value(&mut raw, &stack, key, value);
        if value.is_empty() {
            stack.push(StackEntry {
                indent,
                key: key.to_string(),
            });
        }
    }
    raw
}

fn apply_sequence_item(raw: &mut RawSkillFrontmatter, stack: &[StackEntry], item: &str) {
    let path = stack_path(stack);
    if path == ["metadata", "loom", "phases"] {
        raw.metadata.loom.phases.push(unquote_scalar(item));
    } else if path == ["metadata", "loom", "profiles"] {
        raw.metadata.loom.profiles.push(unquote_scalar(item));
    }
}

fn apply_key_value(raw: &mut RawSkillFrontmatter, stack: &[StackEntry], key: &str, value: &str) {
    let mut path = stack_path(stack);
    path.push(key.to_string());
    match path.as_slice() {
        [name] if name == "name" && !value.is_empty() => {
            raw.name = Some(unquote_scalar(value));
        }
        [description] if description == "description" && !value.is_empty() => {
            raw.description = Some(unquote_scalar(value));
        }
        [metadata, loom, phases]
            if metadata == "metadata" && loom == "loom" && phases == "phases" =>
        {
            raw.metadata.loom.phases = parse_list_or_scalar(value);
        }
        [metadata, loom, profiles]
            if metadata == "metadata" && loom == "loom" && profiles == "profiles" =>
        {
            raw.metadata.loom.profiles = parse_list_or_scalar(value);
        }
        _ => {}
    }
}

fn stack_path(stack: &[StackEntry]) -> Vec<String> {
    stack.iter().map(|entry| entry.key.clone()).collect()
}

fn parse_list_or_scalar(value: &str) -> Vec<String> {
    if value.is_empty() {
        return Vec::new();
    }
    let trimmed = value.trim();
    if let Some(inner) = trimmed
        .strip_prefix('[')
        .and_then(|without_open| without_open.strip_suffix(']'))
    {
        return split_top_level_commas(inner)
            .into_iter()
            .filter(|part| !part.trim().is_empty())
            .map(|part| unquote_scalar(part.trim()))
            .collect();
    }
    vec![unquote_scalar(trimmed)]
}

fn split_top_level_commas(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut quote: Option<char> = None;
    for (idx, ch) in input.char_indices() {
        match (quote, ch) {
            (Some(current), c) if c == current => quote = None,
            (None, '\'' | '"') => quote = Some(ch),
            (None, ',') => {
                parts.push(&input[start..idx]);
                start = idx.saturating_add(ch.len_utf8());
            }
            _ => {}
        }
    }
    parts.push(&input[start..]);
    parts
}

fn unquote_scalar(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 {
        let mut chars = trimmed.chars();
        let first = chars.next();
        let last = trimmed.chars().next_back();
        if matches!(
            (first, last),
            (Some('"'), Some('"')) | (Some('\''), Some('\''))
        ) {
            let start = first.map_or(0, char::len_utf8);
            let end = trimmed.len().saturating_sub(last.map_or(0, char::len_utf8));
            return trimmed[start..end].to_string();
        }
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{SkillSource, SourceShape};

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
                source_hash: "hash".to_string(),
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
    fn missing_frontmatter_is_typed_error() {
        let document = SkillDocument::parse(raw("# no frontmatter\n")).expect("markdown parses");
        assert!(matches!(
            document.typed_frontmatter(),
            Err(FrontmatterError::MissingFrontmatter)
        ));
    }
}

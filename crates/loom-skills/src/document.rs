use loom_events::identifier::ProfileName;
use serde::{Deserialize, Serialize};

use crate::identity::{PhaseName, SkillDescription, SkillName};

/// Frontmatter fields Loom interprets for a registered skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillFrontmatter {
    pub name: SkillName,
    pub description: SkillDescription,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<SkillMetadata>,
}

/// Agent-compatible metadata namespace carried alongside Loom metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loom: Option<LoomMetadata>,
}

/// Loom-owned filters for phase/profile applicability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoomMetadata {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<PhaseName>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<ProfileName>,
}

/// Parsed Markdown skill document after frontmatter extraction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillDocument {
    frontmatter: SkillFrontmatter,
    body: String,
}

impl SkillDocument {
    pub fn new(frontmatter: SkillFrontmatter, body: String) -> Self {
        Self { frontmatter, body }
    }

    pub fn frontmatter(&self) -> &SkillFrontmatter {
        &self.frontmatter
    }

    pub fn body(&self) -> &str {
        &self.body
    }
}

use std::path::PathBuf;

use serde::Deserialize;

/// `[skills]` registry/disclosure policy loaded from `loom.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SkillsConfig {
    pub registration: SkillRegistration,
    pub show_paths: SkillPathDisplay,
    pub paths: Vec<PathBuf>,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            registration: SkillRegistration::Auto,
            show_paths: SkillPathDisplay::Needed,
            paths: Vec::new(),
        }
    }
}

/// User policy for native skill registration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillRegistration {
    #[default]
    Auto,
    Prompt,
}

/// Prompt path visibility policy for skill indexes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillPathDisplay {
    #[default]
    Needed,
    Always,
}

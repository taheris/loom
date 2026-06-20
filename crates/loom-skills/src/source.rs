use serde::{Deserialize, Serialize};

/// Provenance class for a parsed skill candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
    BuiltIn,
    Workspace,
    Configured,
    Override,
}

use crate::bd::{IssueType, Priority};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct BeadsConfig {
    pub priority: Priority,
    pub default_type: IssueType,
}

impl Default for BeadsConfig {
    fn default() -> Self {
        Self {
            priority: Priority::P2,
            default_type: IssueType::Task,
        }
    }
}

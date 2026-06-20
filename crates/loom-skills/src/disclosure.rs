use serde::{Deserialize, Serialize};

/// User policy for selecting native registration or prompt disclosure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationPolicy {
    Auto,
    Prompt,
}

/// Effective disclosure mode selected for a materialized registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisclosureMode {
    Native,
    Prompt,
}

/// Path visibility policy for the compact skill index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathDisplay {
    Needed,
    Always,
}

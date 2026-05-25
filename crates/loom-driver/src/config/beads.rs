use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct BeadsConfig {
    pub priority: u8,
    pub default_type: String,
}

impl Default for BeadsConfig {
    fn default() -> Self {
        Self {
            priority: 2,
            default_type: "task".to_string(),
        }
    }
}

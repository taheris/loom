use serde::Deserialize;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    /// Tool names denied at host-side `control_request` time. Claude only.
    pub denied_tools: Vec<String>,
}

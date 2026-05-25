use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ClaudeConfig {
    /// Seconds to wait for clean exit after `result` before SIGTERM.
    pub post_result_grace_secs: u32,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            post_result_grace_secs: 5,
        }
    }
}

use serde::Deserialize;

/// `[direct]` block from `<workspace>/loom.toml` — Direct-backend runtime
/// settings, symmetric with [`super::ClaudeConfig`]. Applied wherever the
/// direct backend is selected; resolved into
/// [`crate::agent::SpawnConfig::output_limits`] at dispatch time.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct DirectConfig {
    /// Raw-UTF-8 byte budget a content-returning Direct tool may place inline
    /// before offloading the full payload to the scratch offload directory
    /// (`specs/agent.md` § Direct Output Bounding). Defaults to 16384.
    pub max_inline_bytes: usize,
}

impl Default for DirectConfig {
    fn default() -> Self {
        Self {
            max_inline_bytes: 16384,
        }
    }
}

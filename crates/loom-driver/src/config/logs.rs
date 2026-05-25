use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct LogsConfig {
    /// Days to retain `.wrapix/loom/logs/` files. `0` disables sweeping.
    pub retention_days: u32,
}

impl Default for LogsConfig {
    fn default() -> Self {
        Self { retention_days: 14 }
    }
}

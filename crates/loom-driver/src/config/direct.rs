use serde::{Deserialize, Serialize};

/// Direct-backend settings from `<workspace>/loom.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct DirectConfig {
    pub max_inline_bytes: InlineByteLimit,
}

/// Content budget large enough to admit every UTF-8 scalar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "usize")]
pub struct InlineByteLimit(usize);

impl InlineByteLimit {
    pub const fn get(self) -> usize {
        self.0
    }
}

impl Default for InlineByteLimit {
    fn default() -> Self {
        Self(16_384)
    }
}

impl TryFrom<usize> for InlineByteLimit {
    type Error = InvalidInlineByteLimit;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        if value < 4 {
            Err(InvalidInlineByteLimit)
        } else {
            Ok(Self(value))
        }
    }
}

/// Direct `max_inline_bytes` must be at least four
#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub struct InvalidInlineByteLimit;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{agent::OutputLimits, config::LoomConfig};

    #[test]
    fn direct_rejects_sub_four_byte_caps_at_config_and_spawn_boundaries() {
        for cap in 0..4 {
            assert!(InlineByteLimit::try_from(cap).is_err());
            assert!(
                LoomConfig::from_toml_str(&format!("[direct]\nmax_inline_bytes = {cap}")).is_err()
            );
            assert!(
                serde_json::from_value::<OutputLimits>(
                    serde_json::json!({"max_inline_bytes": cap})
                )
                .is_err()
            );
        }
        for cap in [4, 16_384, 32_768] {
            let config =
                LoomConfig::from_toml_str(&format!("[direct]\nmax_inline_bytes = {cap}")).unwrap();
            let wire = serde_json::to_value(config.direct_output_limits()).unwrap();
            assert_eq!(wire["max_inline_bytes"], cap);
            let limits: OutputLimits = serde_json::from_value(wire).unwrap();
            assert_eq!(limits.max_inline_bytes.get(), cap);
        }
    }
}

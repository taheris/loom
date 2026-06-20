use std::fmt;
use std::path::PathBuf;

use displaydoc::Display;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::checker::{CheckerId, Registry, RegistryError};

/// Top-level `loom.toml` tuning fragment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FileConfig {
    pub tune: TuneConfig,
}

/// `[tune]` configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct TuneConfig {
    pub evidence: EvidenceConfig,
    pub checks: ChecksConfig,
}

impl TuneConfig {
    pub fn disabled_checkers(
        &self,
        registry: &Registry,
    ) -> Result<std::collections::BTreeSet<CheckerId>, RegistryError> {
        registry.validate_disabled(&self.checks.disabled)
    }
}

/// `[tune.evidence]` configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EvidenceConfig {
    pub selection_fraction: SelectionFraction,
    pub external_roots: Vec<PathBuf>,
}

/// `[tune.checks]` configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChecksConfig {
    pub max_behavior_cases: usize,
    pub max_wall_time_secs: u64,
    pub max_llm_judge_calls: usize,
    pub disabled: Vec<CheckerId>,
}

impl Default for ChecksConfig {
    fn default() -> Self {
        Self {
            max_behavior_cases: 3,
            max_wall_time_secs: 1_800,
            max_llm_judge_calls: 10,
            disabled: Vec::new(),
        }
    }
}

/// Fraction of mined evidence withheld for selection checks.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct SelectionFraction(f64);

impl SelectionFraction {
    pub fn new(value: f64) -> Result<Self, SelectionFractionError> {
        if value.is_finite() && value > 0.0 && value < 1.0 {
            Ok(Self(value))
        } else {
            Err(SelectionFractionError::OutOfRange { value })
        }
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

impl Default for SelectionFraction {
    fn default() -> Self {
        Self(0.34)
    }
}

impl fmt::Display for SelectionFraction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for SelectionFraction {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.0)
    }
}

impl<'de> Deserialize<'de> for SelectionFraction {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = f64::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Selection fraction construction failures.
#[derive(Debug, Clone, PartialEq, Display, Error)]
pub enum SelectionFractionError {
    /// selection_fraction `{value}` must satisfy 0.0 < value < 1.0
    OutOfRange { value: f64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tune_config_defaults_match_v1_policy() {
        let config = TuneConfig::default();
        assert_eq!(config.evidence.selection_fraction.get(), 0.34);
        assert!(config.evidence.external_roots.is_empty());
        assert_eq!(config.checks.max_behavior_cases, 3);
        assert_eq!(config.checks.max_wall_time_secs, 1_800);
        assert_eq!(config.checks.max_llm_judge_calls, 10);
        assert!(config.checks.disabled.is_empty());
    }

    #[test]
    fn selection_fraction_rejects_zero_one_nan_and_out_of_range() {
        for value in [0.0, 1.0, f64::NAN, -0.1, 1.1] {
            assert!(SelectionFraction::new(value).is_err(), "{value}");
        }
        assert_eq!(SelectionFraction::new(0.5).expect("valid").get(), 0.5);
    }

    #[test]
    fn config_deserialize_rejects_unknown_fields() {
        let err = toml::from_str::<TuneConfig>("unknown = true").expect_err("unknown rejects");
        assert!(err.to_string().contains("unknown"), "{err}");
    }
}

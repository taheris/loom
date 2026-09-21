//! Open model and provider names supplied by external registries.
use displaydoc::Display;
use serde::{Deserialize, Deserializer, Serialize};
use std::{fmt, str::FromStr};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ModelName(String);

impl ModelName {
    /// Parse a nonempty model name without whitespace or control characters.
    /// # Errors
    /// Rejects empty or whitespace-bearing identifiers.
    pub fn new(value: impl AsRef<str>) -> Result<Self, ParseModelNameError> {
        value.as_ref().parse()
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl FromStr for ModelName {
    type Err = ParseModelNameError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if valid_token(value) {
            Ok(Self(value.into()))
        } else {
            Err(ParseModelNameError(value.into()))
        }
    }
}
impl std::ops::Deref for ModelName {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}
impl fmt::Display for ModelName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl<'de> Deserialize<'de> for ModelName {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        String::deserialize(de)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}
#[derive(Debug, Display, Error, PartialEq, Eq)]
/// invalid model name `{0}`: expected a nonempty token without whitespace
pub struct ParseModelNameError(pub String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ProviderName(String);

impl ProviderName {
    /// Parse a nonempty provider name without whitespace or control characters.
    /// # Errors
    /// Rejects empty or whitespace-bearing identifiers.
    pub fn new(value: impl AsRef<str>) -> Result<Self, ParseProviderNameError> {
        value.as_ref().parse()
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl FromStr for ProviderName {
    type Err = ParseProviderNameError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if valid_token(value) {
            Ok(Self(value.into()))
        } else {
            Err(ParseProviderNameError(value.into()))
        }
    }
}
impl std::ops::Deref for ProviderName {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}
impl fmt::Display for ProviderName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl<'de> Deserialize<'de> for ProviderName {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        String::deserialize(de)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}
#[derive(Debug, Display, Error, PartialEq, Eq)]
/// invalid provider name `{0}`: expected a nonempty token without whitespace
pub struct ParseProviderNameError(pub String);

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && !value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn external_names_reject_empty_tokens_at_all_boundaries() {
        for name in ["", " ", "two words", "nul\0byte"] {
            assert!(ModelName::new(name).is_err());
            assert!(ProviderName::new(name).is_err());
            let json = serde_json::to_string(name).unwrap();
            assert!(serde_json::from_str::<ModelName>(&json).is_err());
            assert!(serde_json::from_str::<ProviderName>(&json).is_err());
        }
    }
    #[test]
    fn external_names_preserve_forward_compatible_provider_tokens() {
        for name in [
            "claude-sonnet-4-6",
            "custom/model:version",
            "vendor.example",
        ] {
            let model = ModelName::new(name).unwrap();
            let provider = ProviderName::new(name).unwrap();
            assert_eq!(model.as_str(), name);
            assert_eq!(provider.as_str(), name);
            assert_eq!(
                serde_json::to_string(&model).unwrap(),
                serde_json::to_string(name).unwrap()
            );
        }
    }
}

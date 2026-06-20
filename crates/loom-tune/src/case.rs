use std::fmt;
use std::str::FromStr;

use displaydoc::Display;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

/// Globally unique kebab-case identifier for a `loom-case` block.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CaseId(String);

impl CaseId {
    pub fn new(value: impl Into<String>) -> Result<Self, ParseCaseIdError> {
        value.into().parse()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CaseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for CaseId {
    type Err = ParseCaseIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() {
            return Err(ParseCaseIdError::Invalid {
                value: value.to_owned(),
            });
        }
        if value.starts_with('-') || value.ends_with('-') || value.contains("--") {
            return Err(ParseCaseIdError::Invalid {
                value: value.to_owned(),
            });
        }
        if !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(ParseCaseIdError::Invalid {
                value: value.to_owned(),
            });
        }
        Ok(Self(value.to_owned()))
    }
}

impl<'de> Deserialize<'de> for CaseId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// Case id parse failures.
#[derive(Debug, Clone, PartialEq, Eq, Display, Error)]
pub enum ParseCaseIdError {
    /// invalid loom-case id `{value}`: expected lowercase kebab-case
    Invalid { value: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_id_accepts_kebab_case() {
        let id = CaseId::new("rust-review-regression").expect("valid case id");
        assert_eq!(id.as_str(), "rust-review-regression");
    }

    #[test]
    fn case_id_rejects_non_kebab_case() {
        for input in ["", "Rust", "has space", "double--dash", "trailing-"] {
            assert!(CaseId::new(input).is_err(), "{input}");
        }
    }
}

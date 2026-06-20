use std::fmt;
use std::str::FromStr;

use displaydoc::Display;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

/// Internal checker id in `kind.domain.name` form.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CheckerId(String);

impl CheckerId {
    pub fn new(value: impl Into<String>) -> Result<Self, ParseCheckerIdError> {
        value.into().parse()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CheckerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for CheckerId {
    type Err = ParseCheckerIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = value.split('.').collect();
        if parts.len() != 3 || parts.iter().any(|part| !valid_segment(part)) {
            return Err(ParseCheckerIdError::Invalid {
                value: value.to_owned(),
            });
        }
        Ok(Self(value.to_owned()))
    }
}

impl<'de> Deserialize<'de> for CheckerId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

fn valid_segment(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && !value.ends_with('-')
        && !value.contains("--")
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Checker id parse failures.
#[derive(Debug, Clone, PartialEq, Eq, Display, Error)]
pub enum ParseCheckerIdError {
    /// invalid checker id `{value}`: expected `kind.domain.name`
    Invalid { value: String },
}

/// Top-level checker class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckerKind {
    Preflight,
    Behavior,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checker_id_requires_three_kebab_segments() {
        let id = CheckerId::new("behavior.skill.no-drift").expect("valid checker id");
        assert_eq!(id.as_str(), "behavior.skill.no-drift");
        for input in ["behavior", "behavior.skill", "Behavior.skill.case", "a..b"] {
            assert!(CheckerId::new(input).is_err(), "{input}");
        }
    }
}

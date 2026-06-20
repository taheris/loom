use std::fmt;
use std::str::FromStr;

use displaydoc::Display;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

const MAX_NAME_CHARS: usize = 64;
const MAX_DESCRIPTION_CHARS: usize = 1024;
const MAX_PHASE_CHARS: usize = 64;

/// Agent Skill name compatible with Loom, Pi, and directory package names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SkillName(String);

impl SkillName {
    pub fn new(value: impl Into<String>) -> Result<Self, ParseSkillNameError> {
        value.into().parse()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SkillName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SkillName {
    type Err = ParseSkillNameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let char_count = value.chars().count();
        if char_count == 0 {
            return Err(ParseSkillNameError::Empty);
        }
        if char_count > MAX_NAME_CHARS {
            return Err(ParseSkillNameError::TooLong {
                value: value.to_owned(),
            });
        }
        if value.starts_with('-') || value.ends_with('-') {
            return Err(ParseSkillNameError::EdgeHyphen {
                value: value.to_owned(),
            });
        }
        if value.contains("--") {
            return Err(ParseSkillNameError::ConsecutiveHyphen {
                value: value.to_owned(),
            });
        }
        if !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(ParseSkillNameError::InvalidCharacter {
                value: value.to_owned(),
            });
        }
        Ok(Self(value.to_owned()))
    }
}

impl<'de> Deserialize<'de> for SkillName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// Skill name parse failures.
#[derive(Debug, Clone, PartialEq, Eq, Display, Error)]
pub enum ParseSkillNameError {
    /// skill name is empty
    Empty,
    /// skill name `{value}` exceeds 64 characters
    TooLong { value: String },
    /// skill name `{value}` must use lowercase ASCII letters, digits, and hyphens
    InvalidCharacter { value: String },
    /// skill name `{value}` must not start or end with a hyphen
    EdgeHyphen { value: String },
    /// skill name `{value}` must not contain consecutive hyphens
    ConsecutiveHyphen { value: String },
}

/// Human-readable routing signal shown in compact skill indexes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SkillDescription(String);

impl SkillDescription {
    pub fn new(value: impl Into<String>) -> Result<Self, ParseSkillDescriptionError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ParseSkillDescriptionError::Empty);
        }
        if value.chars().count() > MAX_DESCRIPTION_CHARS {
            return Err(ParseSkillDescriptionError::TooLong { value });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SkillDescription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SkillDescription {
    type Err = ParseSkillDescriptionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for SkillDescription {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// Skill description parse failures.
#[derive(Debug, Clone, PartialEq, Eq, Display, Error)]
pub enum ParseSkillDescriptionError {
    /// skill description is empty
    Empty,
    /// skill description `{value}` exceeds 1024 characters
    TooLong { value: String },
}

/// Workflow phase selector carried by skill applicability filters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PhaseName(String);

impl PhaseName {
    pub fn new(value: impl Into<String>) -> Result<Self, ParsePhaseNameError> {
        value.into().parse()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn matches(&self, current: &Self) -> bool {
        self == current || current.0.split('.').any(|part| part == self.0)
    }
}

impl fmt::Display for PhaseName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for PhaseName {
    type Err = ParsePhaseNameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty()
            || value.chars().count() > MAX_PHASE_CHARS
            || value.starts_with(['-', '.'])
            || value.ends_with(['-', '.'])
            || value.contains("--")
            || value.contains("..")
            || !value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        {
            return Err(ParsePhaseNameError::Invalid {
                value: value.to_owned(),
            });
        }
        Ok(Self(value.to_owned()))
    }
}

impl<'de> Deserialize<'de> for PhaseName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// Phase name parse failures.
#[derive(Debug, Clone, PartialEq, Eq, Display, Error)]
pub enum ParsePhaseNameError {
    /// invalid phase name `{value}`: expected lowercase kebab-case
    Invalid { value: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_name_accepts_agent_skill_shape() {
        let name = SkillName::new("rust-review-1").expect("valid skill name");
        assert_eq!(name.as_str(), "rust-review-1");
    }

    #[test]
    fn skill_name_rejects_incompatible_shapes() {
        let too_long = "a".repeat(MAX_NAME_CHARS + 1);
        let cases = [
            ("", ParseSkillNameError::Empty),
            (
                "-leading",
                ParseSkillNameError::EdgeHyphen {
                    value: "-leading".to_string(),
                },
            ),
            (
                "trailing-",
                ParseSkillNameError::EdgeHyphen {
                    value: "trailing-".to_string(),
                },
            ),
            (
                "double--dash",
                ParseSkillNameError::ConsecutiveHyphen {
                    value: "double--dash".to_string(),
                },
            ),
            (
                "Rust",
                ParseSkillNameError::InvalidCharacter {
                    value: "Rust".to_string(),
                },
            ),
            (
                too_long.as_str(),
                ParseSkillNameError::TooLong {
                    value: too_long.clone(),
                },
            ),
        ];
        for (input, want) in cases {
            assert_eq!(input.parse::<SkillName>().expect_err(input), want);
        }
    }

    #[test]
    fn skill_name_deserializes_through_parser() {
        let err = serde_json::from_str::<SkillName>("\"not valid\"").expect_err("invalid");
        assert!(err.to_string().contains("skill name"), "{err}");
    }

    #[test]
    fn skill_description_enforces_non_empty_and_limit() {
        assert_eq!(
            SkillDescription::new("").expect_err("empty"),
            ParseSkillDescriptionError::Empty,
        );
        let ok = SkillDescription::new("Use for Rust reviews.").expect("valid description");
        assert_eq!(ok.as_str(), "Use for Rust reviews.");
    }

    #[test]
    fn phase_name_uses_kebab_case_filter_identity() {
        let phase = PhaseName::new("gate.review").expect("valid phase");
        assert_eq!(phase.as_str(), "gate.review");
        assert!(PhaseName::new("Gate Review").is_err());
        assert!(
            PhaseName::new("gate")
                .expect("valid filter")
                .matches(&phase)
        );
    }
}

//! Requirement identity and parsed verifier metadata, without execution or evidence authority.

use std::fmt;
use std::str::FromStr;

use displaydoc::Display;
use loom_events::identifier::SpecLabel;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

/// Stable label-dependent identifier for a normalized requirement.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CriterionId(String);

impl CriterionId {
    /// Parse the canonical criterion identifier shape.
    ///
    /// # Errors
    /// Rejects identifiers without exactly sixteen lowercase hexadecimal digits.
    pub fn new(value: impl Into<String>) -> Result<Self, ParseCriterionIdError> {
        let value = value.into();
        let valid = value.strip_prefix("criterion-").is_some_and(|hex| {
            hex.len() == 16
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        if !valid {
            return Err(ParseCriterionIdError { value });
        }
        Ok(Self(value))
    }

    /// Derive identity from structurally extracted requirement text, excluding verifier metadata.
    pub fn for_spec_text(spec_label: &SpecLabel, criterion_text: &str) -> Self {
        let canonical = format!(
            "{}\0{}",
            spec_label.as_str(),
            criterion_text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        );
        let digest = blake3::hash(canonical.as_bytes()).to_hex().to_string();
        Self(format!("criterion-{}", &digest[..16]))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for CriterionId {
    type Err = ParseCriterionIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl fmt::Display for CriterionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CriterionId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// invalid criterion id `{value}`: expected `criterion-` followed by 16 lowercase hex characters
#[derive(Debug, Clone, PartialEq, Eq, Display, Error)]
pub struct ParseCriterionIdError {
    pub value: String,
}

/// Parsed metadata; a binding is not an admitted executable or a verifier result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CriterionAnnotation {
    pub tier: AnnotationTier,
    pub target: AnnotationTarget,
    pub pending: bool,
}

impl fmt::Display for CriterionAnnotation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pending = if self.pending { "?" } else { "" };
        write!(f, "[{}{pending}]({})", self.tier.as_str(), self.target)
    }
}

/// The four supported verifier annotation tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationTier {
    Check,
    Test,
    System,
    Judge,
}

impl AnnotationTier {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Test => "test",
            Self::System => "system",
            Self::Judge => "judge",
        }
    }
}

/// Exact target bytes from an annotation, not an executable-admission certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotationTarget(String);

impl AnnotationTarget {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AnnotationTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn criterion_id_deserialization_checks_shape() {
        let id = CriterionId::for_spec_text(&SpecLabel::new("specs").unwrap(), "Requirement");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<CriterionId>(&json).unwrap(), id);
        for malformed in [
            "",
            "criterion-0123456789abcde",
            "criterion-0123456789ABCDEf",
        ] {
            assert!(serde_json::from_value::<CriterionId>(serde_json::json!(malformed)).is_err());
        }
    }
}

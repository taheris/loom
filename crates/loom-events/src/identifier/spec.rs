use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct SpecLabel(String);

impl SpecLabel {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SpecLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SpecLabel {
    type Err = ParseSpecLabelError;

    /// Parse a kebab-case spec label: lowercase ASCII letters and digits
    /// separated by single `-` characters, non-empty, no leading or
    /// trailing `-`. Matches the file names under `specs/*.md`
    /// (`harness`, `gate`, …) and rejects whitespace,
    /// uppercase, and empty strings so external input (CLI args, JSON
    /// from `bd`) cannot smuggle in a malformed label.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(ParseSpecLabelError(s.to_owned()));
        }
        let bytes = s.as_bytes();
        if bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
            return Err(ParseSpecLabelError(s.to_owned()));
        }
        let mut prev_dash = false;
        for &b in bytes {
            let ok = b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-';
            if !ok {
                return Err(ParseSpecLabelError(s.to_owned()));
            }
            if b == b'-' && prev_dash {
                return Err(ParseSpecLabelError(s.to_owned()));
            }
            prev_dash = b == b'-';
        }
        Ok(Self(s.to_owned()))
    }
}

impl<'de> Deserialize<'de> for SpecLabel {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid spec label `{0}`: expected lowercase ASCII kebab-case")]
pub struct ParseSpecLabelError(pub String);

#[cfg(test)]
mod tests {
    use super::{ParseSpecLabelError, SpecLabel};
    use anyhow::Result;

    #[test]
    fn display_round_trips_with_as_str() {
        let label = SpecLabel::new("harness");
        assert_eq!(label.as_str(), "harness");
        assert_eq!(label.to_string(), "harness");
    }

    #[test]
    fn serde_round_trips_as_plain_string() -> Result<()> {
        let label = SpecLabel::new("gate");
        let json = serde_json::to_string(&label)?;
        assert_eq!(json, "\"gate\"");
        let back: SpecLabel = serde_json::from_str(&json)?;
        assert_eq!(back, label);
        Ok(())
    }

    #[test]
    fn deserialize_rejects_malformed_string() {
        let err = serde_json::from_str::<SpecLabel>("\"Loom Harness\"").unwrap_err();
        assert!(err.to_string().contains("invalid spec label"), "{err}");
    }

    #[test]
    fn parse_accepts_canonical_shapes() -> Result<()> {
        for input in ["harness", "gate", "demo", "a1", "spec-2-final"] {
            let label: SpecLabel = input.parse()?;
            assert_eq!(label.as_str(), input);
        }
        Ok(())
    }

    #[test]
    fn parse_rejects_malformed_inputs() {
        let cases = [
            "",
            "-leading",
            "trailing-",
            "double--dash",
            "UPPER",
            "with space",
            "with.dot",
            "with_underscore",
            "with/slash",
        ];
        for input in cases {
            let err = input.parse::<SpecLabel>().expect_err(input);
            assert_eq!(err, ParseSpecLabelError(input.to_owned()));
        }
    }
}

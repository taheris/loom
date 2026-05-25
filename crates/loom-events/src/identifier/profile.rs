use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ProfileName(String);

impl ProfileName {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProfileName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ProfileName {
    type Err = ParseProfileNameError;

    /// Parse a profile name: lowercase ASCII letters and digits,
    /// optionally separated by single `-`, non-empty, no leading or
    /// trailing `-`. Matches the directory names under `profiles/`
    /// (`rust`, `python`, `base`, etc.).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(ParseProfileNameError(s.to_owned()));
        }
        let bytes = s.as_bytes();
        if bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
            return Err(ParseProfileNameError(s.to_owned()));
        }
        let mut prev_dash = false;
        for &b in bytes {
            let ok = b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-';
            if !ok {
                return Err(ParseProfileNameError(s.to_owned()));
            }
            if b == b'-' && prev_dash {
                return Err(ParseProfileNameError(s.to_owned()));
            }
            prev_dash = b == b'-';
        }
        Ok(Self(s.to_owned()))
    }
}

impl<'de> Deserialize<'de> for ProfileName {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid profile name `{0}`: expected lowercase ASCII kebab-case")]
pub struct ParseProfileNameError(pub String);

#[cfg(test)]
mod tests {
    use super::{ParseProfileNameError, ProfileName};
    use anyhow::Result;

    #[test]
    fn display_round_trips_with_as_str() {
        let p = ProfileName::new("rust");
        assert_eq!(p.as_str(), "rust");
        assert_eq!(p.to_string(), "rust");
    }

    #[test]
    fn serde_round_trips_as_plain_string() -> Result<()> {
        let p = ProfileName::new("python");
        let json = serde_json::to_string(&p)?;
        assert_eq!(json, "\"python\"");
        let back: ProfileName = serde_json::from_str(&json)?;
        assert_eq!(back, p);
        Ok(())
    }

    #[test]
    fn deserialize_rejects_malformed_string() {
        let err = serde_json::from_str::<ProfileName>("\"Rust Profile\"").unwrap_err();
        assert!(err.to_string().contains("invalid profile name"), "{err}");
    }

    #[test]
    fn parse_accepts_canonical_shapes() -> Result<()> {
        for input in ["rust", "python", "base", "wrapix-mcp", "p1"] {
            let p: ProfileName = input.parse()?;
            assert_eq!(p.as_str(), input);
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
            "Rust",
            "with space",
            "with.dot",
            "with_underscore",
        ];
        for input in cases {
            let err = input.parse::<ProfileName>().expect_err(input);
            assert_eq!(err, ParseProfileNameError(input.to_owned()));
        }
    }
}

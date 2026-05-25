use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct BeadId(String);

impl BeadId {
    /// Parse a bead id from a raw string. Validates the canonical
    /// `<prefix>-<base32>(.<digits>)?` shape so that subprocess output
    /// drift (banners, warnings, empty lines) and external input (JSON
    /// from `bd`) is caught at the boundary rather than producing a
    /// malformed [`BeadId`] downstream.
    ///
    /// `<prefix>` is one or more lowercase ASCII letters, `<base32>` is
    /// one or more lowercase ASCII alphanumerics, and the optional
    /// `.<digits>` suffix scopes a sub-issue under a molecule.
    pub fn new(s: &str) -> Result<Self, ParseBeadIdError> {
        let (prefix, rest) = s
            .split_once('-')
            .ok_or_else(|| ParseBeadIdError(s.to_owned()))?;
        if prefix.is_empty() || !prefix.bytes().all(|b| b.is_ascii_lowercase()) {
            return Err(ParseBeadIdError(s.to_owned()));
        }
        let (body, suffix) = match rest.split_once('.') {
            Some((body, suffix)) => (body, Some(suffix)),
            None => (rest, None),
        };
        if body.is_empty()
            || !body
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        {
            return Err(ParseBeadIdError(s.to_owned()));
        }
        if let Some(suffix) = suffix
            && (suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(ParseBeadIdError(s.to_owned()));
        }
        Ok(Self(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BeadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for BeadId {
    type Err = ParseBeadIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl<'de> Deserialize<'de> for BeadId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        BeadId::new(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid bead id `{0}`: expected `<prefix>-<base32>(.<digits>)?`")]
pub struct ParseBeadIdError(pub String);

#[cfg(test)]
mod tests {
    use super::{BeadId, ParseBeadIdError};
    use anyhow::Result;

    #[test]
    fn display_round_trips_with_as_str() -> Result<()> {
        let id = BeadId::new("wx-3hhwq.2")?;
        assert_eq!(id.as_str(), "wx-3hhwq.2");
        assert_eq!(id.to_string(), "wx-3hhwq.2");
        Ok(())
    }

    #[test]
    fn serde_round_trips_as_plain_string() -> Result<()> {
        let id = BeadId::new("wx-abc123")?;
        let json = serde_json::to_string(&id)?;
        assert_eq!(json, "\"wx-abc123\"");
        let back: BeadId = serde_json::from_str(&json)?;
        assert_eq!(back, id);
        Ok(())
    }

    #[test]
    fn deserialize_rejects_malformed_string() {
        let err = serde_json::from_str::<BeadId>("\"not a bead\"").unwrap_err();
        assert!(err.to_string().contains("invalid bead id"), "{err}");
    }

    #[test]
    fn parse_accepts_canonical_shapes() -> Result<()> {
        for input in ["wx-abc123", "wx-3hhwq.2", "wx-3hhwq.20", "loom-a1b2c3"] {
            let id = BeadId::new(input)?;
            assert_eq!(id.as_str(), input);
        }
        Ok(())
    }

    #[test]
    fn parse_rejects_malformed_inputs() {
        let cases = [
            "",
            "wx",
            "wx-",
            "-abc",
            "wx-ABC",
            "wx-abc.",
            "wx-abc.x",
            "wx-abc.1.2",
            "WX-abc",
            "warning: foo\nwx-abc123",
            "wx-abc 123",
            "wx-abc-def",
        ];
        for input in cases {
            let err = BeadId::new(input).expect_err(input);
            assert_eq!(err, ParseBeadIdError(input.to_owned()));
        }
    }
}

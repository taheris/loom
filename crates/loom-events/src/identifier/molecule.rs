use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct MoleculeId(String);

impl MoleculeId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MoleculeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for MoleculeId {
    type Err = ParseMoleculeIdError;

    /// Parse a molecule id: `<prefix>-<base32>`, where `<prefix>` is one
    /// or more lowercase ASCII letters and `<base32>` is one or more
    /// lowercase ASCII alphanumerics. Same shape as a [`BeadId`] without
    /// the `.<digits>` sub-issue suffix, which mirrors how `bd` emits
    /// molecule ids (`wx-3hhwq`, `wx-mol42`).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (prefix, body) = s
            .split_once('-')
            .ok_or_else(|| ParseMoleculeIdError(s.to_owned()))?;
        if prefix.is_empty() || !prefix.bytes().all(|b| b.is_ascii_lowercase()) {
            return Err(ParseMoleculeIdError(s.to_owned()));
        }
        if body.is_empty()
            || !body
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        {
            return Err(ParseMoleculeIdError(s.to_owned()));
        }
        Ok(Self(s.to_owned()))
    }
}

impl<'de> Deserialize<'de> for MoleculeId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid molecule id `{0}`: expected `<prefix>-<base32>`")]
pub struct ParseMoleculeIdError(pub String);

#[cfg(test)]
mod tests {
    use super::{MoleculeId, ParseMoleculeIdError};
    use anyhow::Result;

    #[test]
    fn display_round_trips_with_as_str() {
        let id = MoleculeId::new("wx-3hhwq");
        assert_eq!(id.as_str(), "wx-3hhwq");
        assert_eq!(id.to_string(), "wx-3hhwq");
    }

    #[test]
    fn serde_round_trips_as_plain_string() -> Result<()> {
        let id = MoleculeId::new("wx-mol42");
        let json = serde_json::to_string(&id)?;
        assert_eq!(json, "\"wx-mol42\"");
        let back: MoleculeId = serde_json::from_str(&json)?;
        assert_eq!(back, id);
        Ok(())
    }

    #[test]
    fn deserialize_rejects_malformed_string() {
        let err = serde_json::from_str::<MoleculeId>("\"not a molecule\"").unwrap_err();
        assert!(err.to_string().contains("invalid molecule id"), "{err}");
    }

    #[test]
    fn parse_accepts_canonical_shapes() -> Result<()> {
        for input in ["wx-3hhwq", "wx-mol42", "loom-a1b2c3", "x-y"] {
            let id: MoleculeId = input.parse()?;
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
            "-mol",
            "WX-mol",
            "wx-MOL",
            "wx-3hhwq.1",
            "wx mol",
            "wx_mol",
        ];
        for input in cases {
            let err = input.parse::<MoleculeId>().expect_err(input);
            assert_eq!(err, ParseMoleculeIdError(input.to_owned()));
        }
    }
}

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ToolCallId(String);

impl ToolCallId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ToolCallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ToolCallId {
    type Err = ParseToolCallIdError;

    /// Parse a tool-call id: non-empty ASCII alphanumerics plus `_`
    /// and `-`. Matches Anthropic's `toolu_<base64ish>` convention while
    /// also accepting the short `t1` forms used in fixtures. Whitespace,
    /// dots, and other punctuation are rejected to catch garbled
    /// subprocess output at the boundary.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(ParseToolCallIdError(s.to_owned()));
        }
        for &b in s.as_bytes() {
            let ok = b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
            if !ok {
                return Err(ParseToolCallIdError(s.to_owned()));
            }
        }
        Ok(Self(s.to_owned()))
    }
}

impl<'de> Deserialize<'de> for ToolCallId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid tool call id `{0}`: expected ASCII alphanumerics with `_`/`-`")]
pub struct ParseToolCallIdError(pub String);

#[cfg(test)]
mod tests {
    use super::{ParseToolCallIdError, ToolCallId};
    use anyhow::Result;

    #[test]
    fn display_round_trips_with_as_str() {
        let id = ToolCallId::new("toolu_01");
        assert_eq!(id.as_str(), "toolu_01");
        assert_eq!(id.to_string(), "toolu_01");
    }

    #[test]
    fn serde_round_trips_as_plain_string() -> Result<()> {
        let id = ToolCallId::new("toolu_42");
        let json = serde_json::to_string(&id)?;
        assert_eq!(json, "\"toolu_42\"");
        let back: ToolCallId = serde_json::from_str(&json)?;
        assert_eq!(back, id);
        Ok(())
    }

    #[test]
    fn deserialize_rejects_malformed_string() {
        let err = serde_json::from_str::<ToolCallId>("\"tool call\"").unwrap_err();
        assert!(err.to_string().contains("invalid tool call id"), "{err}");
    }

    #[test]
    fn parse_accepts_canonical_shapes() -> Result<()> {
        for input in ["toolu_01", "toolu_42", "t1", "toolu_AbCd1234", "call-x"] {
            let id: ToolCallId = input.parse()?;
            assert_eq!(id.as_str(), input);
        }
        Ok(())
    }

    #[test]
    fn parse_rejects_malformed_inputs() {
        let cases = [
            "",
            "tool call",
            "tool\tcall",
            "tool.call",
            "tool/call",
            "tool\"call",
            "tool;call",
        ];
        for input in cases {
            let err = input.parse::<ToolCallId>().expect_err(input);
            assert_eq!(err, ParseToolCallIdError(input.to_owned()));
        }
    }
}

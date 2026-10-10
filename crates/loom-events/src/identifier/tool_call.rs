use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ToolCallId(String);

impl ToolCallId {
    /// Parses a canonical tool-call id.
    ///
    /// # Errors
    ///
    /// Returns [`ParseToolCallIdError`] when the input is malformed.
    pub fn new(s: impl AsRef<str>) -> Result<Self, ParseToolCallIdError> {
        s.as_ref().parse()
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

    /// Parse a non-empty ASCII alphanumeric id with `_`, `-`, and `|`,
    /// optionally followed by slash-separated numeric child ids.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let root = s.split_once('/').map_or(s, |(root, _)| root);
        if root.is_empty()
            || !root
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'|'))
            || s.split('/')
                .skip(1)
                .any(|child| child.is_empty() || !child.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(ParseToolCallIdError(s.to_owned()));
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

#[derive(Debug, displaydoc::Display, Error, PartialEq, Eq)]
/// invalid tool call id `{0}`: expected ASCII alphanumerics with `_`/`-`/`|` and optional `/number` child segments
pub struct ParseToolCallIdError(pub String);

#[cfg(test)]
mod tests {
    use super::{ParseToolCallIdError, ToolCallId};
    use anyhow::Result;

    #[test]
    fn display_round_trips_with_as_str() -> Result<()> {
        let id = ToolCallId::new("toolu_01")?;
        assert_eq!(id.as_str(), "toolu_01");
        assert_eq!(id.to_string(), "toolu_01");
        Ok(())
    }

    #[test]
    fn serde_round_trips_as_plain_string() -> Result<()> {
        let id = ToolCallId::new("toolu_42")?;
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
        for input in [
            "toolu_01",
            "toolu_42",
            "t1",
            "toolu_AbCd1234",
            "call-x",
            "call_es8xfFZpiG9eU4F6ONqu9AC5|fc_084fe1d174a690c2016a234c2e4d108191a893f4d1bf036187",
        ] {
            let id: ToolCallId = input.parse()?;
            assert_eq!(id.as_str(), input);
        }
        Ok(())
    }

    #[test]
    fn nested_tool_ids_round_trip_without_losing_numeric_segments() {
        for input in [
            "call_script/1",
            "call_script|fc_script/2",
            "call_script|fc_script/12/3",
        ] {
            let id = ToolCallId::new(input).expect("Pi nested tool id");
            assert_eq!(id.as_str(), input);
            let encoded = serde_json::to_string(&id).expect("serialize");
            let decoded: ToolCallId = serde_json::from_str(&encoded).expect("deserialize");
            assert_eq!(decoded, id);
        }
    }

    #[test]
    fn parse_rejects_malformed_inputs() {
        let cases = [
            "",
            "tool call",
            "tool\tcall",
            "tool.call",
            "tool/call",
            "/1",
            "tool/",
            "tool//1",
            "tool/1/",
            "tool/1/call",
            "tool/-1",
            "tool/1.5",
            "tool/１",
            "tool\"call",
            "tool;call",
        ];
        for input in cases {
            let err = ToolCallId::new(input).expect_err(input);
            assert_eq!(err, ParseToolCallIdError(input.to_owned()));
        }
    }
}

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SessionId {
    type Err = ParseSessionIdError;

    /// Parse a session id: non-empty ASCII alphanumerics plus `-` and
    /// `_`, no whitespace or control characters. Covers both Claude's
    /// hyphenated UUIDs and the short `sess-xyz` forms used in tests
    /// while rejecting empty / whitespace-only strings.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(ParseSessionIdError(s.to_owned()));
        }
        for &b in s.as_bytes() {
            let ok = b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
            if !ok {
                return Err(ParseSessionIdError(s.to_owned()));
            }
        }
        Ok(Self(s.to_owned()))
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid session id `{0}`: expected ASCII alphanumerics with `-`/`_`")]
pub struct ParseSessionIdError(pub String);

#[cfg(test)]
mod tests {
    use super::{ParseSessionIdError, SessionId};
    use anyhow::Result;

    #[test]
    fn display_round_trips_with_as_str() {
        let id = SessionId::new("session-abc");
        assert_eq!(id.as_str(), "session-abc");
        assert_eq!(id.to_string(), "session-abc");
    }

    #[test]
    fn serde_round_trips_as_plain_string() -> Result<()> {
        let id = SessionId::new("sess-xyz");
        let json = serde_json::to_string(&id)?;
        assert_eq!(json, "\"sess-xyz\"");
        let back: SessionId = serde_json::from_str(&json)?;
        assert_eq!(back, id);
        Ok(())
    }

    #[test]
    fn deserialize_rejects_malformed_string() {
        let err = serde_json::from_str::<SessionId>("\"sess xyz\"").unwrap_err();
        assert!(err.to_string().contains("invalid session id"), "{err}");
    }

    #[test]
    fn parse_accepts_canonical_shapes() -> Result<()> {
        for input in [
            "sess-xyz",
            "session-abc",
            "abc12345-1234-5678-9abc-def012345678",
            "claude_sess_42",
            "A1",
        ] {
            let id: SessionId = input.parse()?;
            assert_eq!(id.as_str(), input);
        }
        Ok(())
    }

    #[test]
    fn parse_rejects_malformed_inputs() {
        let cases = [
            "",
            "sess xyz",
            "sess\txyz",
            "sess.xyz",
            "sess/xyz",
            "sess\"xyz",
        ];
        for input in cases {
            let err = input.parse::<SessionId>().expect_err(input);
            assert_eq!(err, ParseSessionIdError(input.to_owned()));
        }
    }
}

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct RequestId(String);

impl RequestId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for RequestId {
    type Err = ParseRequestIdError;

    /// Parse a request id: non-empty ASCII alphanumerics plus `_`
    /// and `-`. Anthropic emits `req_<id>` for their API; the loom
    /// fixtures use the shorter `req-N` form. Whitespace and other
    /// punctuation are rejected to catch malformed external input.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(ParseRequestIdError(s.to_owned()));
        }
        for &b in s.as_bytes() {
            let ok = b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
            if !ok {
                return Err(ParseRequestIdError(s.to_owned()));
            }
        }
        Ok(Self(s.to_owned()))
    }
}

impl<'de> Deserialize<'de> for RequestId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid request id `{0}`: expected ASCII alphanumerics with `_`/`-`")]
pub struct ParseRequestIdError(pub String);

#[cfg(test)]
mod tests {
    use super::{ParseRequestIdError, RequestId};
    use anyhow::Result;

    #[test]
    fn display_round_trips_with_as_str() {
        let id = RequestId::new("req-1");
        assert_eq!(id.as_str(), "req-1");
        assert_eq!(id.to_string(), "req-1");
    }

    #[test]
    fn serde_round_trips_as_plain_string() -> Result<()> {
        let id = RequestId::new("req-42");
        let json = serde_json::to_string(&id)?;
        assert_eq!(json, "\"req-42\"");
        let back: RequestId = serde_json::from_str(&json)?;
        assert_eq!(back, id);
        Ok(())
    }

    #[test]
    fn deserialize_rejects_malformed_string() {
        let err = serde_json::from_str::<RequestId>("\"req 1\"").unwrap_err();
        assert!(err.to_string().contains("invalid request id"), "{err}");
    }

    #[test]
    fn parse_accepts_canonical_shapes() -> Result<()> {
        for input in ["req-1", "req-42", "req_01HG", "request-abc-1", "X1"] {
            let id: RequestId = input.parse()?;
            assert_eq!(id.as_str(), input);
        }
        Ok(())
    }

    #[test]
    fn parse_rejects_malformed_inputs() {
        let cases = ["", "req 1", "req\t1", "req.1", "req/1", "req\"1", "req;1"];
        for input in cases {
            let err = input.parse::<RequestId>().expect_err(input);
            assert_eq!(err, ParseRequestIdError(input.to_owned()));
        }
    }
}

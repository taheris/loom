//! Git object identifier newtype.
//!
//! `GitOid` parses a SHA hash at construction so downstream code cannot
//! hold a malformed OID. Per RS-7 (newtypes for identifiers), git OIDs
//! enter the type system through [`GitOid::new`] (or `parse()`) — there
//! is no `From<String>` / `Into` bypass.

use std::fmt;
use std::str::FromStr;

use displaydoc::Display;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

/// Git object identifier — a lowercase hex SHA hash.
///
/// Accepts SHA-1 (40 hex chars) and SHA-256 (64 hex chars); rejects
/// mixed case, non-hex bytes, and any other length.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct GitOid(String);

impl GitOid {
    /// Parse a git OID from a raw string. Validates the canonical
    /// lowercase-hex shape (40 or 64 characters).
    pub fn new(s: &str) -> Result<Self, ParseGitOidError> {
        let len = s.len();
        if !(len == 40 || len == 64) {
            return Err(ParseGitOidError(s.to_owned()));
        }
        if !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ParseGitOidError(s.to_owned()));
        }
        Ok(Self(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GitOid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for GitOid {
    type Err = ParseGitOidError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl<'de> Deserialize<'de> for GitOid {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        GitOid::new(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Display, Error, PartialEq, Eq)]
/// invalid git OID `{0}`: expected 40 or 64 lowercase hex characters
pub struct ParseGitOidError(pub String);

#[cfg(test)]
mod tests {
    use super::{GitOid, ParseGitOidError};

    #[test]
    fn parse_accepts_sha1_and_sha256_lowercase_hex() {
        let sha1 = "0000000000000000000000000000000000000000";
        let sha256 = "0".repeat(64);
        let mixed_sha1 = "deadbeefcafe1234567890abcdef0123456789ab";
        assert_eq!(GitOid::new(sha1).unwrap().as_str(), sha1);
        assert_eq!(GitOid::new(&sha256).unwrap().as_str(), sha256);
        assert_eq!(GitOid::new(mixed_sha1).unwrap().as_str(), mixed_sha1);
    }

    #[test]
    fn parse_rejects_wrong_length() {
        for input in [
            "",
            "abc",
            &"a".repeat(39),
            &"a".repeat(41),
            &"a".repeat(63),
            &"a".repeat(65),
        ] {
            let err = GitOid::new(input).expect_err(input);
            assert_eq!(err, ParseGitOidError(input.to_owned()));
        }
    }

    #[test]
    fn parse_rejects_uppercase_and_non_hex() {
        let upper = "DEADBEEFCAFE1234567890ABCDEF0123456789AB";
        let non_hex = "g".repeat(40);
        let punct = "deadbeef cafe1234567890abcdef0123456789a";
        assert!(GitOid::new(upper).is_err());
        assert!(GitOid::new(&non_hex).is_err());
        assert!(GitOid::new(punct).is_err());
    }

    #[test]
    fn display_round_trips_with_as_str() {
        let sha1 = "0123456789abcdef0123456789abcdef01234567";
        let oid = GitOid::new(sha1).unwrap();
        assert_eq!(oid.to_string(), sha1);
        assert_eq!(oid.as_str(), sha1);
    }

    #[test]
    fn serde_round_trips_as_plain_string() {
        let sha1 = "0123456789abcdef0123456789abcdef01234567";
        let oid = GitOid::new(sha1).unwrap();
        let json = serde_json::to_string(&oid).unwrap();
        assert_eq!(json, format!("\"{sha1}\""));
        let back: GitOid = serde_json::from_str(&json).unwrap();
        assert_eq!(back, oid);
    }

    #[test]
    fn deserialize_rejects_malformed_string() {
        let err = serde_json::from_str::<GitOid>("\"not a git oid\"").unwrap_err();
        assert!(err.to_string().contains("invalid git OID"), "{err}");
    }
}

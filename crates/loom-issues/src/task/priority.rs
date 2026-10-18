//! Beads priorities are bounded domain numbers, not arbitrary bytes.
use displaydoc::Display;
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use thiserror::Error;

/// A Beads priority in `0..=4`; all public construction checks that range.
/// The wire projection's default remains P0; configuration explicitly defaults to P2.
///
/// ```compile_fail
/// use loom_driver::bd::Priority;
/// let invalid = Priority(9);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Priority(u8);

impl Priority {
    pub const P0: Self = Self(0);
    pub const P1: Self = Self(1);
    pub const P2: Self = Self(2);
    pub const P3: Self = Self(3);
    pub const P4: Self = Self(4);
    /// Construct a priority in the inclusive range 0..=4.
    /// # Errors
    /// Rejects values outside the Beads priority range.
    pub const fn new(value: u8) -> Result<Self, ParsePriorityError> {
        if value <= 4 {
            Ok(Self(value))
        } else {
            Err(ParsePriorityError)
        }
    }
    pub const fn get(self) -> u8 {
        self.0
    }
}
impl TryFrom<u8> for Priority {
    type Error = ParsePriorityError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<Priority> for u8 {
    fn from(value: Priority) -> Self {
        value.get()
    }
}
impl FromStr for Priority {
    type Err = ParsePriorityError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value.parse().map_err(|_| ParsePriorityError)?)
    }
}
impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
#[derive(Debug, Display, Error, PartialEq, Eq)]
/// invalid priority: expected an integer in 0..=4
pub struct ParsePriorityError;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn priority_rejects_out_of_range_and_roundtrips_valid_values() {
        for value in 0..=u8::MAX {
            let parsed = serde_json::from_str::<Priority>(&value.to_string());
            assert_eq!(parsed.is_ok(), value <= 4);
            assert_eq!(Priority::new(value).is_ok(), value <= 4);
            if let Ok(priority) = parsed {
                assert_eq!(priority.get(), value);
                assert_eq!(serde_json::to_string(&priority).unwrap(), value.to_string());
            }
        }
        assert!(serde_json::from_str::<Priority>("-1").is_err());
        assert!(serde_json::from_str::<Priority>("256").is_err());
        assert!("5".parse::<Priority>().is_err());
    }
}

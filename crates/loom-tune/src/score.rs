use displaydoc::Display;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

/// Normalized checker score in the closed interval `[0.0, 1.0]`.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Score(f64);

impl Score {
    pub fn new(value: f64) -> Result<Self, ScoreError> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ScoreError::OutOfRange { value })
        }
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for Score {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = f64::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Score construction failures.
#[derive(Debug, Clone, PartialEq, Display, Error)]
pub enum ScoreError {
    /// score `{value}` is outside the inclusive 0.0..=1.0 range
    OutOfRange { value: f64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_accepts_inclusive_range() {
        assert_eq!(Score::new(0.0).expect("lower bound").get(), 0.0);
        assert_eq!(Score::new(1.0).expect("upper bound").get(), 1.0);
    }

    #[test]
    fn score_rejects_nan_and_out_of_range() {
        for input in [f64::NAN, -0.1, 1.1] {
            assert!(Score::new(input).is_err(), "{input}");
        }
    }
}

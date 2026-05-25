use std::num::NonZeroU32;
use std::str::FromStr;

use displaydoc::Display;
use thiserror::Error;

/// Parsed `--parallel N` (alias `-p N`) flag value. Always at least 1.
///
/// `Parallelism::ONE` is the default — sequential mode, no worktree, work
/// happens on the driver branch (preserves the previous `loom run` shape
/// from `wx-3hhwq.15`). `--parallel N` for `N > 1` always uses worktrees,
/// even for a single ready bead in that batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parallelism(NonZeroU32);

impl Parallelism {
    /// Sequential default.
    pub const ONE: Self = Self(NonZeroU32::MIN);

    pub fn get(self) -> u32 {
        self.0.get()
    }

    pub fn is_one(self) -> bool {
        self.0.get() == 1
    }
}

impl Default for Parallelism {
    fn default() -> Self {
        Self::ONE
    }
}

#[derive(Debug, Display, Error, PartialEq, Eq)]
pub enum ParallelismError {
    /// --parallel must be a positive integer (got: {input})
    NotPositiveInteger { input: String },
}

impl FromStr for Parallelism {
    type Err = ParallelismError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        let n = trimmed
            .parse::<u32>()
            .ok()
            .and_then(NonZeroU32::new)
            .ok_or_else(|| ParallelismError::NotPositiveInteger {
                input: s.to_owned(),
            })?;
        Ok(Self(n))
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests use panicking helpers"
)]
mod tests {
    use super::*;

    #[test]
    fn default_is_one() {
        assert_eq!(Parallelism::default(), Parallelism::ONE);
        assert_eq!(Parallelism::ONE.get(), 1);
        assert!(Parallelism::ONE.is_one());
    }

    #[test]
    fn parse_accepts_positive_integers() {
        let cases = [("1", 1), ("2", 2), ("4", 4), ("16", 16)];
        for (input, expected) in cases {
            let p: Parallelism = input.parse().unwrap_or_else(|e| {
                panic!("expected `{input}` to parse, got error {e:?}");
            });
            assert_eq!(p.get(), expected);
        }
    }

    #[test]
    fn parse_rejects_zero_and_negatives_and_non_integers() {
        for input in [
            "0",
            "-1",
            "-7",
            "abc",
            "1.5",
            "",
            "  ",
            "0x10",
            "9999999999999999999",
        ] {
            let err = Parallelism::from_str(input).err().unwrap_or_else(|| {
                panic!("expected `{input}` to be rejected");
            });
            match err {
                ParallelismError::NotPositiveInteger { input: bad } => {
                    assert_eq!(bad, input);
                }
            }
        }
    }

    #[test]
    fn is_one_false_for_n_greater_than_one() {
        let p: Parallelism = "5".parse().unwrap();
        assert!(!p.is_one());
        assert_eq!(p.get(), 5);
    }
}

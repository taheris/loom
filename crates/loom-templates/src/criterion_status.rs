//! Typed `criterion_status` decomposition-evidence surface.
//!
//! `CriterionStatus` is the per-criterion record that gives `todo_*`
//! decomposition agents evidence of which Success-Criteria bullets already
//! have current verifier evidence before they fan out beads. The driver joins
//! freshly parsed criteria against `.loom/cache.db`; cache misses are explicit
//! [`EvidenceState::Missing`] values.

use std::fmt;
use std::str::FromStr;

use displaydoc::Display;
use loom_events::identifier::SpecLabel;
use loom_protocol::todo::GitSha;
use thiserror::Error;

/// Per-criterion evidence record threaded into todo contexts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CriterionStatus {
    pub spec_label: SpecLabel,
    pub criterion_id: CriterionId,
    pub criterion_text: String,
    pub annotation: CriterionAnnotation,
    pub evidence: EvidenceState,
}

/// Stable identifier for a success criterion within one spec.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CriterionId(String);

impl CriterionId {
    pub fn new(value: impl Into<String>) -> Result<Self, ParseCriterionIdError> {
        let value = value.into();
        if !is_criterion_id(&value) {
            return Err(ParseCriterionIdError { value });
        }
        Ok(Self(value))
    }

    pub fn for_spec_text(spec_label: &SpecLabel, criterion_text: &str) -> Self {
        let canonical = format!(
            "{}\0{}",
            spec_label.as_str(),
            normalize_criterion_whitespace(criterion_text),
        );
        let digest = blake3::hash(canonical.as_bytes()).to_hex().to_string();
        Self(format!("criterion-{}", &digest[..16]))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for CriterionId {
    type Err = ParseCriterionIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl fmt::Display for CriterionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// invalid criterion id `{value}`: expected `criterion-` followed by 16 lowercase hex characters
#[derive(Debug, Clone, PartialEq, Eq, Display, Error)]
pub struct ParseCriterionIdError {
    pub value: String,
}

fn is_criterion_id(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("criterion-") else {
        return false;
    };
    hex.len() == 16
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn normalize_criterion_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parsed verifier annotation attached to a criterion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CriterionAnnotation {
    pub tier: AnnotationTier,
    pub target: AnnotationTarget,
    pub pending: bool,
}

impl fmt::Display for CriterionAnnotation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pending = if self.pending { "?" } else { "" };
        write!(f, "[{}{pending}]({})", self.tier.as_str(), self.target)
    }
}

/// Annotation tier closed set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationTier {
    Check,
    Test,
    System,
    Judge,
}

impl AnnotationTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Test => "test",
            Self::System => "system",
            Self::Judge => "judge",
        }
    }
}

/// Opaque annotation target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotationTarget(String);

impl AnnotationTarget {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AnnotationTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Evidence state for a parsed criterion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceState {
    Current {
        result: CriterionResult,
        last_timestamp_ms: i64,
        last_commit: GitSha,
        commits_since: u32,
    },
    Missing,
    StaleAnnotation {
        cached_annotation: CriterionAnnotation,
        last_timestamp_ms: i64,
        last_commit: GitSha,
        commits_since: u32,
    },
}

impl EvidenceState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Current { .. } => "Current",
            Self::Missing => "Missing",
            Self::StaleAnnotation { .. } => "StaleAnnotation",
        }
    }

    pub fn result_label(&self) -> &'static str {
        match self {
            Self::Current { result, .. } => result.as_str(),
            Self::Missing => "—",
            Self::StaleAnnotation { .. } => "—",
        }
    }

    pub fn last_timestamp_label(&self) -> String {
        match self {
            Self::Current {
                last_timestamp_ms, ..
            }
            | Self::StaleAnnotation {
                last_timestamp_ms, ..
            } => last_timestamp_ms.to_string(),
            Self::Missing => "—".to_string(),
        }
    }

    pub fn last_commit_label(&self) -> String {
        match self {
            Self::Current { last_commit, .. } | Self::StaleAnnotation { last_commit, .. } => {
                format!("`{last_commit}`")
            }
            Self::Missing => "—".to_string(),
        }
    }

    pub fn commits_since_label(&self) -> String {
        match self {
            Self::Current { commits_since, .. } | Self::StaleAnnotation { commits_since, .. } => {
                commits_since.to_string()
            }
            Self::Missing => "—".to_string(),
        }
    }

    pub fn cached_annotation_label(&self) -> String {
        match self {
            Self::StaleAnnotation {
                cached_annotation, ..
            } => cached_annotation.to_string(),
            Self::Current { .. } | Self::Missing => "—".to_string(),
        }
    }
}

/// Verdict variant for current criterion evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CriterionResult {
    Pass,
    Fail,
    Skipped,
}

impl CriterionResult {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pass => "Pass",
            Self::Fail => "Fail",
            Self::Skipped => "Skipped",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CriterionId, ParseCriterionIdError};
    use loom_events::identifier::SpecLabel;

    #[test]
    fn criterion_id_new_accepts_generated_shape() {
        let id = CriterionId::new("criterion-0123456789abcdef").expect("valid criterion id");
        assert_eq!(id.as_str(), "criterion-0123456789abcdef");
    }

    #[test]
    fn criterion_id_new_rejects_malformed_input() {
        for value in [
            "",
            "criterion-status-surface",
            "criterion-0123456789abcde",
            "criterion-0123456789abcdef0",
            "criterion-0123456789abcdeg",
            "CRITERION-0123456789abcdef",
            "with space",
        ] {
            let err = CriterionId::new(value).expect_err("malformed criterion id");
            assert_eq!(
                err,
                ParseCriterionIdError {
                    value: value.to_owned()
                },
            );
        }
    }

    #[test]
    fn criterion_id_for_spec_text_normalizes_whitespace() {
        let label = SpecLabel::new("templates");
        let a = CriterionId::for_spec_text(&label, "A criterion");
        let b = CriterionId::for_spec_text(&label, "A   criterion");
        assert_eq!(a, b);
        assert!(a.as_str().starts_with("criterion-"));
    }
}

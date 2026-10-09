//! Typed `criterion_status` decomposition-evidence surface.
//!
//! `CriterionStatus` is the per-criterion record that gives `todo_*`
//! decomposition agents evidence of which Success-Criteria bullets already
//! have current verifier evidence before they fan out beads. The driver joins
//! freshly parsed criteria against `.loom/cache.db`; cache misses are explicit
//! [`EvidenceState::Missing`] values.

use std::fmt;

use loom_events::identifier::SpecLabel;
pub use loom_protocol::criterion::{
    AnnotationTarget, AnnotationTier, CriterionAnnotation, CriterionId, ParseCriterionIdError,
};
use loom_protocol::todo::GitSha;

/// Per-criterion evidence record threaded into todo contexts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CriterionStatus {
    pub spec_label: SpecLabel,
    pub criterion_id: CriterionId,
    pub criterion_text: String,
    pub annotation: CriterionAnnotation,
    pub evidence: EvidenceState,
}

impl fmt::Display for CriterionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "**{} / {}** · {} · annotation `{}` · evidence `{}` · result {} · last commit {} · commits since {} · last timestamp {} · cached annotation `{}`",
            self.spec_label,
            self.criterion_id,
            self.criterion_text,
            self.annotation,
            self.evidence.as_str(),
            self.evidence.result_label(),
            self.evidence.last_commit_label(),
            self.evidence.commits_since_label(),
            self.evidence.last_timestamp_label(),
            self.evidence.cached_annotation_label()
        )
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
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Current { .. } => "Current",
            Self::Missing => "Missing",
            Self::StaleAnnotation { .. } => "StaleAnnotation",
        }
    }

    pub const fn result_label(&self) -> &'static str {
        match self {
            Self::Current { result, .. } => result.as_str(),
            Self::Missing | Self::StaleAnnotation { .. } => "—",
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
    pub const fn as_str(&self) -> &'static str {
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
        let label = SpecLabel::new("templates").unwrap();
        let a = CriterionId::for_spec_text(&label, "A criterion");
        let b = CriterionId::for_spec_text(&label, "A   criterion");
        assert_eq!(a, b);
        assert!(a.as_str().starts_with("criterion-"));
    }
}

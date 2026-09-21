//! A suppression selects exactly one finding identity and explains why.
use displaydoc::Display;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuppressionSelector {
    Id(String),
    Hash(String),
}

/// A checked selector and reason, immutable outside this module.
///
/// ```compile_fail
/// use loom_driver::config::{SuppressionConfig, SuppressionSelector};
/// let invalid = SuppressionConfig {
///     selector: SuppressionSelector::Id(String::new()),
///     reason: String::new(),
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawSuppression")]
pub struct SuppressionConfig {
    selector: SuppressionSelector,
    reason: String,
}

impl SuppressionConfig {
    /// Construct an exclusive, nonempty suppression.
    ///
    /// # Errors
    /// Rejects blank selectors and reasons.
    pub fn new(selector: SuppressionSelector, reason: String) -> Result<Self, SuppressionError> {
        let (SuppressionSelector::Id(value) | SuppressionSelector::Hash(value)) = &selector;
        if value.trim().is_empty() {
            return Err(SuppressionError::EmptySelector);
        }
        if reason.trim().is_empty() {
            return Err(SuppressionError::EmptyReason);
        }
        Ok(Self { selector, reason })
    }
    pub const fn selector(&self) -> &SuppressionSelector {
        &self.selector
    }
    pub fn reason(&self) -> &str {
        &self.reason
    }
    pub fn matches(&self, id: &str, hash: &str) -> bool {
        match &self.selector {
            SuppressionSelector::Id(expected) => expected == id,
            SuppressionSelector::Hash(expected) => expected == hash,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSuppression {
    id: Option<String>,
    hash: Option<String>,
    reason: String,
}

impl TryFrom<RawSuppression> for SuppressionConfig {
    type Error = SuppressionError;
    fn try_from(raw: RawSuppression) -> Result<Self, Self::Error> {
        let selector = match (raw.id, raw.hash) {
            (Some(id), None) => SuppressionSelector::Id(id),
            (None, Some(hash)) => SuppressionSelector::Hash(hash),
            _ => return Err(SuppressionError::ExclusiveSelector),
        };
        Self::new(selector, raw.reason)
    }
}

#[derive(Debug, Display, Error)]
pub enum SuppressionError {
    /// exactly one of id or hash is required
    ExclusiveSelector,
    /// suppression identity must not be empty
    EmptySelector,
    /// suppression reason is required
    EmptyReason,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn direct_serde_cannot_bypass_exclusive_nonempty_suppression() {
        for wire in [
            r#"{"reason":"explanation"}"#,
            r#"{"id":"x","hash":"y","reason":"explanation"}"#,
            r#"{"id":" ","reason":"explanation"}"#,
            r#"{"hash":"x","reason":" "}"#,
        ] {
            assert!(
                serde_json::from_str::<SuppressionConfig>(wire).is_err(),
                "{wire}"
            );
        }
        let parsed: SuppressionConfig =
            serde_json::from_str(r#"{"id":"v1:known","reason":"explanation"}"#).unwrap();
        assert_eq!(
            parsed.selector(),
            &SuppressionSelector::Id("v1:known".into())
        );
        assert_eq!(parsed.reason(), "explanation");
    }
}

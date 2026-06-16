use serde::{Deserialize, Serialize};

use crate::identifier::{ProfileName, SpecLabel};

/// One bead label. Wraps the raw `bd` string so the prefix families
/// (`spec:<X>`, `profile:<X>`, `loom:<X>`) parse exactly once at
/// deserialization time and call sites read through typed accessors instead
/// of re-doing `strip_prefix` walks.
///
/// Serde is `transparent` so `bd`'s `--json` output (a JSON string) round-trips
/// unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Label(String);

const SPEC_PREFIX: &str = "spec:";
const PROFILE_PREFIX: &str = "profile:";
const ACTIVE: &str = "loom:active";
const BLOCKED: &str = "loom:blocked";
const CLARIFY: &str = "loom:clarify";
const SPEC: &str = "loom:spec";
const TODO: &str = "loom:todo";

impl Label {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `Some(SpecLabel)` when the label is `spec:<X>`.
    pub fn spec_label(&self) -> Option<SpecLabel> {
        self.0.strip_prefix(SPEC_PREFIX).map(SpecLabel::new)
    }

    /// `Some(ProfileName)` when the label is `profile:<X>`.
    pub fn profile_name(&self) -> Option<ProfileName> {
        self.0.strip_prefix(PROFILE_PREFIX).map(ProfileName::new)
    }

    /// `true` when the label is exactly `loom:active`.
    pub fn is_active(&self) -> bool {
        self.0 == ACTIVE
    }

    /// `true` when the label is exactly `loom:blocked`.
    pub fn is_blocked(&self) -> bool {
        self.0 == BLOCKED
    }

    /// `true` when the label is exactly `loom:clarify`.
    pub fn is_clarify(&self) -> bool {
        self.0 == CLARIFY
    }

    /// `true` when the label is exactly `loom:spec`.
    pub fn is_spec_epic(&self) -> bool {
        self.0 == SPEC
    }

    /// `true` when the label is exactly `loom:todo`.
    pub fn is_todo_stage(&self) -> bool {
        self.0 == TODO
    }
}

impl ::std::fmt::Display for Label {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    #[test]
    fn spec_label_strips_prefix() {
        let l = Label::new("spec:harness");
        assert_eq!(l.spec_label(), Some(SpecLabel::new("harness")));
        assert!(l.profile_name().is_none());
        assert!(!l.is_blocked());
        assert!(!l.is_clarify());
    }

    #[test]
    fn profile_name_strips_prefix() {
        let l = Label::new("profile:rust");
        assert_eq!(l.profile_name(), Some(ProfileName::new("rust")));
        assert!(l.spec_label().is_none());
    }

    #[test]
    fn loom_blocked_and_clarify_are_exact_match() {
        assert!(Label::new("loom:blocked").is_blocked());
        assert!(Label::new("loom:clarify").is_clarify());
        assert!(!Label::new("loom:blocked-cause").is_blocked());
        assert!(!Label::new("loom:clarify-soon").is_clarify());
        assert!(!Label::new("loom:blocked").is_clarify());
        assert!(!Label::new("loom:clarify").is_blocked());
    }

    #[test]
    fn unrecognised_label_yields_no_typed_value() {
        let l = Label::new("urgent");
        assert!(l.spec_label().is_none());
        assert!(l.profile_name().is_none());
        assert!(!l.is_blocked());
        assert!(!l.is_clarify());
        assert_eq!(l.as_str(), "urgent");
        assert_eq!(l.to_string(), "urgent");
    }

    #[test]
    fn empty_suffix_still_strips_prefix() {
        // `bd` does not emit `spec:` with an empty suffix, but the parser
        // mustn't reject it — the typed value carries the empty string.
        assert_eq!(Label::new("spec:").spec_label(), Some(SpecLabel::new("")));
    }

    #[test]
    fn serde_round_trips_as_plain_string() -> Result<()> {
        let l = Label::new("spec:harness");
        let json = serde_json::to_string(&l)?;
        assert_eq!(json, "\"spec:harness\"");
        let back: Label = serde_json::from_str(&json)?;
        assert_eq!(back, l);
        Ok(())
    }

    #[test]
    fn vec_of_labels_round_trips_through_serde() -> Result<()> {
        let v = vec![Label::new("profile:rust"), Label::new("spec:harness")];
        let json = serde_json::to_string(&v)?;
        assert_eq!(json, r#"["profile:rust","spec:harness"]"#);
        let back: Vec<Label> = serde_json::from_str(&json)?;
        assert_eq!(back, v);
        Ok(())
    }
}

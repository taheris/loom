use super::Label;
use crate::identifier::{ProfileName, SpecLabel};

/// Loom-owned projections of otherwise opaque tracker labels.
pub trait LabelExt {
    fn spec_label(&self) -> Option<SpecLabel>;
    fn profile_name(&self) -> Option<ProfileName>;
    fn is_active(&self) -> bool;
    fn is_blocked(&self) -> bool;
    fn is_clarify(&self) -> bool;
    fn is_deferred(&self) -> bool;
    fn is_infra(&self) -> bool;
    fn is_spec_epic(&self) -> bool;
    fn is_todo_stage(&self) -> bool;
}
fn projection<T: std::str::FromStr>(label: &str, prefix: &str) -> Option<T> {
    if let Ok(value) = label.strip_prefix(prefix)?.parse() {
        Some(value)
    } else {
        tracing::warn!(
            label,
            prefix,
            "Ignoring a malformed workflow label projection"
        );
        None
    }
}
impl LabelExt for Label {
    fn spec_label(&self) -> Option<SpecLabel> {
        projection(self.as_str(), "spec:")
    }
    fn profile_name(&self) -> Option<ProfileName> {
        projection(self.as_str(), "profile:")
    }
    fn is_active(&self) -> bool {
        self.as_str() == "loom:active"
    }
    fn is_blocked(&self) -> bool {
        self.as_str() == "loom:blocked"
    }
    fn is_clarify(&self) -> bool {
        self.as_str() == "loom:clarify"
    }
    fn is_deferred(&self) -> bool {
        self.as_str() == "loom:deferred"
    }
    fn is_infra(&self) -> bool {
        self.as_str() == "loom:infra"
    }
    fn is_spec_epic(&self) -> bool {
        self.as_str() == "loom:spec"
    }
    fn is_todo_stage(&self) -> bool {
        self.as_str() == "loom:todo"
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projections_do_not_adopt_malformed_or_similar_labels() {
        assert_eq!(
            Label::new("spec:harness").unwrap().spec_label(),
            Some("harness".parse().unwrap())
        );
        assert!(Label::new("spec:Bad_Label").unwrap().spec_label().is_none());
        assert!(Label::new("loom:blocked").unwrap().is_blocked());
        assert!(!Label::new("loom:blocked-cause").unwrap().is_blocked());
    }
}

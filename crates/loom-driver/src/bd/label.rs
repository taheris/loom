use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::identifier::{ProfileName, SpecLabel};

const SPEC_PREFIX: &str = "spec:";
const PROFILE_PREFIX: &str = "profile:";
const ACTIVE: &str = "loom:active";
const BLOCKED: &str = "loom:blocked";
const CLARIFY: &str = "loom:clarify";
const DEFERRED: &str = "loom:deferred";
const INFRA: &str = "loom:infra";
const SPEC: &str = "loom:spec";
const TODO: &str = "loom:todo";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Label {
    raw: String,
    kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Kind {
    Spec(SpecLabel),
    Profile(ProfileName),
    Other,
}

impl Label {
    ///
    /// # Errors
    ///
    /// Returns an error when the `bd` command fails or its response cannot be decoded.
    pub fn new(s: impl AsRef<str>) -> Result<Self, ParseLabelError> {
        let raw = s.as_ref();
        let kind = if let Some(suffix) = raw.strip_prefix(SPEC_PREFIX) {
            Kind::Spec(
                suffix
                    .parse()
                    .map_err(|_| ParseLabelError(raw.to_string()))?,
            )
        } else if let Some(suffix) = raw.strip_prefix(PROFILE_PREFIX) {
            Kind::Profile(
                suffix
                    .parse()
                    .map_err(|_| ParseLabelError(raw.to_string()))?,
            )
        } else {
            Kind::Other
        };
        Ok(Self {
            raw: raw.to_string(),
            kind,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub fn spec_label(&self) -> Option<SpecLabel> {
        match &self.kind {
            Kind::Spec(label) => Some(label.clone()),
            Kind::Profile(_) | Kind::Other => None,
        }
    }

    pub fn profile_name(&self) -> Option<ProfileName> {
        match &self.kind {
            Kind::Profile(profile) => Some(profile.clone()),
            Kind::Spec(_) | Kind::Other => None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.raw == ACTIVE
    }

    pub fn is_blocked(&self) -> bool {
        self.raw == BLOCKED
    }

    pub fn is_clarify(&self) -> bool {
        self.raw == CLARIFY
    }

    pub fn is_deferred(&self) -> bool {
        self.raw == DEFERRED
    }

    pub fn is_infra(&self) -> bool {
        self.raw == INFRA
    }

    pub fn is_spec_epic(&self) -> bool {
        self.raw == SPEC
    }

    pub fn is_todo_stage(&self) -> bool {
        self.raw == TODO
    }
}

impl std::fmt::Display for Label {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.raw)
    }
}

impl Serialize for Label {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.raw)
    }
}

impl<'de> Deserialize<'de> for Label {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, displaydoc::Display, thiserror::Error, PartialEq, Eq)]
/// invalid typed bead label `{0}`
pub struct ParseLabelError(pub String);

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::*;

    #[test]
    fn typed_prefixes_parse_once_at_construction() -> Result<()> {
        let spec = Label::new("spec:harness")?;
        assert_eq!(spec.spec_label(), Some("harness".parse()?));
        assert!(spec.profile_name().is_none());

        let profile = Label::new("profile:rust")?;
        assert_eq!(profile.profile_name(), Some("rust".parse()?));
        assert!(profile.spec_label().is_none());
        Ok(())
    }

    #[test]
    fn loom_resolution_labels_are_exact_match() -> Result<()> {
        assert!(Label::new("loom:blocked")?.is_blocked());
        assert!(Label::new("loom:clarify")?.is_clarify());
        assert!(Label::new("loom:deferred")?.is_deferred());
        assert!(Label::new("loom:infra")?.is_infra());
        assert!(!Label::new("loom:blocked-cause")?.is_blocked());
        assert!(!Label::new("loom:clarify-soon")?.is_clarify());
        assert!(!Label::new("loom:deferred-work")?.is_deferred());
        assert!(!Label::new("loom:infra-retry")?.is_infra());
        Ok(())
    }

    #[test]
    fn unrecognised_label_remains_an_open_set() -> Result<()> {
        let label = Label::new("urgent")?;
        assert!(label.spec_label().is_none());
        assert!(label.profile_name().is_none());
        assert_eq!(label.as_str(), "urgent");
        assert_eq!(label.to_string(), "urgent");
        Ok(())
    }

    #[test]
    fn invalid_typed_suffix_is_rejected() {
        assert!(Label::new("spec:").is_err());
        assert!(Label::new("spec:Bad_Label").is_err());
        assert!(Label::new("profile:Rust").is_err());
        assert!(serde_json::from_str::<Label>("\"spec:\"").is_err());
    }

    #[test]
    fn serde_round_trips_as_plain_string() -> Result<()> {
        let label = Label::new("spec:harness")?;
        let json = serde_json::to_string(&label)?;
        assert_eq!(json, "\"spec:harness\"");
        let back: Label = serde_json::from_str(&json)?;
        assert_eq!(back, label);
        Ok(())
    }

    #[test]
    fn vec_of_labels_round_trips_through_serde() -> Result<()> {
        let labels = vec![Label::new("profile:rust")?, Label::new("spec:harness")?];
        let json = serde_json::to_string(&labels)?;
        assert_eq!(json, r#"["profile:rust","spec:harness"]"#);
        let back: Vec<Label> = serde_json::from_str(&json)?;
        assert_eq!(back, labels);
        Ok(())
    }
}

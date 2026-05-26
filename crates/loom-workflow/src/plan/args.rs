use loom_driver::identifier::SpecLabel;

use super::error::PlanError;

/// Parsed `-n <label>` / `-u <label>` selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanMode {
    /// `loom plan -n <label>` — new spec interview.
    New(SpecLabel),
    /// `loom plan -u <label>` — update an existing spec.
    Update(SpecLabel),
}

impl PlanMode {
    pub fn label(&self) -> &SpecLabel {
        match self {
            PlanMode::New(l) | PlanMode::Update(l) => l,
        }
    }
}

/// Resolve clap's pair of `Option<String>` flags into a [`PlanMode`].
///
/// Exactly one of `new`/`update` must be set. Both unset and both set are
/// rejected with [`PlanError::ModeRequired`] / [`PlanError::ConflictingModes`].
pub fn parse_mode(new: Option<String>, update: Option<String>) -> Result<PlanMode, PlanError> {
    match (new, update) {
        (Some(_), Some(_)) => Err(PlanError::ConflictingModes),
        (Some(label), None) => Ok(PlanMode::New(SpecLabel::new(label))),
        (None, Some(label)) => Ok(PlanMode::Update(SpecLabel::new(label))),
        (None, None) => Err(PlanError::ModeRequired),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mode_accepts_new_only() {
        let mode = parse_mode(Some("harness".into()), None).expect("new label");
        assert_eq!(mode, PlanMode::New(SpecLabel::new("harness")));
        assert_eq!(mode.label().as_str(), "harness");
    }

    #[test]
    fn parse_mode_accepts_update_only() {
        let mode = parse_mode(None, Some("harness".into())).expect("update label");
        assert_eq!(mode, PlanMode::Update(SpecLabel::new("harness")));
    }

    #[test]
    fn parse_mode_rejects_both_flags() {
        let err = parse_mode(Some("a".into()), Some("b".into())).unwrap_err();
        assert!(matches!(err, PlanError::ConflictingModes));
    }

    #[test]
    fn parse_mode_rejects_no_flags() {
        let err = parse_mode(None, None).unwrap_err();
        assert!(matches!(err, PlanError::ModeRequired));
    }
}

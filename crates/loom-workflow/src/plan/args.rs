use loom_driver::identifier::SpecLabel;

use super::error::PlanError;

/// Parse optional positional plan anchors into typed spec labels.
pub fn parse_anchor_labels(labels: Vec<String>) -> Result<Vec<SpecLabel>, PlanError> {
    labels
        .into_iter()
        .map(|label| {
            label
                .parse::<SpecLabel>()
                .map_err(|_| PlanError::InvalidAnchorLabel { label })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_anchor_labels_accepts_empty_roster() {
        let labels = parse_anchor_labels(Vec::new()).expect("labels");
        assert!(labels.is_empty());
    }

    #[test]
    fn parse_anchor_labels_preserves_order_and_multiplicity() {
        let labels =
            parse_anchor_labels(vec!["harness".into(), "templates".into()]).expect("labels");
        assert_eq!(
            labels,
            vec![SpecLabel::new("harness"), SpecLabel::new("templates")]
        );
    }

    #[test]
    fn parse_anchor_labels_rejects_malformed_label() {
        let err = parse_anchor_labels(vec!["Bad_Label".into()]).unwrap_err();
        assert!(matches!(
            err,
            PlanError::InvalidAnchorLabel { label } if label == "Bad_Label"
        ));
    }
}

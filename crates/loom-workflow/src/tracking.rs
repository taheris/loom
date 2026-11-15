//! Host adapter composition and workflow-owned checked tracking tokens.

mod access;
pub use access::Adapter;
pub(crate) use access::configure_worker;

use loom_issues::{task::Label, tracking::Error};

pub(crate) fn label(value: &str) -> Result<Label, Error> {
    Label::new(value).map_err(|error| Error::Rejected {
        reason: error.to_string(),
    })
}

pub(crate) fn labels(values: impl IntoIterator<Item = String>) -> Result<Vec<Label>, Error> {
    values.into_iter().map(|value| label(&value)).collect()
}

pub(crate) fn infra_metadata(
    diagnostic: &crate::r#loop::InfraDiagnostic,
) -> std::collections::BTreeMap<String, serde_json::Value> {
    let mut metadata = std::collections::BTreeMap::from([
        ("loom.infra.cause".into(), diagnostic.cause.clone().into()),
        ("loom.infra.phase".into(), "loop".into()),
        (
            "loom.infra.class".into(),
            diagnostic.infra_class.clone().into(),
        ),
    ]);
    if let Some(seen) = diagnostic.first_event_seen {
        metadata.insert("loom.infra.first_event_seen".into(), seen.into());
    }
    if let Some(attempt) = diagnostic.attempt {
        metadata.insert("loom.infra.attempt".into(), attempt.into());
    }
    if let Some(attempts) = diagnostic.max_attempts {
        metadata.insert("loom.infra.max_attempts".into(), attempts.into());
    }
    metadata
}

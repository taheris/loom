//! Spec → molecule resolution via `bd find`.
//!
//! Under the at-most-one-open-epic-per-spec invariant, the spec's
//! "active" molecule is the open epic returned by
//! `bd find --type=epic --label=spec:<X> --status=open`. Zero results
//! means no molecule (callers either mint one or treat the spec as
//! pristine); more than one is a structural invariant violation that
//! refuses to proceed.

use displaydoc::Display;
use thiserror::Error;

use loom_driver::bd::{BdClient, BdError, CommandRunner, ListOpts};
use loom_driver::identifier::{MoleculeId, SpecLabel};

/// Failures from [`resolve_open_epic`].
#[derive(Debug, Display, Error)]
pub enum ResolveError {
    /// bd query failed: {0}
    Bd(#[from] BdError),
    /// multiple open epics found for spec `{label}`: {ids}; close all but one before re-running
    InvariantViolation { label: String, ids: String },
}

/// Resolve the spec's active molecule via `bd find --type=epic
/// --label=spec:<X> --status=open`. Returns the open epic's id, `None`
/// when no open epic exists, or [`ResolveError::InvariantViolation`]
/// when more than one open epic exists for the spec.
pub async fn resolve_open_epic<R: CommandRunner>(
    bd: &BdClient<R>,
    label: &SpecLabel,
) -> Result<Option<MoleculeId>, ResolveError> {
    let beads = bd
        .list(ListOpts {
            issue_type: Some("epic".to_string()),
            label: Some(format!("spec:{}", label.as_str())),
            status: Some("open".to_string()),
            ..Default::default()
        })
        .await?;
    match beads.len() {
        0 => Ok(None),
        1 => Ok(Some(MoleculeId::new(beads[0].id.as_str()))),
        _ => {
            let ids = beads
                .iter()
                .map(|b| b.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Err(ResolveError::InvariantViolation {
                label: label.to_string(),
                ids,
            })
        }
    }
}

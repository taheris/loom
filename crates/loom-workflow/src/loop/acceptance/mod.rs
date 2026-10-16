//! Resolve task-purpose candidates without interpreting labels as acceptance.

use std::path::Path;

use displaydoc::Display;
use loom_driver::bd::{BdClient, BdError, Bead, CommandRunner};
use loom_protocol::acceptance::{Candidates, METADATA_KEY};
use thiserror::Error;

use crate::mint::acceptance as remediation;

pub use loom_protocol::acceptance::dispatch::Acceptance as Resolved;

#[derive(Debug, Display, Error)]
pub enum Error {
    /// Beads failed while reading task acceptance
    Bd(#[from] BdError),
    /// task {0} lacks explicit acceptance; bind criteria or admit finding goals
    Missing(loom_driver::identifier::BeadId),
    /// {0} is not an implementation task; decisions and epics are not worker acceptance
    NotTask(loom_driver::identifier::BeadId),
    /// task acceptance metadata is malformed; explicitly repair its purpose and references
    Metadata(#[from] serde_json::Error),
    /// implementation acceptance snapshot is invalid
    Snapshot(#[from] loom_gate::acceptance::Error),
    /// ordinary task references no longer resolve; explicitly rebind them
    Criteria(#[from] loom_protocol::acceptance::ResolveError),
    /// finding-goal acceptance requires repair
    Remediation(#[from] remediation::Error),
}

/// Resolve explicit task acceptance, admitting typed follow-up proposals before resolution.
///
/// # Errors
/// Missing, malformed, unrelated or stale acceptance rejects dispatch without a purpose fallback.
pub async fn resolve<R: CommandRunner>(
    bd: &BdClient<R>,
    task: &Bead,
    workspace: &Path,
) -> Result<Resolved, Error> {
    if task.issue_type != loom_driver::bd::IssueType::Task {
        return Err(Error::NotTask(task.id.clone()));
    }
    let refreshed;
    let task = if task.metadata.contains_key(remediation::FOLLOW_UP_KEY) {
        remediation::admit_proposal(bd, task, workspace).await?;
        refreshed = bd.show(&task.id).await?;
        &refreshed
    } else {
        task
    };
    let value = task
        .metadata
        .get(METADATA_KEY)
        .ok_or_else(|| Error::Missing(task.id.clone()))?;
    match Candidates::from_metadata(value)? {
        Candidates::Implementation { criteria } => {
            let snapshot = loom_gate::acceptance::load(workspace)?;
            Ok(Resolved::Implementation(snapshot.resolve(&criteria)?))
        }
        Candidates::Remediation { goals } => Ok(Resolved::Remediation(
            remediation::resolve(bd, task, &goals, workspace).await?,
        )),
    }
}

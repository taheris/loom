use std::path::Path;

use rusqlite::params;
use tracing::debug;

use crate::identifier::SpecLabel;
use crate::spec::package;

use super::companions::parse_companions;
use super::db::{CacheDb, SpecEpicRow, WorkEpicRow, drop_and_recreate};
use super::error::CacheError;

/// A durable metadata carrier or an independent pending/active work epic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebuildEpic {
    Spec(SpecEpicRow),
    Work(WorkEpicRow),
}

/// Counts of rows written by [`CacheDb::rebuild`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RebuildReport {
    pub specs: usize,
    pub spec_epics: usize,
    pub work_epics: usize,
    pub companions: usize,
}

impl CacheDb {
    /// Recreate cache tables from complete spec owners and durable epic metadata.
    ///
    /// 1. `docs/README.md` — one `specs` row per indexed package, cross-checked
    ///    against the discovered inventory before replacing any cache rows.
    /// 2. `epics` — independent spec metadata and work-epic rows.
    /// 3. Each spec's `## Companions` section — one `companions` row per
    ///    listed path. Specs without the section contribute zero rows.
    ///
    /// Iteration counters reset to 0.
    ///
    /// # Errors
    ///
    /// Returns an error when database access, stored state, or state validation fails.
    pub fn rebuild(
        &self,
        workspace: &Path,
        epics: &[RebuildEpic],
    ) -> Result<RebuildReport, CacheError> {
        let has_index = workspace.join("docs/README.md").try_exists()?;
        let spec_rows = collect_spec_rows(workspace, has_index)?;

        let mut by_spec: std::collections::BTreeMap<&str, Vec<&str>> =
            std::collections::BTreeMap::new();
        for epic in epics {
            if let RebuildEpic::Spec(row) = epic {
                by_spec
                    .entry(row.spec_label.as_str())
                    .or_default()
                    .push(row.epic_id.as_str());
                if !spec_rows
                    .iter()
                    .any(|(label, _, _)| label == &row.spec_label)
                {
                    return Err(CacheError::SpecIndexMismatch {
                        detail: format!(
                            "spec epic `{}` refers to missing spec `{}`",
                            row.epic_id, row.spec_label
                        ),
                    });
                }
            }
        }
        if let Some((label, ids)) = by_spec.iter().find(|(_, ids)| ids.len() > 1) {
            return Err(CacheError::DuplicateSpecEpics {
                label: (*label).to_string(),
                ids: ids.join(", "),
            });
        }
        if has_index
            && let Some(label) = spec_rows
                .iter()
                .map(|(label, _, _)| label.as_str())
                .find(|label| !by_spec.contains_key(label))
        {
            return Err(CacheError::SpecIndexMismatch {
                detail: format!("indexed spec `{label}` has no loom:spec epic"),
            });
        }

        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            drop_and_recreate(&tx)?;
            let mut report = RebuildReport::default();

            for (label, spec_path, content) in &spec_rows {
                tx.execute(
                    "INSERT INTO specs(label, spec_path) VALUES (?1, ?2)",
                    params![label.as_str(), spec_path],
                )?;
                report.specs += 1;

                for path in parse_companions(content) {
                    tx.execute(
                        "INSERT OR IGNORE INTO companions(spec_label, companion_path)
                         VALUES (?1, ?2)",
                        params![label.as_str(), path],
                    )?;
                    report.companions += 1;
                }
            }

            for epic in epics {
                match epic {
                    RebuildEpic::Spec(row) => {
                        tx.execute(
                            "INSERT INTO spec_epics(spec_label, epic_id, todo_cursor) VALUES (?1, ?2, ?3)",
                            params![row.spec_label.as_str(), row.epic_id.as_str(), row.todo_cursor],
                        )?;
                        report.spec_epics += 1;
                    }
                    RebuildEpic::Work(row) => {
                        tx.execute(
                            "INSERT INTO work_epics(epic_id, base_commit, todo_fingerprint, is_active, iteration_count)
                             VALUES (?1, ?2, ?3, ?4, 0)",
                            params![row.epic_id.as_str(), row.base_commit, row.todo_fingerprint, row.is_active],
                        )?;
                        report.work_epics += 1;
                    }
                }
            }
            tx.commit()?;
            debug!(?report, "cache-db rebuild complete");
            Ok(report)
        })
    }
}

fn collect_spec_rows(
    workspace: &Path,
    has_index: bool,
) -> Result<Vec<(SpecLabel, String, String)>, CacheError> {
    if !has_index && !workspace.join("specs").try_exists()? {
        return Ok(Vec::new());
    }
    let packages = package::discover_workspace(workspace).map_err(|error| match error {
        package::Error::Index { detail } => CacheError::SpecIndexMismatch { detail },
        source => CacheError::Package(source),
    })?;
    packages
        .into_iter()
        .map(|package| {
            let path = package.contract().strip_prefix(workspace).map_err(|_| {
                CacheError::SpecIndexMismatch {
                    detail: format!(
                        "spec contract `{}` is outside the workspace",
                        package.contract().display()
                    ),
                }
            })?;
            Ok((
                package.label().clone(),
                path.to_string_lossy().into_owned(),
                std::fs::read_to_string(package.contract())?,
            ))
        })
        .collect()
}

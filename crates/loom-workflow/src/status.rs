//! `loom status` — read-only snapshot of cache health and work epics.

use displaydoc::Display;
use loom_driver::state::{CacheDb, CacheError, WorkEpicRow};
use thiserror::Error;

/// Snapshot returned by [`load`].
#[derive(Debug, Clone)]
pub struct StatusReport {
    pub active_work_epic: Option<WorkEpicRow>,
    pub pending_todo: Vec<WorkEpicRow>,
    pub integration_branch: String,
    pub cache_healthy: bool,
}

/// Failures raised by [`load`].
#[derive(Debug, Display, Error)]
pub enum StatusError {
    /// cache-db read failed
    State(#[from] CacheError),
}

/// Read cached work-epic mirrors from `db` for a non-load-bearing listing.
pub fn load(db: &CacheDb, integration_branch: String) -> Result<StatusReport, StatusError> {
    let work_epics = db.work_epics()?;
    let active_work_epic = work_epics.iter().find(|row| row.is_active).cloned();
    let pending_todo = work_epics
        .into_iter()
        .filter(|row| !row.is_active && row.todo_head.is_some())
        .collect();
    Ok(StatusReport {
        active_work_epic,
        pending_todo,
        integration_branch,
        cache_healthy: true,
    })
}

/// Render [`StatusReport`] as a multi-line, human-friendly string.
pub fn render(report: &StatusReport) -> String {
    let mut out = String::new();
    out.push_str(if report.cache_healthy {
        "cache: healthy\n"
    } else {
        "cache: unhealthy\n"
    });
    match &report.active_work_epic {
        Some(epic) => {
            out.push_str(&format!("active work epic: {}\n", epic.epic_id));
            out.push_str(&format!("active iteration: {}\n", epic.iteration_count));
        }
        None => {
            out.push_str("active work epic: <none>\n");
            out.push_str("active iteration: 0\n");
        }
    }
    if report.pending_todo.is_empty() {
        out.push_str("pending loom:todo: <none>\n");
    } else {
        for epic in &report.pending_todo {
            out.push_str(&format!(
                "pending loom:todo: {} head={} iteration={}\n",
                epic.epic_id,
                epic.todo_head.as_deref().unwrap_or("<unset>"),
                epic.iteration_count,
            ));
        }
    }
    out.push_str(&format!(
        "integration branch: {}\n",
        report.integration_branch,
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use loom_driver::identifier::MoleculeId;

    fn fresh_db(workspace: &std::path::Path) -> Result<CacheDb> {
        Ok(CacheDb::open(workspace.join(".loom/cache.db"))?)
    }

    #[test]
    fn empty_cache_reports_no_work_epics() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = fresh_db(dir.path())?;
        let report = load(&db, "main".to_string())?;
        assert!(report.active_work_epic.is_none());
        assert!(report.pending_todo.is_empty());
        let body = render(&report);
        assert!(body.contains("active work epic: <none>"), "body: {body}");
        assert!(body.contains("pending loom:todo: <none>"), "body: {body}");
        Ok(())
    }

    #[test]
    fn status_reports_active_work_epic_not_current_spec() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = fresh_db(dir.path())?;
        db.upsert_work_epic(&loom_driver::state::WorkEpicRow {
            epic_id: MoleculeId::new("lm-active"),
            todo_head: Some("abc".to_string()),
            todo_fingerprint: Some("fp".to_string()),
            is_active: true,
            iteration_count: 3,
        })?;
        db.upsert_work_epic(&loom_driver::state::WorkEpicRow {
            epic_id: MoleculeId::new("lm-todo"),
            todo_head: Some("def".to_string()),
            todo_fingerprint: Some("fp2".to_string()),
            is_active: false,
            iteration_count: 0,
        })?;

        let body = render(&load(&db, "main".to_string())?);
        assert!(body.contains("active work epic: lm-active"), "body: {body}");
        assert!(body.contains("active iteration: 3"), "body: {body}");
        assert!(body.contains("pending loom:todo: lm-todo"), "body: {body}");
        assert!(!body.contains("current_spec"), "body: {body}");
        Ok(())
    }
}

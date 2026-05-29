//! `loom status` — read-only snapshot of the active spec and its molecule.
//!
//! No locks are acquired (per the `Concurrency & Locking` lock matrix in
//! `specs/harness.md`). The command opens the state DB read-only,
//! fetches `current_spec` plus that spec's active molecule, and prints a
//! short summary.
//!
//! [`render`] formats the status to a `String` so the binary can route it to
//! stdout or the test harness can assert on the body verbatim.

use loom_driver::state::{MoleculeRow, StateDb, StateError};

use displaydoc::Display;
use thiserror::Error;

/// Snapshot returned by [`load`]. `None` for `current_spec` means the user
/// has not yet run `loom use <label>`. `molecule` is `None` when the active
/// spec has no live molecule. `integration_branch` is the configured loom
/// integration branch (default `main`); surfaced so the operator sees
/// which branch a fresh `loom loop` will target.
#[derive(Debug, Clone)]
pub struct StatusReport {
    pub current_spec: Option<String>,
    pub molecule: Option<MoleculeRow>,
    pub integration_branch: String,
}

/// Failures raised by [`load`].
#[derive(Debug, Display, Error)]
pub enum StatusError {
    /// state-db read failed
    State(#[from] StateError),
}

/// Read [`current_spec`](StateDb::current_spec) and — if present — the
/// cached molecule row for that spec from `db`. Read-only; the
/// per-machine cache is acceptable here because `loom status` is a
/// non-load-bearing listing. `integration_branch` is sourced from the
/// caller (typically `LoomConfig.loom.integration_branch`) so the
/// status surface reflects the operator's configured target without
/// requiring `loom-workflow` to re-parse the config.
pub fn load(db: &StateDb, integration_branch: String) -> Result<StatusReport, StatusError> {
    let current = db.current_spec()?;
    let molecule = match &current {
        Some(label) => db.molecule_for_spec(label)?,
        None => None,
    };
    Ok(StatusReport {
        current_spec: current.map(|s| s.to_string()),
        molecule,
        integration_branch,
    })
}

/// Render [`StatusReport`] as a multi-line, human-friendly string. Layout is
/// stable so tests can assert against the exact body.
pub fn render(report: &StatusReport) -> String {
    let mut out = String::new();
    match &report.current_spec {
        Some(label) => out.push_str(&format!("active spec: {label}\n")),
        None => out.push_str("active spec: <unset> (run `loom use <label>`)\n"),
    }
    match &report.molecule {
        Some(mol) => {
            out.push_str(&format!("molecule: {}\n", mol.id));
            out.push_str(&format!("iteration: {}\n", mol.iteration_count));
        }
        None => {
            out.push_str("molecule: <none>\n");
            out.push_str("iteration: 0\n");
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
    use loom_driver::identifier::{MoleculeId, SpecLabel};
    use loom_driver::state::ActiveMolecule;

    fn fresh_db(workspace: &std::path::Path) -> Result<StateDb> {
        Ok(StateDb::open(workspace.join(".wrapix/loom/state.db"))?)
    }

    #[test]
    fn empty_state_reports_unset_spec() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = fresh_db(dir.path())?;
        let report = load(&db, "main".to_string())?;
        assert!(report.current_spec.is_none());
        assert!(report.molecule.is_none());
        let body = render(&report);
        assert!(body.contains("<unset>"), "body: {body}");
        assert!(body.contains("iteration: 0"), "body: {body}");
        Ok(())
    }

    #[test]
    fn populated_state_reports_label_and_iteration() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("specs"))?;
        std::fs::write(dir.path().join("specs/harness.md"), "# x\n")?;
        let db = fresh_db(dir.path())?;
        db.rebuild(
            dir.path(),
            &[ActiveMolecule {
                id: MoleculeId::new("lm-3hhwq"),
                spec_label: SpecLabel::new("harness"),
                base_commit: None,
            }],
        )?;
        db.set_current_spec(&SpecLabel::new("harness"))?;
        db.increment_iteration(&MoleculeId::new("lm-3hhwq"))?;
        db.increment_iteration(&MoleculeId::new("lm-3hhwq"))?;

        let report = load(&db, "main".to_string())?;
        assert_eq!(report.current_spec.as_deref(), Some("harness"));
        let mol = report
            .molecule
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("molecule must be present"))?;
        assert_eq!(mol.iteration_count, 2);

        let body = render(&report);
        assert!(body.contains("harness"));
        assert!(body.contains("lm-3hhwq"));
        assert!(body.contains("iteration: 2"));
        Ok(())
    }

    /// The configured integration branch is surfaced verbatim in the
    /// rendered body — `loom status` is the operator's only on-the-fly
    /// reflection of `[loom] integration_branch` outside re-reading
    /// `config.toml` by hand.
    #[test]
    fn rendered_body_surfaces_integration_branch() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = fresh_db(dir.path())?;
        let report = load(&db, "trunk".to_string())?;
        let body = render(&report);
        assert!(
            body.contains("integration branch: trunk"),
            "configured integration branch must appear in status body: {body}",
        );
        Ok(())
    }

    /// `load` must be safe to call without any explicit lock; the lock-matrix
    /// row for read-only commands is "no lock acquired". This sanity check
    /// confirms the function compiles without borrowing a `LockGuard` and
    /// that an active spec lock does not influence the call.
    #[test]
    fn no_lock_required_to_call_load() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mgr = loom_driver::lock::LockManager::new(dir.path())?;
        let _spec_guard = mgr.acquire_spec(&SpecLabel::new("alpha"))?;
        let db = fresh_db(dir.path())?;
        let _ = load(&db, "main".to_string())?;
        Ok(())
    }
}

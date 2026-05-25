//! Active-molecule bootstrap for `loom plan`.
//!
//! At session start, `loom plan -n <label>` / `loom plan -u <label>` ensures
//! a `loom:active` epic exists for `<label>` so the next `loom todo` reads
//! `loom.base_commit` from Beads as a tier-1 diff base
//! (`specs/harness.md` § *Plan creates the molecule*).
//!
//! The helper is idempotent: when an active molecule already exists for the
//! anchor the call is a no-op (no metadata overwrite, no relabel).

use loom_driver::bd::{BdClient, CommandRunner, CreateOpts, ListOpts};
use loom_driver::identifier::SpecLabel;

use super::error::PlanError;

const ACTIVE_LABEL: &str = "loom:active";

/// Ensure a `loom:active` epic exists for `label` with
/// `loom.base_commit = head` recorded as metadata. Idempotent — when an
/// active molecule already exists for the anchor, returns without writing.
pub async fn ensure_active_molecule<R: CommandRunner>(
    bd: &BdClient<R>,
    head: &str,
    label: &SpecLabel,
) -> Result<(), PlanError> {
    let spec_label = format!("spec:{}", label.as_str());
    let candidates = bd
        .list(ListOpts {
            label: Some(ACTIVE_LABEL.to_string()),
            ..ListOpts::default()
        })
        .await?;
    let already = candidates.iter().any(|bead| {
        bead.labels
            .iter()
            .any(|l| l.as_str() == spec_label.as_str())
    });
    if already {
        return Ok(());
    }
    let metadata = serde_json::json!({ "loom.base_commit": head }).to_string();
    bd.create(CreateOpts {
        title: format!("{}: pending decomposition", label.as_str()),
        description: String::new(),
        issue_type: Some("epic".into()),
        labels: vec![spec_label, ACTIVE_LABEL.to_string()],
        metadata: Some(metadata),
        ..CreateOpts::default()
    })
    .await?;
    Ok(())
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests use panicking helpers")]
mod tests {
    use super::*;
    use anyhow::{Result, anyhow};
    use loom_driver::bd::{BdError, CommandRunner, RunOutput};
    use std::collections::VecDeque;
    use std::ffi::OsString;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Clone, Default)]
    struct CapturingRunner {
        responses: Arc<Mutex<VecDeque<RunOutput>>>,
        calls: Arc<Mutex<Vec<Vec<OsString>>>>,
    }

    impl CapturingRunner {
        fn new(responses: impl IntoIterator<Item = RunOutput>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into_iter().collect())),
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|argv| {
                    argv.iter()
                        .map(|a| a.to_string_lossy().into_owned())
                        .collect()
                })
                .collect()
        }
    }

    impl CommandRunner for CapturingRunner {
        async fn run(
            &self,
            args: Vec<OsString>,
            _timeout: Duration,
        ) -> std::result::Result<RunOutput, BdError> {
            self.calls.lock().unwrap().push(args);
            Ok(self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(RunOutput {
                    status: 0,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                }))
        }
    }

    fn ok(stdout: &[u8]) -> RunOutput {
        RunOutput {
            status: 0,
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        }
    }

    #[tokio::test]
    async fn creates_epic_when_no_active_molecule_exists_for_label() -> Result<()> {
        let runner = CapturingRunner::new([ok(b"null\n"), ok(b"wx-mol.1\n")]);
        let runner_handle = runner.clone();
        let client = BdClient::with_runner(runner);
        ensure_active_molecule(&client, "deadbeefcafe", &SpecLabel::new("harness")).await?;

        let calls = runner_handle.calls();
        assert_eq!(calls.len(), 2, "expected list+create calls, got {calls:?}");
        assert_eq!(calls[0][0], "list");
        assert!(calls[0].contains(&"--label=loom:active".to_string()));
        let create = &calls[1];
        assert_eq!(create[0], "create");
        assert!(create.contains(&"--silent".to_string()));
        let title_idx = create.iter().position(|a| a == "--title").unwrap();
        assert_eq!(create[title_idx + 1], "harness: pending decomposition");
        let type_idx = create.iter().position(|a| a == "--type").unwrap();
        assert_eq!(create[type_idx + 1], "epic");
        let labels_idx = create.iter().position(|a| a == "--labels").unwrap();
        assert_eq!(create[labels_idx + 1], "spec:harness,loom:active");
        let meta_idx = create.iter().position(|a| a == "--metadata").unwrap();
        assert_eq!(
            create[meta_idx + 1],
            r#"{"loom.base_commit":"deadbeefcafe"}"#
        );
        Ok(())
    }

    #[tokio::test]
    async fn no_op_when_active_molecule_already_exists_for_label() -> Result<()> {
        let existing = br#"[
            {
                "id": "wx-mol",
                "title": "loom-harness: pending decomposition",
                "status": "open",
                "priority": 2,
                "issue_type": "epic",
                "labels": ["spec:harness", "loom:active"]
            }
        ]"#;
        let runner = CapturingRunner::new([ok(existing)]);
        let runner_handle = runner.clone();
        let client = BdClient::with_runner(runner);
        ensure_active_molecule(&client, "newhead", &SpecLabel::new("harness")).await?;

        let calls = runner_handle.calls();
        assert_eq!(
            calls.len(),
            1,
            "idempotent reuse must not issue bd create: {calls:?}",
        );
        assert_eq!(calls[0][0], "list");
        Ok(())
    }

    #[tokio::test]
    async fn active_molecule_for_other_spec_does_not_block_creation() -> Result<()> {
        let other_spec = br#"[
            {
                "id": "wx-other",
                "title": "other: pending decomposition",
                "status": "open",
                "priority": 2,
                "issue_type": "epic",
                "labels": ["spec:other-anchor", "loom:active"]
            }
        ]"#;
        let runner = CapturingRunner::new([ok(other_spec), ok(b"wx-mol.2\n")]);
        let runner_handle = runner.clone();
        let client = BdClient::with_runner(runner);
        ensure_active_molecule(&client, "abc123", &SpecLabel::new("harness")).await?;

        let calls = runner_handle.calls();
        assert_eq!(
            calls.len(),
            2,
            "different-spec active molecule must not satisfy our label: {calls:?}",
        );
        assert_eq!(calls[1][0], "create");
        Ok(())
    }

    /// Spec contract `[test]` annotation
    /// (`specs/harness.md` § Success Criteria · State DB):
    /// `loom plan -n/-u` writes `loom.base_commit = HEAD` as bead metadata
    /// on the newly-created (or reused) `loom:active` epic at session
    /// start; idempotent re-runs do not overwrite an existing active
    /// molecule's metadata. Exercises both branches in a single
    /// session-shape: first run creates with metadata; second run on an
    /// already-present molecule for the same label is a no-op.
    #[tokio::test]
    async fn plan_writes_base_commit_to_bead_metadata_at_session_start() -> Result<()> {
        let cold = CapturingRunner::new([ok(b"null\n"), ok(b"wx-mol.1\n")]);
        let cold_handle = cold.clone();
        let client = BdClient::with_runner(cold);
        ensure_active_molecule(&client, "deadbeef", &SpecLabel::new("harness")).await?;

        let cold_calls = cold_handle.calls();
        assert_eq!(cold_calls.len(), 2);
        let create = &cold_calls[1];
        assert_eq!(create[0], "create");
        let labels_idx = create.iter().position(|a| a == "--labels").unwrap();
        assert_eq!(create[labels_idx + 1], "spec:harness,loom:active");
        let meta_idx = create.iter().position(|a| a == "--metadata").unwrap();
        assert_eq!(create[meta_idx + 1], r#"{"loom.base_commit":"deadbeef"}"#);

        let existing = br#"[
            {
                "id": "wx-mol.1",
                "title": "loom-harness: pending decomposition",
                "status": "open",
                "priority": 2,
                "issue_type": "epic",
                "labels": ["spec:harness", "loom:active"]
            }
        ]"#;
        let warm = CapturingRunner::new([ok(existing)]);
        let warm_handle = warm.clone();
        let client = BdClient::with_runner(warm);
        ensure_active_molecule(&client, "newhead", &SpecLabel::new("harness")).await?;

        let warm_calls = warm_handle.calls();
        assert_eq!(
            warm_calls.len(),
            1,
            "idempotent reuse must not overwrite metadata: {warm_calls:?}",
        );
        assert_eq!(warm_calls[0][0], "list");
        assert!(!warm_calls[0].iter().any(|a| a == "create"));
        Ok(())
    }

    #[tokio::test]
    async fn list_failure_propagates_as_plan_error() -> Result<()> {
        let runner = CapturingRunner::new([RunOutput {
            status: 1,
            stdout: Vec::new(),
            stderr: b"bd boom".to_vec(),
        }]);
        let client = BdClient::with_runner(runner);
        let err = ensure_active_molecule(&client, "head", &SpecLabel::new("x"))
            .await
            .err()
            .ok_or_else(|| anyhow!("expected propagated bd failure"))?;
        assert!(matches!(err, PlanError::Bd(_)), "got {err:?}");
        Ok(())
    }
}

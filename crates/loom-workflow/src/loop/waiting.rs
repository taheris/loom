use loom_driver::bd::{BdClient, CommandRunner};
use loom_driver::identifier::BeadId;

use super::{AgentOutcome, LoopError};

/// Stable recovery cause for a dependency-wait marker whose Beads state does
/// not prove a real wait.
pub(crate) const INVALID_WAITING_CAUSE: &str = "invalid-waiting";

/// Non-empty set of active Beads blockers that authorized a loop wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveBlockers {
    first: BeadId,
    rest: Vec<BeadId>,
}

impl ActiveBlockers {
    /// Construct a non-empty blocker set.
    #[must_use]
    pub(crate) fn new(first: BeadId, rest: Vec<BeadId>) -> Self {
        Self { first, rest }
    }

    /// Number of active blockers proving this wait.
    #[must_use]
    pub fn count(&self) -> usize {
        1 + self.rest.len()
    }
}

/// Validate a parsed `LOOM_WAITING` request against authoritative Beads state.
///
/// Non-waiting outcomes pass through unchanged. A valid wait becomes the
/// scheduler-safe [`AgentOutcome::Waiting`] shape carrying a non-empty blocker
/// set. Invalid wait requests become ordinary recovery failures, so they never
/// silently park an unblocked or closed bead.
pub async fn validate_waiting_outcome<R: CommandRunner>(
    bd: &BdClient<R>,
    bead: &BeadId,
    outcome: AgentOutcome,
) -> Result<AgentOutcome, LoopError> {
    if outcome != AgentOutcome::WaitingRequested {
        return Ok(outcome);
    }

    let snapshot = bd.dependency_snapshot(bead).await?;
    if !snapshot.is_open() {
        return Ok(invalid_waiting(format!(
            "bead {bead} has status {}, expected open",
            snapshot.status_label(),
        )));
    }

    let mut blockers = snapshot.active_blockers().into_iter();
    let Some(first) = blockers.next() else {
        return Ok(invalid_waiting(format!(
            "bead {bead} has no active declared blocking dependency",
        )));
    };
    Ok(AgentOutcome::Waiting {
        blockers: ActiveBlockers::new(first, blockers.collect()),
    })
}

fn invalid_waiting(detail: String) -> AgentOutcome {
    AgentOutcome::Failure {
        error: format!("{INVALID_WAITING_CAUSE}: {detail}"),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::time::Duration;

    use loom_driver::bd::{BdError, RunOutput};

    use super::*;

    #[derive(Clone)]
    struct SnapshotRunner {
        json: &'static str,
    }

    impl CommandRunner for SnapshotRunner {
        async fn run(
            &self,
            _args: Vec<OsString>,
            _timeout: Duration,
        ) -> Result<RunOutput, BdError> {
            Ok(RunOutput {
                status: 0,
                stdout: self.json.as_bytes().to_vec(),
                stderr: Vec::new(),
            })
        }
    }

    fn client(json: &'static str) -> BdClient<SnapshotRunner> {
        BdClient::with_runner(SnapshotRunner { json })
    }

    fn bead_id(raw: &str) -> BeadId {
        BeadId::new(raw).expect("valid bead id fixture")
    }

    #[tokio::test]
    async fn waiting_marker_requires_open_bead_with_active_blocker() {
        let bd = client(
            r#"[{"status":"open","dependencies":[
                {"id":"lm-blocker","status":"in_progress","dependency_type":"blocks"},
                {"id":"lm-done","status":"closed","dependency_type":"blocks"}
            ]}]"#,
        );

        let outcome =
            validate_waiting_outcome(&bd, &bead_id("lm-wait"), AgentOutcome::WaitingRequested)
                .await
                .expect("dependency snapshot parses");

        match outcome {
            AgentOutcome::Waiting { blockers } => {
                assert_eq!(
                    blockers,
                    ActiveBlockers::new(bead_id("lm-blocker"), Vec::new()),
                );
            }
            other => panic!("expected validated waiting outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn waiting_marker_without_active_blocker_routes_to_recovery() {
        let bd = client(
            r#"[{"status":"open","dependencies":[
                {"id":"lm-done","status":"closed","dependency_type":"blocks"},
                {"id":"lm-parent","status":"open","dependency_type":"parent-child"}
            ]}]"#,
        );

        let outcome =
            validate_waiting_outcome(&bd, &bead_id("lm-wait"), AgentOutcome::WaitingRequested)
                .await
                .expect("dependency snapshot parses");

        assert!(matches!(
            outcome,
            AgentOutcome::Failure { error }
                if error.contains(INVALID_WAITING_CAUSE)
                    && error.contains("no active declared blocking dependency")
        ));
    }

    #[tokio::test]
    async fn waiting_marker_on_closed_bead_routes_to_recovery() {
        let bd = client(
            r#"[{"status":"closed","dependencies":[
                {"id":"lm-blocker","status":"open","dependency_type":"blocks"}
            ]}]"#,
        );

        let outcome =
            validate_waiting_outcome(&bd, &bead_id("lm-wait"), AgentOutcome::WaitingRequested)
                .await
                .expect("dependency snapshot parses");

        assert!(matches!(
            outcome,
            AgentOutcome::Failure { error }
                if error.contains(INVALID_WAITING_CAUSE)
                    && error.contains("status closed, expected open")
        ));
    }
}

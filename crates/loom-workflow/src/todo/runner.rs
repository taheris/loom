//! Backend-agnostic driver for `loom todo`.
//!
//! [`run`] is the entry point the binary calls to execute one todo session.
//! The function takes a controller (which owns spec/state/bd interactions)
//! and a `spawn` closure (which the binary builds out of the per-phase
//! `dispatch` function). Keeping the spawn step generic over a closure
//! preserves static dispatch — the binary monomorphizes `run_agent::<B>` per
//! concrete backend inside its `dispatch` match, and this driver never sees
//! the backend type.

use loom_driver::agent::{ProtocolError, SessionOutcome, SpawnConfig};
use loom_driver::scratch::ScratchSession;

use loom_driver::identifier::SpecLabel;
use loom_protocol::todo::TodoSuccess;

use super::ExitSignal;
use super::error::TodoError;

/// Bundle of the spawn config and the scratch-session guard that owns
/// `.loom/scratch/<label>/` for the duration of the dispatch. The
/// runner drops the guard after the agent returns so cleanup runs on
/// every exit path (success, failure, panic).
pub struct TodoSession {
    pub config: SpawnConfig,
    pub scratch: ScratchSession,
}

/// Side-effect surface the [`run`] driver depends on.
///
/// The trait is deliberately shallow — it owns the spec read, prompt build,
/// and outcome recording but leaves agent invocation to the caller-provided
/// `spawn` closure. The split keeps backend selection out of the controller
/// (the binary's `dispatch` function owns that) while letting tests substitute
/// fakes for the bd/state/git surface.
pub trait TodoController: Send {
    /// Open the per-session scratch dir and compute the spawn config for
    /// the upcoming todo session — resolve the tier decision, render the
    /// prompt, build the env allowlist, and pin the scratch dir into the
    /// returned [`TodoSession`].
    fn build_session(
        &mut self,
    ) -> impl std::future::Future<Output = Result<TodoSession, TodoError>> + Send;

    /// Persist the agent outcome — write per-spec cursors, commit the spec
    /// file, etc. Called once after the agent session completes. `marker`
    /// is the exit signal parsed from the agent's final turn so
    /// implementations can gate cursor writes per
    /// `specs/harness.md` lines 902-918.
    fn record_outcome(
        &mut self,
        outcome: &SessionOutcome,
        marker: Option<&ExitSignal>,
        todo_success: Option<&TodoSuccess>,
    ) -> impl std::future::Future<Output = Result<TodoRecord, TodoError>> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TodoRecord {
    pub spec_outcomes: Vec<TodoSpecSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoSpecSummary {
    pub label: SpecLabel,
    pub outcome: String,
}

/// Summary of one [`run`] invocation surfaced to the binary so it can print
/// a human-readable line and exit appropriately.
#[derive(Debug, Clone, PartialEq)]
pub struct TodoSummary {
    pub exit_code: i32,
    pub cost_usd: Option<f64>,
    pub spec_outcomes: Vec<TodoSpecSummary>,
}

/// Drive one `loom todo` session: build the spawn config, hand it to the
/// caller-provided agent dispatcher, then record the outcome.
///
/// `spawn` is the per-phase backend dispatcher closure. The binary builds it
/// from `dispatch(Phase::Todo, &config, _)` so the workflow stays
/// backend-agnostic. Errors from the closure are surfaced as
/// [`TodoError::Protocol`] — the binary maps them to the user-visible exit
/// status.
pub async fn run<C, S, F>(controller: &mut C, spawn: S) -> Result<TodoSummary, TodoError>
where
    C: TodoController + ?Sized,
    S: FnOnce(SpawnConfig) -> F,
    F: std::future::Future<
            Output = Result<
                (SessionOutcome, Option<ExitSignal>, Option<TodoSuccess>),
                ProtocolError,
            >,
        >,
{
    let TodoSession { config, scratch } = controller.build_session().await?;
    let result = spawn(config).await;
    drop(scratch);
    let (outcome, marker, todo_success) = result?;
    let record = controller
        .record_outcome(&outcome, marker.as_ref(), todo_success.as_ref())
        .await?;
    Ok(TodoSummary {
        exit_code: outcome.exit_code,
        cost_usd: outcome.cost_usd,
        spec_outcomes: record.spec_outcomes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::agent::RePinContent;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct FakeController {
        workspace: tempfile::TempDir,
        recorded: AtomicU32,
        last_exit: std::sync::Mutex<Option<i32>>,
        last_marker: std::sync::Mutex<Option<ExitSignal>>,
    }

    impl FakeController {
        fn new() -> Self {
            Self {
                workspace: tempfile::tempdir().unwrap(),
                recorded: AtomicU32::new(0),
                last_exit: std::sync::Mutex::new(None),
                last_marker: std::sync::Mutex::new(None),
            }
        }
    }

    impl TodoController for FakeController {
        async fn build_session(&mut self) -> Result<TodoSession, TodoError> {
            let scratch = ScratchSession::open(
                self.workspace.path(),
                "test-spec",
                "todo prompt",
                "loom todo @ test-spec",
            )
            .unwrap();
            let scratch_dir = scratch.path().to_path_buf();
            Ok(TodoSession {
                config: SpawnConfig {
                    image_ref: "wrix-base:latest".into(),
                    image_source: PathBuf::from("/nix/store/zzz-wrix-base.tar"),
                    image_source_kind: Some(loom_driver::agent::ImageSourceKind::NixDescriptor),
                    profile_config: None,
                    workspace: self.workspace.path().to_path_buf(),
                    env: vec![],
                    mounts: vec![],
                    initial_prompt: "todo prompt".into(),
                    agent_args: vec![],
                    repin: RePinContent {
                        orientation: String::new(),
                        pinned_context: String::new(),
                        partial_bodies: vec![],
                    },
                    skills: None,
                    scratch_dir,
                    model: None,
                    thinking_level: None,
                    output_limits: None,
                    shutdown_grace: None,
                    handshake_timeout: None,
                    stall_warn_interval: None,
                    launcher_env: Vec::new(),
                },
                scratch,
            })
        }

        async fn record_outcome(
            &mut self,
            outcome: &SessionOutcome,
            marker: Option<&ExitSignal>,
            _todo_success: Option<&TodoSuccess>,
        ) -> Result<TodoRecord, TodoError> {
            self.recorded.fetch_add(1, Ordering::SeqCst);
            *self.last_exit.lock().unwrap() = Some(outcome.exit_code);
            *self.last_marker.lock().unwrap() = marker.cloned();
            Ok(TodoRecord::default())
        }
    }

    #[tokio::test]
    async fn run_threads_spawn_outcome_through_controller() {
        let mut controller = FakeController::new();
        let summary = run(&mut controller, |cfg: SpawnConfig| {
            assert_eq!(cfg.initial_prompt, "todo prompt");
            async move {
                Ok((
                    SessionOutcome {
                        exit_code: 0,
                        cost_usd: Some(0.42),
                    },
                    Some(ExitSignal::Complete),
                    None,
                ))
            }
        })
        .await
        .expect("run ok");
        assert_eq!(summary.exit_code, 0);
        assert_eq!(summary.cost_usd, Some(0.42));
        assert!(summary.spec_outcomes.is_empty());
        assert_eq!(controller.recorded.load(Ordering::SeqCst), 1);
        assert_eq!(*controller.last_exit.lock().unwrap(), Some(0));
        assert_eq!(
            *controller.last_marker.lock().unwrap(),
            Some(ExitSignal::Complete),
        );
    }

    #[tokio::test]
    async fn spawn_error_is_surfaced_as_protocol_error() {
        let mut controller = FakeController::new();
        let result = run(&mut controller, |_cfg: SpawnConfig| async {
            Err(ProtocolError::Unsupported)
        })
        .await;
        match result {
            Err(TodoError::Protocol(ProtocolError::Unsupported)) => {}
            other => panic!("expected Protocol(Unsupported), got {other:?}"),
        }
        // Outcome never recorded when the agent fails.
        assert_eq!(controller.recorded.load(Ordering::SeqCst), 0);
    }
}

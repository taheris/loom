//! Two-pass commit-signature verification shared by the sequential
//! (`production`) and parallel (`parallel`) per-bead integration paths.
//!
//! `specs/harness.md` § Verdict Gate mandates that **every** landed bead
//! verify commit signatures in two passes — pass 1 over the fetched worker
//! commits, pass 2 over the rebased/rewritten commits — before the
//! ff-merge moves the integration branch. Both the sequential `run_bead`
//! and the parallel `merge_back_one` integration steps drive the same
//! [`GitClient::verify_commit_range`] check, emit the same
//! `signature-verification-failed` driver event, and delete the transient
//! `loom/<id>` ref on rejection. Factoring the pass into one helper keeps
//! the two paths from drifting (the parallel path previously skipped
//! verification entirely).

use loom_driver::git::{GitClient, SignatureCheck};
use loom_driver::identifier::BeadId;
use loom_events::DriverKind;
use tracing::warn;

use super::driver_emit::BeadEmit;
use super::error::LoopError;

/// Observed verifier process state; none of these values authorize publication.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum ExecutionState {
    Completed,
    Rejected,
    Unavailable,
    Interrupted,
    TimedOut,
}

impl ExecutionState {
    const fn infrastructure(self) -> bool {
        matches!(self, Self::Unavailable | Self::Interrupted | Self::TimedOut)
    }
}

pub(super) struct Execution {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub state: ExecutionState,
}

impl Execution {
    pub const fn infrastructure(&self) -> bool {
        self.state.infrastructure()
    }
}

/// Execute only the caller's existing trusted Gate command with the configured infrastructure budget.
pub(super) async fn execute_gate(
    command: &mut tokio::process::Command,
    workspace: &std::path::Path,
    clock: &dyn loom_driver::clock::Clock,
    timeout: std::time::Duration,
    max_attempts: u32,
) -> Result<Execution, LoopError> {
    use std::io::Write as _;
    let head = loom_driver::git::sync_head_commit_sha(workspace)?;
    let tree = loom_driver::git::head_tree_oid_sync(workspace)?;
    let config = loom_driver::config::LoomConfig::resolve_path(workspace);
    let config = if config.exists() {
        std::fs::read(config)?
    } else {
        Vec::new()
    };
    let config_digest = blake3::hash(&config).to_hex().to_string();
    let path = workspace.join(".loom/logs/gate/verification-attempts.jsonl");
    let parent = path.parent().ok_or_else(|| LoopError::Bug {
        context: "verification log has no parent".into(),
    })?;
    std::fs::create_dir_all(parent)?;
    let max_attempts = max_attempts.max(1);
    for attempt in 1..=max_attempts {
        if loom_driver::git::sync_head_commit_sha(workspace)? != head
            || loom_driver::git::head_tree_oid_sync(workspace)? != tree
        {
            return Err(LoopError::Bug { context: "verification candidate changed during infrastructure recovery; no coverage admitted".into() });
        }
        let execution = match loom_driver::process::output(command, clock, timeout).await {
            Ok(output) => {
                let code = output.status.code();
                #[cfg(unix)]
                let recorded_code = {
                    use std::os::unix::process::ExitStatusExt as _;
                    code.unwrap_or_else(|| 128 + output.status.signal().unwrap_or(0))
                };
                #[cfg(not(unix))]
                let recorded_code = code.unwrap_or(128);
                let state = match code {
                    Some(0) => ExecutionState::Completed,
                    Some(77 | 78) => ExecutionState::Unavailable,
                    Some(124) => ExecutionState::TimedOut,
                    Some(137) | None => ExecutionState::Interrupted,
                    Some(_) => ExecutionState::Rejected,
                };
                Execution {
                    code: recorded_code,
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    state,
                }
            }
            Err(loom_driver::process::Error::Timeout) => Execution {
                code: 124,
                stdout: String::new(),
                stderr: "verifier timed out; its owned process group was terminated and reaped"
                    .into(),
                state: ExecutionState::TimedOut,
            },
            Err(loom_driver::process::Error::Io(error)) => Execution {
                code: 78,
                stdout: String::new(),
                stderr: format!("verifier process unavailable: {error}"),
                state: ExecutionState::Unavailable,
            },
            Err(loom_driver::process::Error::Cleanup(error)) => return Err(LoopError::Io(error)),
        };
        let record = serde_json::json!({
            "event":"verification-process-observation", "head":head, "tree":tree,
            "config_digest":config_digest, "program":command.as_std().get_program().to_string_lossy(),
            "argv":command.as_std().get_args().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>(),
            "attempt":attempt, "max_attempts":max_attempts, "state":execution.state,
            "exit_code":execution.code, "stdout":execution.stdout, "stderr":execution.stderr,
            "coverage_admitted":false,
        });
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        log.lock()?;
        writeln!(log, "{record}")?;
        log.sync_data()?;
        if loom_driver::git::sync_head_commit_sha(workspace)? != head
            || loom_driver::git::head_tree_oid_sync(workspace)? != tree
        {
            return Err(LoopError::Bug {
                context: "verification candidate changed while its verifier ran; observation retained without coverage admission".into(),
            });
        }
        if !execution.infrastructure() || attempt == max_attempts {
            return Ok(execution);
        }
        tracing::warn!(attempt, max_attempts, exit_code = execution.code, log = %path.display(), "retrying verifier infrastructure without source redispatch or semantic retry");
        clock.sleep(std::time::Duration::from_secs(1)).await;
    }
    Err(LoopError::Bug {
        context: "nonempty verification attempt budget exhausted without an observation".into(),
    })
}

/// Which side of the per-bead integration a signature-verification pass
/// covers.
///
/// The two variants are the closed, codebase-owned set the
/// verdict gate enumerates (`specs/harness.md` § Verdict Gate phases 2 &
/// 4); modelling them as an enum rather than a `&str` keeps a typo'd side
/// label from silently corrupting the operator-facing routing detail
/// (RS-17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyPass {
    /// Pass 1 — the fetched worker commits. A rejected signature here
    /// means the wrix container's signing setup is broken; the bead
    /// routes to `loom:blocked` (worker-side).
    Worker,
    /// Pass 2 — the rebased commits the integration rewrite produced. A
    /// rejected signature here means loom's own (driver-side) signing
    /// setup is broken; the bead routes to `loom:blocked` (driver-side).
    Driver,
}

impl VerifyPass {
    /// Operator-facing side label that rides into the
    /// `signature-verification-failed ({side}-side)` reason string, the
    /// `warn!` field, and the `side` field of the
    /// `SignatureVerificationFailed` JSONL payload.
    pub const fn side(self) -> &'static str {
        match self {
            VerifyPass::Worker => "worker",
            VerifyPass::Driver => "driver",
        }
    }
}

/// Run one `git verify-commit` pass over `range` in the loom workspace.
///
/// Returns `Ok(None)` when the pass verified (or was skipped because no
/// signing key resolved). Returns `Ok(Some(reason))` on a rejected
/// signature — the operator-facing reason string. On that path the
/// `SignatureVerificationFailed` driver event is emitted (when `emit` is
/// `Some`), a `warn!` is logged, and the transient `branch` is deleted
/// unconditionally so a later dispatch's fetch starts clean. The caller
/// wraps the reason into its own outcome type
/// ([`AgentOutcome::SignatureVerificationFailed`] for the sequential
/// path, [`BatchResult::AgentBlocked`] for the parallel path).
///
/// [`AgentOutcome::SignatureVerificationFailed`]: super::outcome::AgentOutcome::SignatureVerificationFailed
/// [`BatchResult::AgentBlocked`]: super::parallel::BatchResult::AgentBlocked
///
/// # Errors
///
/// Returns an error when loop state, agent execution, or gate handling fails.
pub async fn verify_pass(
    git: &GitClient,
    emit: Option<&mut BeadEmit>,
    bead: &BeadId,
    branch: &str,
    range: &str,
    pass: VerifyPass,
) -> Result<Option<String>, LoopError> {
    match git.verify_commit_range(range).await? {
        SignatureCheck::Skipped | SignatureCheck::Verified => Ok(None),
        SignatureCheck::Failed { commit, detail } => {
            let side = pass.side();
            let reason = format!(
                "signature-verification-failed ({side}-side): \
                 git verify-commit rejected {commit} in range {range} — {detail}",
            );
            warn!(
                bead = %bead,
                branch,
                side,
                commit = %commit,
                "signature verification failed — routing to loom:blocked",
            );
            if let Some(emit) = emit {
                emit.emit(
                    DriverKind::SignatureVerificationFailed,
                    &format!("signature verification failed ({side}-side): {commit}"),
                    serde_json::json!({
                        "bead_id": bead.to_string(),
                        "branch": branch,
                        "side": side,
                        "commit": commit,
                        "range": range,
                        "detail": detail,
                    }),
                );
            }
            // `loom/<id>` ref deleted unconditionally on this exit path. A
            // successful rebase (pass 2) leaves the workspace on the rewritten
            // bead branch, so return to the integration branch first — git
            // refuses to delete the checked-out branch, and this restores the
            // untouched integration line as the checked-out state. On pass 1
            // the workspace is already on the integration branch (no-op).
            git.checkout_integration().await?;
            git.delete_branch(branch).await?;
            Ok(Some(reason))
        }
    }
}

#[cfg(test)]
mod execution_tests {
    use super::*;
    use loom_driver::clock::SystemClock;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn fixture(body: &str) -> (tempfile::TempDir, PathBuf, tokio::process::Command) {
        let root = tempfile::tempdir().expect("fixture");
        let workspace = root.path().join("workspace");
        loom_driver::git::init_test_repo_with_integration(&workspace).expect("real Git workspace");
        let script = root.path().join("verifier.sh");
        loom_test_support::write_executable_bash_script(&script, body).expect("external verifier");
        let mut command = tokio::process::Command::new("bash");
        command.arg(script).current_dir(&workspace);
        (root, workspace, command)
    }

    fn observations(workspace: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(workspace.join(".loom/logs/gate/verification-attempts.jsonl"))
            .expect("retained attempts")
            .lines()
            .map(|line| serde_json::from_str(line).expect("observation JSON"))
            .collect()
    }

    #[tokio::test]
    async fn interrupted_verifier_recovers_without_source_redispatch() {
        let (_root, workspace, mut command) = fixture(
            "set -euo pipefail\nmkdir -p .loom\nif [[ ! -f .loom/interrupted ]]; then\n touch .loom/interrupted\n kill -KILL $$\nfi\nprintf 'actual recovered verification\\n'\n",
        );
        let execution = execute_gate(
            &mut command,
            &workspace,
            &SystemClock::new(),
            Duration::from_secs(30),
            3,
        )
        .await
        .expect("bounded execution");
        assert_eq!(execution.code, 0);
        assert!(execution.stdout.contains("actual recovered verification"));
        let attempts = observations(&workspace);
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0]["state"], "interrupted");
        assert_eq!(attempts[1]["state"], "completed");
        assert!(attempts.iter().all(|row| row["coverage_admitted"] == false));
        assert_eq!(attempts[0]["head"], attempts[1]["head"]);
    }

    #[tokio::test]
    async fn unavailable_verifier_exhausts_only_its_infrastructure_budget() {
        let (_root, workspace, mut command) = fixture(
            "set -euo pipefail\nprintf 'required isolated kernel capability unavailable\\n' >&2\nexit 78\n",
        );
        let execution = execute_gate(
            &mut command,
            &workspace,
            &SystemClock::new(),
            Duration::from_secs(30),
            3,
        )
        .await
        .expect("bounded execution");
        assert!(execution.infrastructure());
        assert_eq!(execution.code, 78);
        let attempts = observations(&workspace);
        assert_eq!(attempts.len(), 3);
        assert!(
            attempts
                .iter()
                .all(|row| row["state"] == "unavailable" && row["coverage_admitted"] == false)
        );
    }

    #[tokio::test]
    async fn semantic_system_failure_is_never_retried_as_infrastructure() {
        let (_root, workspace, mut command) =
            fixture("set -euo pipefail\nprintf 'real failing system assertion\\n' >&2\nexit 1\n");
        let execution = execute_gate(
            &mut command,
            &workspace,
            &SystemClock::new(),
            Duration::from_secs(30),
            3,
        )
        .await
        .expect("bounded execution");
        assert!(!execution.infrastructure());
        assert_eq!(execution.code, 1);
        assert_eq!(observations(&workspace).len(), 1);
    }

    #[tokio::test]
    async fn verifier_deadline_terminates_and_reaps_the_running_process() {
        use loom_driver::clock::{BoxFuture, Clock};
        use tokio::io::{AsyncBufReadExt, BufReader};
        use tokio::net::TcpListener;
        struct TriggerClock(tokio::sync::Notify);
        impl Clock for TriggerClock {
            fn now(&self) -> std::time::Instant {
                SystemClock::new().now()
            }
            fn wall_now(&self) -> std::time::SystemTime {
                SystemClock::new().wall_now()
            }
            fn sleep(&self, _: Duration) -> BoxFuture<'_, ()> {
                Box::pin(self.0.notified())
            }
        }
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture listener");
        let port = listener.local_addr().expect("listener address").port();
        let (_root, workspace, mut command) = fixture(&format!(
            "set -euo pipefail\nexec 3<>/dev/tcp/127.0.0.1/{port}\nprintf 'ready\\n' >&3\nread -r release <&3\nprintf 'late write\\n' > .loom/late-write\n"
        ));
        let clock = TriggerClock(tokio::sync::Notify::new());
        let run = execute_gate(&mut command, &workspace, &clock, Duration::from_secs(30), 1);
        let observe = async {
            let (stream, _) = listener.accept().await.expect("actual verifier connected");
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("startup signal");
            assert_eq!(line, "ready\n");
            clock.0.notify_one();
            line.clear();
            assert_eq!(
                reader
                    .read_line(&mut line)
                    .await
                    .expect("terminated verifier socket"),
                0
            );
        };
        let (execution, ()) = tokio::join!(run, observe);
        let execution = execution.expect("deadline handled");
        assert_eq!(execution.code, 124);
        assert!(execution.infrastructure());
        assert!(!workspace.join(".loom/late-write").exists());
        assert_eq!(observations(&workspace)[0]["state"], "timed-out");
    }
}

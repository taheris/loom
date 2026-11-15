use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;

use crate::clock::{Clock, SystemClock};

use super::{BdError, CommandRunner, RunOutput};

/// Default runner for the `bd` subprocess.
///
/// Each argument is passed through `.arg()` so no shell is involved. The subprocess timeout is
/// driven by the injected [`Clock`] so tests can substitute
/// [`crate::clock::MockClock`]. Timeouts terminate the process group and reap the child;
/// cancellation kills the group and lets Tokio reap the child in the background.
/// Neither outcome implies that earlier mutations were rolled back.
#[derive(Clone)]
pub struct TokioRunner {
    clock: Arc<dyn Clock>,
    publication: Option<Publication>,
}

#[derive(Clone)]
struct Publication {
    program: PathBuf,
    workspace: PathBuf,
}

impl TokioRunner {
    /// Build a runner that uses `clock` for the per-call timeout.
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            publication: None,
        }
    }

    /// Select the driver-owned publication transport without changing worker shared-state access.
    #[must_use]
    pub fn with_publication(mut self, program: &Path, workspace: &Path) -> Self {
        self.publication = Some(Publication {
            program: program.to_path_buf(),
            workspace: workspace.to_path_buf(),
        });
        self
    }

    fn publication_command(&self) -> Result<Command, BdError> {
        let publication = self
            .publication
            .as_ref()
            .ok_or_else(|| BdError::Unsupported {
                reason: "tracking publication transport is not configured".into(),
            })?;
        let mut command = Command::new(&publication.program);
        command
            .current_dir(&publication.workspace)
            .args(["beads", "push"]);
        Ok(command)
    }

    async fn run_command(
        &self,
        command: &mut Command,
        args: &[OsString],
        timeout: Duration,
    ) -> Result<RunOutput, BdError> {
        command.env("BD_JSON_ENVELOPE", "1");
        let output = crate::process::output(command, self.clock.as_ref(), timeout)
            .await
            .map_err(|error| match error {
                crate::process::Error::Io(error) => BdError::Spawn(error),
                crate::process::Error::Cleanup(error) => BdError::Cleanup(error),
                crate::process::Error::Timeout => BdError::Timeout {
                    args: render_args(args),
                },
            })?;
        Ok(RunOutput {
            status: output.status.code().unwrap_or(-1),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

impl Default for TokioRunner {
    fn default() -> Self {
        Self::with_clock(Arc::new(SystemClock::new()))
    }
}

impl std::fmt::Debug for TokioRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokioRunner").finish_non_exhaustive()
    }
}

impl CommandRunner for TokioRunner {
    async fn run(&self, args: Vec<OsString>, t: Duration) -> Result<RunOutput, BdError> {
        let mut cmd = Command::new("bd");
        for arg in &args {
            cmd.arg(arg);
        }
        self.run_command(&mut cmd, &args, t).await
    }

    async fn publish(&self, timeout: Duration) -> Result<(), BdError> {
        let mut command = self.publication_command()?;
        let args = [OsString::from("beads"), OsString::from("push")];
        let output = self.run_command(&mut command, &args, timeout).await?;
        if output.success() {
            Ok(())
        } else {
            Err(BdError::Cli {
                status: output.status,
                args: render_args(&args),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
        }
    }
}

pub(super) fn render_args(args: &[OsString]) -> String {
    args.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use crate::process::tests::{Fixture, TriggerClock, bounded};

    /// An OS process and descendant expose writes that would otherwise outlive the bd timeout.
    #[tokio::test]
    async fn bd_timeout_terminates_descendants_before_returning() {
        bounded(async {
            let fixture = Fixture::new().await;
            let runner = TokioRunner::with_clock(fixture.clock.clone());
            let mut command = fixture.command();
            let args = [OsString::from("update"), OsString::from("lm-1")];
            let (result, peers) = tokio::join!(
                runner.run_command(&mut command, &args, Duration::from_secs(60)),
                async {
                    let peers = fixture.ready(2).await;
                    fixture.clock.expire();
                    peers
                },
            );
            assert!(matches!(result, Err(BdError::Timeout { args }) if args == "update lm-1"));
            fixture.assert_stopped(peers).await;
        })
        .await;
    }

    /// Real pipes verify that normal capture survives the cancellation-safe execution boundary.
    #[tokio::test]
    async fn bd_runner_preserves_success_and_output() {
        bounded(async {
            let runner = TokioRunner::with_clock(Arc::new(TriggerClock::default()));
            let mut command = Command::new("bash");
            command.args(["-c", "printf 'stdout'; printf 'stderr' >&2"]);
            let result = runner
                .run_command(&mut command, &[], Duration::from_secs(60))
                .await
                .expect("run");
            assert!(result.success());
            assert_eq!(result.stdout, b"stdout");
            assert_eq!(result.stderr, b"stderr");
        })
        .await;
    }

    #[tokio::test]
    async fn bd_runner_opts_into_json_envelope() {
        let runner = TokioRunner::default();
        let mut command = Command::new("bash");
        command.env("BD_JSON_ENVELOPE", "0");
        command.args(["-c", "printf '%s' \"$BD_JSON_ENVELOPE\""]);
        let result = runner
            .run_command(&mut command, &[], Duration::from_secs(60))
            .await
            .expect("run");
        assert_eq!(result.stdout, b"1");
    }

    #[tokio::test]
    async fn publication_requires_explicit_transport_and_keeps_worker_sync_local() {
        use loom_issues::tracking::{Error, Store, Synchronization};
        let store = crate::bd::BdClient::with_runner(TokioRunner::default());
        store
            .synchronize(Synchronization::SharedState)
            .await
            .unwrap();
        assert!(matches!(
            store.synchronize(Synchronization::Publish).await,
            Err(Error::Unsupported { .. })
        ));
    }

    #[tokio::test]
    async fn publication_uses_selected_workspace_and_preserves_uncertain_nonzero_status() {
        use loom_issues::tracking::{Error, Store, Synchronization};
        use std::os::unix::fs::PermissionsExt as _;
        let workspace = tempfile::tempdir().unwrap();
        let program = workspace.path().join("wrix");
        std::fs::write(&program, loom_test_support::bash_script(
            "set -euo pipefail\n[[ \"$*\" == 'beads push' ]]\nprintf called > publication-called\nexit 17\n"
        )).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let runner = TokioRunner::default().with_publication(&program, workspace.path());
        let store = crate::bd::BdClient::with_runner(runner);
        assert!(matches!(
            store.synchronize(Synchronization::Publish).await,
            Err(Error::Uncertain { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("publication-called")).unwrap(),
            "called"
        );
    }

    /// Nonzero process exit is output, not a timeout or spawn failure.
    #[tokio::test]
    async fn bd_runner_preserves_nonzero_exit_status() {
        bounded(async {
            let runner = TokioRunner::with_clock(Arc::new(TriggerClock::default()));
            let mut command = Command::new("bash");
            command.args(["-c", "printf 'failed' >&2; exit 7"]);
            let result = runner
                .run_command(&mut command, &[], Duration::from_secs(60))
                .await
                .expect("run");
            assert_eq!(result.status, 7);
            assert_eq!(result.stderr, b"failed");
            assert!(!result.success());
        })
        .await;
    }
}

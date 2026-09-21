use std::ffi::OsString;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;

use crate::clock::{Clock, SystemClock};

use super::error::BdError;

/// Captured stdout/stderr/exit code from a single `bd` invocation.
#[derive(Debug, Clone)]
pub struct RunOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl RunOutput {
    pub const fn success(&self) -> bool {
        self.status == 0
    }
}

/// Subprocess execution boundary used by [`super::BdClient`].
///
/// Implementors run `bd <args>` (the program name is hardcoded by the
/// runner; only arguments arrive here) and surface the captured exit
/// code plus stdout/stderr. Tests substitute a capturing fake to avoid
/// a real `bd` binary; production code uses [`TokioRunner`].
pub trait CommandRunner: Send + Sync + 'static {
    fn run(
        &self,
        args: Vec<OsString>,
        timeout: Duration,
    ) -> impl Future<Output = Result<RunOutput, BdError>> + Send;
}

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
}

impl TokioRunner {
    /// Build a runner that uses `clock` for the per-call timeout.
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self { clock }
    }

    async fn run_command(
        &self,
        command: &mut Command,
        args: &[OsString],
        timeout: Duration,
    ) -> Result<RunOutput, BdError> {
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

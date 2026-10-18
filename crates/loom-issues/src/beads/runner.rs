use super::BdError;
use std::{ffi::OsString, future::Future, time::Duration};

/// Captured output from one native invocation.
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

/// Native transport; dropped futures and timeouts never promise rollback.
pub trait CommandRunner: Send + Sync + 'static {
    /// Publication is composed by the consumer; no per-task synchronization is implied.
    fn publish(&self, _timeout: Duration) -> impl Future<Output = Result<(), BdError>> + Send {
        async {
            Err(BdError::Unsupported {
                reason: "publication transport is not configured".into(),
            })
        }
    }

    fn run(
        &self,
        args: Vec<OsString>,
        timeout: Duration,
    ) -> impl Future<Output = Result<RunOutput, BdError>> + Send;
}

pub(super) fn render_args(args: &[OsString]) -> String {
    args.iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

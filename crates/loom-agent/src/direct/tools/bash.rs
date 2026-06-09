//! `Bash` — run a shell command inside the container, bounded by a
//! per-invocation timeout, returning the combined stdout/stderr capture.

use std::process::Stdio;
use std::time::Duration;

use loom_llm::{Tool, ToolOutput, tool::InvokeFuture};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command;
use tokio::time;

use super::{ToolContext, parse_args, schema_for};

/// Per-invocation timeout when the agent does not pass `timeout_ms`.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// Bash tool bound to a session context.
pub struct Bash {
    ctx: ToolContext,
}

impl Bash {
    pub fn new(ctx: ToolContext) -> Self {
        Self { ctx }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Args {
    /// Shell command line passed to `sh -c`.
    pub command: String,
    /// Per-invocation timeout in milliseconds. Defaults to 120,000.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

impl Tool for Bash {
    fn name(&self) -> &str {
        "Bash"
    }

    fn description(&self) -> &str {
        "Run `command` via `sh -c`, returning stdout, stderr, and the \
         exit status. Bounded by `timeout_ms` (default 120000 ms)."
    }

    fn input_schema(&self) -> Value {
        schema_for::<Args>()
    }

    fn invoke<'a>(&'a self, args: Value) -> InvokeFuture<'a> {
        Box::pin(async move {
            let parsed: Args = parse_args(args)?;
            run_command(parsed, self.ctx.clone()).await
        })
    }
}

async fn run_command(args: Args, ctx: ToolContext) -> Result<ToolOutput, loom_llm::LlmError> {
    let timeout = args
        .timeout_ms
        .map_or(DEFAULT_TIMEOUT, Duration::from_millis);

    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(&args.command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => return Ok(error(format!("spawn sh: {err}"))),
    };

    match time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            Ok(ToolOutput {
                content: json!({
                    "exit_code": output.status.code(),
                    "stdout": ctx.cap_or_offload("Bash", stdout)?,
                    "stderr": ctx.cap_or_offload("Bash", stderr)?,
                }),
                is_error: !output.status.success(),
            })
        }
        Ok(Err(err)) => Ok(error(format!("wait: {err}"))),
        Err(_) => Ok(error(format!("timeout after {} ms", timeout.as_millis()))),
    }
}

fn error(message: String) -> ToolOutput {
    ToolOutput {
        content: Value::String(message),
        is_error: true,
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use serde_json::json;
    use tempfile::{TempDir, tempdir};

    fn bash_with(dir: &TempDir) -> Bash {
        Bash::new(ToolContext::new(dir.path().join("offload"), usize::MAX))
    }

    fn capped_bash_with(dir: &TempDir, cap: usize) -> Bash {
        Bash::new(ToolContext::new(dir.path().join("offload"), cap))
    }

    #[tokio::test]
    async fn bash_captures_stdout_for_successful_command() {
        let dir = tempdir().unwrap();
        let out = bash_with(&dir)
            .invoke(json!({ "command": "printf hello" }))
            .await
            .expect("invoke");
        assert!(!out.is_error);
        assert_eq!(out.content["stdout"].as_str(), Some("hello"));
        assert_eq!(out.content["exit_code"].as_i64(), Some(0));
    }

    #[tokio::test]
    async fn bash_marks_nonzero_exit_as_tool_error() {
        let dir = tempdir().unwrap();
        let out = bash_with(&dir)
            .invoke(json!({ "command": "exit 7" }))
            .await
            .expect("invoke");
        assert!(out.is_error);
        assert_eq!(out.content["exit_code"].as_i64(), Some(7));
    }

    #[tokio::test]
    async fn bash_timeout_kills_long_running_command() {
        let dir = tempdir().unwrap();
        let out = bash_with(&dir)
            .invoke(json!({ "command": "sleep 5", "timeout_ms": 50 }))
            .await
            .expect("invoke");
        assert!(out.is_error);
        let msg = out.content.as_str().unwrap();
        assert!(msg.contains("timeout"), "{msg}");
    }

    #[tokio::test]
    async fn bash_captures_stderr_separately_from_stdout() {
        let dir = tempdir().unwrap();
        let out = bash_with(&dir)
            .invoke(json!({ "command": "printf err 1>&2" }))
            .await
            .expect("invoke");
        assert_eq!(out.content["stderr"].as_str(), Some("err"));
        assert_eq!(out.content["stdout"].as_str(), Some(""));
    }

    #[tokio::test]
    async fn bash_applies_inline_cap_to_stdout() {
        let dir = tempdir().unwrap();
        let out = capped_bash_with(&dir, 5)
            .invoke(json!({ "command": "printf 'alpha beta'" }))
            .await
            .expect("invoke");

        assert!(!out.is_error);
        assert_eq!(out.content["stdout"]["offloaded"], json!(true));
        assert_eq!(out.content["stdout"]["total_bytes"], json!(10));
        assert_eq!(out.content["stderr"].as_str(), Some(""));
        let path = out.content["stdout"]["path"].as_str().expect("stdout path");
        assert_eq!(std::fs::read_to_string(path).unwrap(), "alpha beta");
    }

    #[tokio::test]
    async fn bash_applies_inline_cap_to_stderr() {
        let dir = tempdir().unwrap();
        let out = capped_bash_with(&dir, 5)
            .invoke(json!({ "command": "printf 'alpha beta' 1>&2" }))
            .await
            .expect("invoke");

        assert!(!out.is_error);
        assert_eq!(out.content["stderr"]["offloaded"], json!(true));
        assert_eq!(out.content["stderr"]["total_bytes"], json!(10));
        assert_eq!(out.content["stdout"].as_str(), Some(""));
        let path = out.content["stderr"]["path"].as_str().expect("stderr path");
        assert_eq!(std::fs::read_to_string(path).unwrap(), "alpha beta");
    }
}

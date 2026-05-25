//! `Read` — read a workspace file into a string with optional line slice.
//!
//! Errors as a tool-result (not an [`LlmError`](loom_llm::LlmError)) on
//! binary files or IO failures, so the agent can adjust its plan
//! without aborting the conversation loop.

use std::path::PathBuf;

use loom_llm::{Tool, ToolOutput, tool::InvokeFuture};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use tokio::fs;

use super::{parse_args, schema_for};

/// Heuristic threshold for binary detection: bytes scanned from the
/// start of the file for NUL (0x00). The same value `git diff` uses
/// for its binary-file heuristic; large enough to catch text-with-NULs
/// such as locale-encoded `.mo` files, small enough to keep the read
/// bounded on huge binaries.
const BINARY_SCAN_BYTES: usize = 8 * 1024;

/// Zero-sized Read tool. State-free; one instance services every call
/// the conversation loop dispatches.
pub struct Read;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Args {
    /// Absolute or workspace-relative path to read.
    pub file_path: PathBuf,
    /// One-indexed first line to include in the returned slice.
    #[serde(default)]
    pub offset: Option<usize>,
    /// Maximum number of lines to return from `offset`.
    #[serde(default)]
    pub limit: Option<usize>,
}

impl Tool for Read {
    fn name(&self) -> &str {
        "Read"
    }

    fn description(&self) -> &str {
        "Read a workspace file. Optional 1-indexed `offset` and `limit` \
         slice the content by line. Errors on binary files."
    }

    fn input_schema(&self) -> Value {
        schema_for::<Args>()
    }

    fn invoke<'a>(&'a self, args: Value) -> InvokeFuture<'a> {
        Box::pin(async move {
            let parsed: Args = parse_args(args)?;
            Ok(read_file(parsed).await)
        })
    }
}

async fn read_file(args: Args) -> ToolOutput {
    let bytes = match fs::read(&args.file_path).await {
        Ok(bytes) => bytes,
        Err(err) => return error(format!("read {}: {err}", args.file_path.display())),
    };

    if is_binary(&bytes) {
        return error(format!(
            "binary file rejected: {}",
            args.file_path.display()
        ));
    }

    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => return error(format!("invalid utf-8: {}", args.file_path.display())),
    };

    let sliced = slice_lines(&text, args.offset, args.limit);
    ToolOutput {
        content: Value::String(sliced),
        is_error: false,
    }
}

fn is_binary(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(BINARY_SCAN_BYTES)];
    head.contains(&0)
}

fn slice_lines(text: &str, offset: Option<usize>, limit: Option<usize>) -> String {
    if offset.is_none() && limit.is_none() {
        return text.to_string();
    }
    let start = offset.unwrap_or(1).saturating_sub(1);
    let take = limit.unwrap_or(usize::MAX);
    text.lines()
        .skip(start)
        .take(take)
        .collect::<Vec<_>>()
        .join("\n")
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
    use tempfile::tempdir;

    #[tokio::test]
    async fn read_returns_full_content_when_no_slice() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        fs::write(&path, "alpha\nbeta\ngamma").await.unwrap();

        let out = Read
            .invoke(json!({ "file_path": path }))
            .await
            .expect("invoke");
        assert!(!out.is_error);
        assert_eq!(out.content, Value::String("alpha\nbeta\ngamma".into()));
    }

    #[tokio::test]
    async fn read_applies_offset_and_limit_as_line_slice() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("multi.txt");
        fs::write(&path, "one\ntwo\nthree\nfour\nfive")
            .await
            .unwrap();

        let out = Read
            .invoke(json!({ "file_path": path, "offset": 2, "limit": 2 }))
            .await
            .expect("invoke");
        assert!(!out.is_error);
        assert_eq!(out.content, Value::String("two\nthree".into()));
    }

    #[tokio::test]
    async fn read_rejects_binary_file_as_tool_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bin.dat");
        fs::write(&path, b"hello\x00world").await.unwrap();

        let out = Read
            .invoke(json!({ "file_path": path }))
            .await
            .expect("invoke");
        assert!(out.is_error);
        let msg = out.content.as_str().unwrap();
        assert!(msg.contains("binary"), "{msg}");
    }

    #[tokio::test]
    async fn read_missing_file_returns_tool_error_not_protocol_error() {
        let out = Read
            .invoke(json!({ "file_path": "/nonexistent/path/x" }))
            .await
            .expect("invoke");
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn read_input_schema_describes_file_path_required() {
        let schema = Read.input_schema();
        let required = schema["required"]
            .as_array()
            .expect("required array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>();
        assert!(required.contains(&"file_path"), "schema: {schema}");
    }

    /// Spec contract (`specs/agent.md` § Direct backend, L761–762):
    /// inside the container the workspace is bind-mounted at
    /// `/workspace/...`, and Direct tools resolve absolute paths through
    /// the same kernel filesystem APIs as on the host. This test stands
    /// in for the container by rooting a fake workspace mount under a
    /// tempdir and confirming that an absolute path nested under it —
    /// the same path-shape `/workspace/<dir>/<file>` the agent receives
    /// in production — round-trips through the Read tool. A regression
    /// that introduces sandbox-side path translation, prefix stripping,
    /// or sandbox-internal virtual filesystems would silently change the
    /// contract; this test trips on it.
    #[tokio::test]
    async fn direct_tools_read_against_container_workspace_mount() {
        let workspace_mount = tempfile::tempdir().expect("workspace mount tempdir");
        let nested = workspace_mount.path().join("crates/loom-agent/src");
        fs::create_dir_all(&nested)
            .await
            .expect("create nested dir");
        let target = nested.join("lib.rs");
        let body = "//! workspace-mount probe\npub fn hello() {}\n";
        fs::write(&target, body).await.expect("write fixture");

        assert!(
            target.is_absolute(),
            "test must exercise the absolute-path contract; got={}",
            target.display(),
        );

        let out = Read
            .invoke(json!({ "file_path": target }))
            .await
            .expect("invoke");
        assert!(
            !out.is_error,
            "Read against the workspace-mount file must succeed; got={out:?}",
        );
        assert_eq!(
            out.content,
            Value::String(body.to_string()),
            "Read must return the bytes the kernel resolved at the absolute path",
        );
    }
}

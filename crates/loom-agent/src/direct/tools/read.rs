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

use super::{ToolContext, parse_args, schema_for};

/// Heuristic threshold for binary detection: bytes scanned from the
/// start of the file for NUL (0x00). The same value `git diff` uses
/// for its binary-file heuristic; large enough to catch text-with-NULs
/// such as locale-encoded `.mo` files, small enough to keep the read
/// bounded on huge binaries.
const BINARY_SCAN_BYTES: usize = 8 * 1024;

/// Read tool bound to a session context.
pub struct Read {
    ctx: ToolContext,
}

impl Read {
    pub fn new(ctx: ToolContext) -> Self {
        Self { ctx }
    }
}

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
            read_file(parsed, self.ctx.clone()).await
        })
    }
}

async fn read_file(args: Args, ctx: ToolContext) -> Result<ToolOutput, loom_llm::LlmError> {
    let bytes = match fs::read(&args.file_path).await {
        Ok(bytes) => bytes,
        Err(err) => return Ok(error(format!("read {}: {err}", args.file_path.display()))),
    };

    if is_binary(&bytes) {
        return Ok(error(format!(
            "binary file rejected: {}",
            args.file_path.display()
        )));
    }

    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => {
            return Ok(error(format!(
                "invalid utf-8: {}",
                args.file_path.display()
            )));
        }
    };

    let sliced = slice_lines(&text, args.offset, args.limit);
    Ok(ToolOutput {
        content: ctx.cap_or_offload("Read", sliced)?,
        is_error: false,
    })
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
    use tempfile::{TempDir, tempdir};

    fn read_with(dir: &TempDir, cap: usize) -> Read {
        Read::new(ToolContext::new(dir.path().join("offload"), cap))
    }

    #[tokio::test]
    async fn read_returns_full_content_when_no_slice() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        fs::write(&path, "alpha\nbeta\ngamma").await.unwrap();

        let out = read_with(&dir, usize::MAX)
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

        let out = read_with(&dir, usize::MAX)
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

        let out = read_with(&dir, usize::MAX)
            .invoke(json!({ "file_path": path }))
            .await
            .expect("invoke");
        assert!(out.is_error);
        let msg = out.content.as_str().unwrap();
        assert!(msg.contains("binary"), "{msg}");
    }

    #[tokio::test]
    async fn read_missing_file_returns_tool_error_not_protocol_error() {
        let dir = tempdir().unwrap();
        let out = read_with(&dir, usize::MAX)
            .invoke(json!({ "file_path": "/nonexistent/path/x" }))
            .await
            .expect("invoke");
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn read_input_schema_describes_file_path_required() {
        let dir = tempdir().unwrap();
        let schema = read_with(&dir, usize::MAX).input_schema();
        let required = schema["required"]
            .as_array()
            .expect("required array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>();
        assert!(required.contains(&"file_path"), "schema: {schema}");
    }

    #[tokio::test]
    async fn read_over_cap_offloads_full_payload_and_returns_head_reference() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("large.txt");
        let body = "alpha\nbeta\ngamma\ndelta\n";
        fs::write(&path, body).await.unwrap();

        let out = read_with(&dir, "alpha\nbeta\n".len())
            .invoke(json!({ "file_path": path }))
            .await
            .expect("invoke");

        assert!(!out.is_error);
        assert_eq!(out.content["offloaded"], json!(true));
        assert_eq!(out.content["total_bytes"], json!(body.len()));
        assert_eq!(out.content["total_lines"], json!(4));
        assert_eq!(out.content["head_lines"], json!(2));
        let head = out.content["head"].as_str().expect("head string");
        assert!(head.starts_with("alpha\nbeta\n"), "{head}");
        assert!(head.contains("[truncated:"), "{head}");
        let offload_path = out.content["path"].as_str().expect("offload path");
        assert_eq!(fs::read_to_string(offload_path).await.unwrap(), body);
    }

    #[tokio::test]
    async fn cap_measured_on_raw_utf8_byte_length_not_serialized() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("quoted.txt");
        fs::write(&path, "\"").await.unwrap();

        let out = read_with(&dir, 1)
            .invoke(json!({ "file_path": path }))
            .await
            .expect("invoke");

        assert!(!out.is_error);
        assert_eq!(out.content, Value::String("\"".to_string()));
    }

    #[tokio::test]
    async fn offloaded_file_round_trips_through_read_via_head_lines_offset() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("source.txt");
        let body = "one\ntwo\nthree\nfour";
        fs::write(&path, body).await.unwrap();

        let out = read_with(&dir, "one\ntwo\n".len())
            .invoke(json!({ "file_path": path }))
            .await
            .expect("invoke");
        let offload_path = out.content["path"].as_str().expect("offload path");
        let head_lines = usize::try_from(out.content["head_lines"].as_u64().expect("head_lines"))
            .expect("head_lines fits usize");

        let tail = read_with(&dir, usize::MAX)
            .invoke(json!({ "file_path": offload_path, "offset": head_lines + 1 }))
            .await
            .expect("tail read");
        assert!(!tail.is_error);
        let prefix = body.lines().take(head_lines).collect::<Vec<_>>().join("\n");
        let reconstructed = format!("{prefix}\n{}", tail.content.as_str().unwrap());
        assert_eq!(reconstructed, body);
    }

    #[tokio::test]
    async fn distinct_content_offloads_to_distinct_deterministic_paths() {
        let dir = tempdir().unwrap();
        let first = dir.path().join("first.txt");
        let second = dir.path().join("second.txt");
        fs::write(&first, "aaaa\nbbbb\n").await.unwrap();
        fs::write(&second, "aaaa\ncccc\n").await.unwrap();

        let first_out = read_with(&dir, 5)
            .invoke(json!({ "file_path": first }))
            .await
            .expect("first read");
        let second_out = read_with(&dir, 5)
            .invoke(json!({ "file_path": second }))
            .await
            .expect("second read");
        let first_again = read_with(&dir, 5)
            .invoke(json!({ "file_path": dir.path().join("first.txt") }))
            .await
            .expect("first reread");

        let first_path = first_out.content["path"].as_str().expect("first path");
        let second_path = second_out.content["path"].as_str().expect("second path");
        assert_ne!(first_path, second_path);
        assert_eq!(
            first_path,
            first_again.content["path"].as_str().expect("repeat path"),
        );
        assert!(first_path.ends_with(".txt"), "{first_path}");
    }

    #[tokio::test]
    async fn offload_write_failure_degrades_to_inline_truncation() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("large.txt");
        let blocker = dir.path().join("blocker");
        fs::write(&source, "alpha\nbeta\ngamma\n").await.unwrap();
        fs::write(&blocker, "not a directory").await.unwrap();
        let ctx = ToolContext::new(blocker.join("offload"), "alpha\n".len());

        let out = Read::new(ctx)
            .invoke(json!({ "file_path": source }))
            .await
            .expect("invoke");

        assert!(!out.is_error);
        let text = out.content.as_str().expect("path-less truncation string");
        assert!(text.starts_with("alpha\n"), "{text}");
        assert!(text.contains("[truncated: showing 1 of 3 lines]"), "{text}");
    }

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

        let out = read_with(&workspace_mount, usize::MAX)
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

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
    pub const fn new(ctx: ToolContext) -> Self {
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
    /// Zero-indexed UTF-8 byte boundary; cannot be combined with line slicing.
    #[serde(default)]
    pub byte_offset: Option<usize>,
}

impl Tool for Read {
    fn name(&self) -> &'static str {
        "Read"
    }

    fn description(&self) -> &'static str {
        "Read a workspace file. Optional 1-indexed `offset` and `limit` \
         slice the content by line without changing line endings. Use `byte_offset` \
         alone to continue an offloaded partial line. Errors on binary files."
    }

    fn input_schema(&self) -> Value {
        schema_for::<Args>()
    }

    fn invoke(&self, args: Value) -> InvokeFuture<'_> {
        Box::pin(async move {
            let parsed: Args = parse_args(args)?;
            read_file(parsed, self.ctx.clone()).await
        })
    }
}

async fn read_file(args: Args, ctx: ToolContext) -> Result<ToolOutput, loom_llm::LlmError> {
    let display_path = args.file_path.display().to_string();
    let path = ctx.resolve_workspace_path(&args.file_path);
    let bytes = match fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(err) => return Ok(error(format!("read {display_path}: {err}"))),
    };

    if is_binary(&bytes) && !ctx.is_offload_file(&path, &bytes) {
        return Ok(error(format!("binary file rejected: {display_path}")));
    }

    let Ok(text) = String::from_utf8(bytes) else {
        return Ok(error(format!("invalid utf-8: {display_path}")));
    };

    let content = if let Some(byte_offset) = args.byte_offset {
        if args.offset.is_some() || args.limit.is_some() {
            return Ok(error(
                "byte_offset cannot be combined with offset or limit".into(),
            ));
        }
        if text.get(byte_offset..).is_none() {
            return Ok(error(
                "byte_offset must be a UTF-8 boundary within the file".into(),
            ));
        }
        ctx.cap_or_offload_at("Read", &text, byte_offset)?
    } else {
        let sliced = slice_lines(&text, args.offset, args.limit);
        ctx.cap_or_offload("Read", &sliced)?
    };
    Ok(ToolOutput {
        content,
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
    text.split_inclusive('\n').skip(start).take(take).collect()
}

const fn error(message: String) -> ToolOutput {
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
        Read::new(ToolContext::new(
            dir.path().join("offload"),
            cap.try_into().unwrap(),
        ))
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
        assert_eq!(out.content, Value::String("two\nthree\n".into()));
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
    async fn read_only_accepts_nul_in_verified_offload_files() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().join("offload"), 4.try_into().unwrap());
        let reference = ctx.cap_or_offload("Bash", "abc\0def\0ghi").unwrap();
        let path = reference["path"].as_str().unwrap();
        fs::write(path, "changed\0binary").await.unwrap();
        let output = Read::new(ctx)
            .invoke(json!({"file_path": path}))
            .await
            .unwrap();
        assert!(
            output.is_error,
            "a filename alone cannot bypass binary detection"
        );
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
        let mut required = schema["required"]
            .as_array()
            .expect("required array")
            .iter()
            .filter_map(|v| v.as_str());
        assert!(
            required.any(|field| field == "file_path"),
            "schema: {schema}"
        );
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
        fs::write(&path, "\"\"\"\"").await.unwrap();

        let out = read_with(&dir, 4)
            .invoke(json!({ "file_path": path }))
            .await
            .expect("invoke");

        assert!(!out.is_error);
        assert_eq!(out.content, Value::String("\"\"\"\"".to_string()));
    }

    #[tokio::test]
    async fn offloaded_file_round_trips_through_read_via_head_lines_offset() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("source.txt");
        let body = "one\ntwo\nthree\nfour";
        fs::write(&path, body).await.unwrap();

        let tool = read_with(&dir, 8);
        let mut output = tool.invoke(json!({"file_path": path})).await.unwrap();
        let mut recovered = String::new();
        let mut chunks = 0;
        while output.content["offloaded"] == true {
            assert!(!output.is_error);
            let bytes = usize::try_from(output.content["head_bytes"].as_u64().unwrap()).unwrap();
            let head = &output.content["head"].as_str().unwrap()[..bytes];
            assert!(head.ends_with('\n'));
            assert!(bytes <= 8);
            recovered.push_str(head);
            let lines = output.content["head_lines"].as_u64().unwrap();
            output = tool
                .invoke(json!({
                    "file_path": output.content["path"], "offset": lines + 1,
                }))
                .await
                .unwrap();
            chunks += 1;
            assert!(chunks < body.len());
        }
        recovered.push_str(output.content.as_str().unwrap());
        assert!(chunks >= 2);
        assert_eq!(recovered, body);
    }

    async fn recover_with_byte_cursor(body: &str, cap: usize) {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().join("offload"), cap.try_into().unwrap());
        let mut output = ToolOutput {
            content: ctx.cap_or_offload("Bash", body).unwrap(),
            is_error: false,
        };
        let tool = Read::new(ctx);
        let mut recovered = String::new();
        let mut cursor = 0;
        let mut offload_path = None;
        loop {
            assert!(!output.is_error);
            if let Some(tail) = output.content.as_str() {
                assert!(tail.len() <= cap);
                recovered.push_str(tail);
                break;
            }
            assert_eq!(output.content["offloaded"], true);
            let bytes = usize::try_from(output.content["head_bytes"].as_u64().unwrap()).unwrap();
            assert!(bytes > 0 && bytes <= cap);
            let head = output.content["head"].as_str().unwrap();
            recovered.push_str(&head[..bytes]);
            let next =
                usize::try_from(output.content["next_byte_offset"].as_u64().unwrap()).unwrap();
            assert_eq!(next, cursor + bytes);
            let path = output.content["path"].as_str().unwrap().to_owned();
            if let Some(previous) = &offload_path {
                assert_eq!(&path, previous);
            }
            assert_eq!(fs::read(&path).await.unwrap(), body.as_bytes());
            offload_path = Some(path.clone());
            cursor = next;
            output = tool
                .invoke(json!({"file_path": path, "byte_offset": next}))
                .await
                .unwrap();
        }
        assert_eq!(recovered.as_bytes(), body.as_bytes());
    }

    #[tokio::test]
    async fn byte_cursor_recovers_oversized_lines_and_exact_line_endings() {
        for body in [
            "😀😀😀😀",
            "abcdefgh",
            "aa\r\nbb\r\ncc\r\n",
            "a\n\nb\r\n",
            "abcd\nefgh\n",
            "終わりなし",
            "abc\0def\0ghi\0jkl",
        ] {
            for cap in 4..=9 {
                recover_with_byte_cursor(body, cap).await;
            }
        }
    }

    proptest::proptest! {
        #![proptest_config(loom_test_support::proptest_config())]
        #[test]
        fn capped_byte_recovery_is_lossless(
            chars in proptest::collection::vec(
                proptest::prop_oneof![
                    proptest::char::range('\u{0}', '\u{10ffff}'),
                    proptest::strategy::Just('\n'),
                    proptest::strategy::Just('\r'),
                    proptest::strategy::Just('\0'),
                ], 0..160),
            cap in 4usize..40,
        ) {
            let body: String = chars.into_iter().collect();
            tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
                .block_on(recover_with_byte_cursor(&body, cap));
        }
    }

    #[tokio::test]
    async fn read_rejects_invalid_or_ambiguous_byte_cursor() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("source.txt");
        fs::write(&path, "😀text").await.unwrap();
        for args in [
            json!({"file_path": path, "byte_offset": 1}),
            json!({"file_path": path, "byte_offset": 9}),
            json!({"file_path": path, "byte_offset": 0, "offset": 1}),
            json!({"file_path": path, "byte_offset": 0, "limit": 1}),
        ] {
            assert!(read_with(&dir, 4).invoke(args).await.unwrap().is_error);
        }
        assert_eq!(
            read_with(&dir, 4)
                .invoke(json!({"file_path": path, "byte_offset": 8}))
                .await
                .unwrap()
                .content,
            ""
        );
    }

    #[tokio::test]
    async fn line_slices_preserve_crlf_and_final_newline() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("source.txt");
        fs::write(&path, "aa\r\nbb\r\ncc\r\n").await.unwrap();
        let output = read_with(&dir, 8)
            .invoke(json!({"file_path": path, "offset": 2, "limit": 2}))
            .await
            .unwrap();
        assert_eq!(output.content, "bb\r\ncc\r\n");
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
        assert_eq!(
            std::path::Path::new(first_path).extension(),
            Some(std::ffi::OsStr::new("txt")),
            "{first_path}"
        );
    }

    #[tokio::test]
    async fn offload_write_failure_degrades_to_inline_truncation() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("large.txt");
        let blocker = dir.path().join("blocker");
        fs::write(&source, "alpha\nbeta\ngamma\n").await.unwrap();
        fs::write(&blocker, "not a directory").await.unwrap();
        let ctx = ToolContext::new(blocker.join("offload"), "alpha\n".len().try_into().unwrap());

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
    async fn read_maps_container_paths_to_explicit_workspace_root() {
        let workspace_mount = tempfile::tempdir().expect("workspace mount tempdir");
        let nested = workspace_mount.path().join("crates/loom-agent/src");
        fs::create_dir_all(&nested)
            .await
            .expect("create nested dir");
        let target = nested.join("lib.rs");
        let body = "//! workspace-mount probe\npub fn hello() {}\n";
        fs::write(&target, body).await.expect("write fixture");
        let container_path = PathBuf::from("/workspace/crates/loom-agent/src/lib.rs");
        let tool = Read::new(ToolContext::with_workspace_root(
            workspace_mount.path().join("offload"),
            usize::MAX.try_into().unwrap(),
            workspace_mount.path().to_path_buf(),
        ));

        assert!(
            container_path.starts_with("/workspace"),
            "test must exercise the agent-visible workspace mount path",
        );

        let out = tool
            .invoke(json!({ "file_path": container_path }))
            .await
            .expect("invoke");
        assert!(
            !out.is_error,
            "Read against the workspace-mount file must succeed; got={out:?}",
        );
        assert_eq!(
            out.content,
            Value::String(body.to_string()),
            "Read must return the bytes resolved through the /workspace mount",
        );
    }
}

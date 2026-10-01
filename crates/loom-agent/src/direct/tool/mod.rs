//! Sandbox-aware tool implementations registered with the in-process
//! `Conversation` by `loom-direct-runner`.
//!
//! Six net-new tools — [`Read`], [`Write`], [`Edit`], [`Bash`], [`Grep`],
//! [`Glob`] — each implementing the
//! [`Tool`](loom_llm::Tool) trait and executing against the workspace
//! bind-mount inside the container. See `specs/agent.md`
//! § Direct Backend — *The six tools*.

pub mod bash;
pub mod edit;
pub mod glob;
pub mod grep;
pub mod read;
pub mod write;

pub use bash::Bash;
pub use edit::Edit;
pub use glob::Glob;
pub use grep::Grep;
pub use read::Read;
pub use write::Write;

use std::fs::{self, OpenOptions};
use std::io::{self, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use displaydoc::Display;
use loom_driver::config::InlineByteLimit;
use loom_llm::LlmError;
use schemars::{JsonSchema, SchemaGenerator};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use thiserror::Error;

/// Per-session capabilities available to Direct tool handlers.
#[derive(Clone)]
pub struct ToolContext {
    capabilities: Arc<Capabilities>,
}

struct Capabilities {
    offload: OffloadSink,
    records: Mutex<Vec<OffloadRecord>>,
    workspace: WorkspaceMount,
}

struct OffloadSink {
    dir: PathBuf,
    max_inline_bytes: InlineByteLimit,
}

struct WorkspaceMount {
    container_root: PathBuf,
    host_root: PathBuf,
}

struct Head {
    content: String,
    lines: usize,
}

struct CapOutcome {
    value: Value,
    total_bytes: Option<usize>,
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

const CONTAINER_WORKSPACE: &str = "/workspace";

/// Successful Direct tool output offload recorded at the cap point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OffloadRecord {
    pub tool: String,
    pub total_bytes: usize,
}

impl ToolContext {
    /// Create a Direct tool context rooted at the session's offload directory.
    pub fn new(offload_dir: PathBuf, max_inline_bytes: InlineByteLimit) -> Self {
        Self::with_workspace_root(
            offload_dir,
            max_inline_bytes,
            PathBuf::from(CONTAINER_WORKSPACE),
        )
    }

    /// Create a context whose `/workspace` paths resolve under `workspace_root`.
    pub fn with_workspace_root(
        offload_dir: PathBuf,
        max_inline_bytes: InlineByteLimit,
        workspace_root: PathBuf,
    ) -> Self {
        Self {
            capabilities: Arc::new(Capabilities {
                offload: OffloadSink {
                    dir: offload_dir,
                    max_inline_bytes,
                },
                records: Mutex::new(Vec::new()),
                workspace: WorkspaceMount {
                    container_root: PathBuf::from(CONTAINER_WORKSPACE),
                    host_root: workspace_root,
                },
            }),
        }
    }

    /// Resolve an agent-visible path through the container workspace mount.
    pub fn resolve_workspace_path(&self, path: &Path) -> PathBuf {
        self.capabilities.workspace.resolve(path)
    }

    pub(super) fn is_offload_file(&self, path: &Path, content: &[u8]) -> bool {
        path == self.capabilities.offload.path_for(content)
    }

    /// Return `content` inline when it fits, otherwise offload the full payload.
    ///
    /// # Errors
    ///
    /// Returns an error when agent startup, protocol handling, or tool execution fails.
    pub fn cap_or_offload(&self, tool: &str, content: &str) -> Result<Value, ToolContextError> {
        self.cap_or_offload_at(tool, content, 0)
    }

    pub(super) fn cap_or_offload_at(
        &self,
        tool: &str,
        content: &str,
        byte_offset: usize,
    ) -> Result<Value, ToolContextError> {
        let outcome = self
            .capabilities
            .offload
            .cap_or_offload(content, byte_offset)?;
        if let Some(total_bytes) = outcome.total_bytes {
            self.capabilities
                .records
                .lock()
                .map_err(|_| ToolContextError::OffloadRecordLockPoisoned)?
                .push(OffloadRecord {
                    tool: tool.to_string(),
                    total_bytes,
                });
        }
        Ok(outcome.value)
    }

    /// Drain successful offloads recorded since the prior drain.
    ///
    /// # Errors
    ///
    /// Returns an error when agent startup, protocol handling, or tool execution fails.
    pub fn drain_offloads(&self) -> Result<Vec<OffloadRecord>, ToolContextError> {
        let mut records = self
            .capabilities
            .records
            .lock()
            .map_err(|_| ToolContextError::OffloadRecordLockPoisoned)?;
        Ok(std::mem::take(&mut *records))
    }
}

/// Errors raised by the Direct tool session context.
#[derive(Debug, Display, Error)]
pub enum ToolContextError {
    /// Direct offload record lock poisoned
    OffloadRecordLockPoisoned,
    /// Read byte offset must be a UTF-8 boundary within the file
    InvalidByteOffset,
}

impl From<ToolContextError> for LlmError {
    fn from(err: ToolContextError) -> Self {
        Self::Provider {
            message: err.to_string(),
        }
    }
}

impl WorkspaceMount {
    fn resolve(&self, path: &Path) -> PathBuf {
        match path.strip_prefix(&self.container_root) {
            Ok(rel) => self.host_root.join(rel),
            Err(_) => path.to_path_buf(),
        }
    }
}

impl OffloadSink {
    fn cap_or_offload(
        &self,
        content: &str,
        byte_offset: usize,
    ) -> Result<CapOutcome, ToolContextError> {
        let remaining = content
            .get(byte_offset..)
            .ok_or(ToolContextError::InvalidByteOffset)?;
        let total_bytes = content.len();
        if remaining.len() <= self.max_inline_bytes.get() {
            return Ok(CapOutcome {
                value: Value::String(remaining.to_string()),
                total_bytes: None,
            });
        }

        let total_lines = content.lines().count();
        let head = head_within_cap(remaining, self.max_inline_bytes.get());
        match self.write(content) {
            Ok(path) => {
                let path = path.display().to_string();
                let head_lines = head.lines;
                let head_bytes = head.content.len();
                let next_byte_offset = byte_offset + head_bytes;
                let head = append_marker(
                    head.content,
                    &format!(
                        "[truncated: showing {head_bytes} bytes; full output at {path}; Read with byte_offset {next_byte_offset} to continue]",
                    ),
                );
                Ok(CapOutcome {
                    value: json!({
                        "offloaded": true,
                        "path": path,
                        "total_bytes": total_bytes,
                        "total_lines": total_lines,
                        "head_lines": head_lines,
                        "head_bytes": head_bytes,
                        "next_byte_offset": next_byte_offset,
                        "head": head,
                    }),
                    total_bytes: Some(total_bytes),
                })
            }
            Err(err) => {
                tracing::warn!(tool_output_bytes = total_bytes, error = ?err, "offload failed; returning inline truncation");
                Ok(CapOutcome {
                    value: Value::String(append_marker(
                        head.content,
                        &format!("[truncated: showing {} of {total_lines} lines]", head.lines),
                    )),
                    total_bytes: None,
                })
            }
        }
    }

    fn path_for(&self, content: &[u8]) -> PathBuf {
        self.dir
            .join(format!("{}.txt", blake3::hash(content).to_hex()))
    }

    fn write(&self, content: &str) -> io::Result<PathBuf> {
        fs::create_dir_all(&self.dir)?;
        let path = self.path_for(content.as_bytes());
        if path.is_file() {
            return Ok(path);
        }
        let tmp = write_unique_temp(&path, content)?;
        if path.is_file() {
            fs::remove_file(&tmp)?;
            return Ok(path);
        }
        match fs::rename(&tmp, &path) {
            Ok(()) => Ok(path),
            Err(_err) if path.is_file() => {
                fs::remove_file(&tmp)?;
                Ok(path)
            }
            Err(err) => Err(err),
        }
    }
}

fn write_unique_temp(path: &Path, content: &str) -> io::Result<PathBuf> {
    loop {
        let tmp = path.with_extension(format!(
            "{}.{}.tmp",
            process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(mut file) => {
                file.write_all(content.as_bytes())?;
                return Ok(tmp);
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
}

fn head_within_cap(content: &str, cap: usize) -> Head {
    let mut bytes = 0;
    let mut lines = 0;
    for line in content.split_inclusive('\n') {
        let next = bytes + line.len();
        if next <= cap {
            bytes = next;
            lines += 1;
            continue;
        }

        break;
    }

    if lines == 0 {
        Head {
            content: char_prefix_within_bytes(content, cap).to_string(),
            lines,
        }
    } else {
        Head {
            content: content[..bytes].to_string(),
            lines,
        }
    }
}

fn char_prefix_within_bytes(content: &str, cap: usize) -> &str {
    if cap >= content.len() {
        return content;
    }

    let mut end = 0;
    for (idx, ch) in content.char_indices() {
        let next = idx + ch.len_utf8();
        if next > cap {
            break;
        }
        end = next;
    }
    &content[..end]
}

fn append_marker(mut head: String, marker: &str) -> String {
    if !head.is_empty() && !head.ends_with('\n') {
        head.push('\n');
    }
    head.push_str(marker);
    head
}

/// Generate a JSON-Schema value for the tool's argument struct. Each
/// tool's [`Tool::input_schema`](loom_llm::Tool::input_schema) calls
/// this with its own `Args` type so the model sees a typed surface.
fn schema_for<T: JsonSchema>() -> Value {
    SchemaGenerator::default()
        .into_root_schema_for::<T>()
        .to_value()
}

/// Decode the model-supplied `args` payload into the tool's typed
/// argument struct. Returns [`LlmError::MalformedJson`] on a shape
/// mismatch so the caller surfaces a typed protocol error rather than
/// a tool-result.
fn parse_args<T: DeserializeOwned>(args: Value) -> Result<T, LlmError> {
    serde_json::from_value(args).map_err(|err| LlmError::MalformedJson(err.to_string()))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;

    use loom_llm::Tool;

    use super::*;

    #[tokio::test]
    async fn grep_and_glob_offload_string_output_over_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let grep_source = dir.path().join("grep-source.txt");
        let glob_source = dir.path().join("glob-result-with-long-name.data");
        std::fs::write(&grep_source, "needle with long payload\nignored\n")
            .expect("write grep fixture");
        std::fs::write(&glob_source, "").expect("write glob fixture");
        let ctx = ToolContext::new(dir.path().join("offload"), 8.try_into().unwrap());

        let grep_output = Grep::new(ctx.clone())
            .invoke(json!({ "pattern": "needle", "path": grep_source }))
            .await
            .expect("invoke Grep");
        let expected_grep = format!("{}:1:needle with long payload", grep_source.display());
        assert!(!grep_output.is_error);
        assert_eq!(grep_output.content["offloaded"], json!(true));
        assert_eq!(
            grep_output.content["total_bytes"],
            json!(expected_grep.len())
        );
        let grep_path = grep_output.content["path"]
            .as_str()
            .expect("Grep offload path");
        assert_eq!(
            std::fs::read_to_string(grep_path).expect("read Grep offload"),
            expected_grep
        );

        let glob_output = Glob::new(ctx)
            .invoke(json!({
                "pattern": "glob-result-*.data",
                "path": dir.path(),
            }))
            .await
            .expect("invoke Glob");
        let expected_glob = glob_source.display().to_string();
        assert!(!glob_output.is_error);
        assert_eq!(glob_output.content["offloaded"], json!(true));
        assert_eq!(
            glob_output.content["total_bytes"],
            json!(expected_glob.len())
        );
        let glob_path = glob_output.content["path"]
            .as_str()
            .expect("Glob offload path");
        assert_eq!(
            std::fs::read_to_string(glob_path).expect("read Glob offload"),
            expected_glob
        );
    }

    #[test]
    fn same_content_concurrent_offloads_converge_to_one_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(dir.path().join("offload"), 4.try_into().unwrap());
        let body = "alpha\nbeta\ngamma\n".to_string();
        let barrier = Arc::new(Barrier::new(16));
        let mut handles = Vec::with_capacity(16);
        for _ in 0..16 {
            let ctx = ctx.clone();
            let body = body.clone();
            let barrier = barrier.clone();
            handles.push(thread::spawn(move || {
                barrier.wait();
                ctx.cap_or_offload("Read", &body).expect("offload")
            }));
        }

        let outputs = handles
            .into_iter()
            .map(|handle| handle.join().expect("thread joins"))
            .collect::<Vec<_>>();
        let first_path = outputs[0]["path"].as_str().expect("path").to_string();
        for output in &outputs {
            assert_eq!(output["offloaded"], json!(true));
            assert_eq!(output["path"], json!(first_path));
        }
        assert_eq!(
            std::fs::read_to_string(first_path).expect("offload file"),
            body
        );
    }
}

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

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use loom_llm::LlmError;
use schemars::{JsonSchema, SchemaGenerator};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// Per-session capabilities available to Direct tool handlers.
#[derive(Clone)]
pub struct ToolContext {
    capabilities: Arc<Capabilities>,
}

struct Capabilities {
    offload: OffloadSink,
    records: Mutex<Vec<OffloadRecord>>,
}

struct OffloadSink {
    dir: PathBuf,
    max_inline_bytes: usize,
}

struct Head {
    content: String,
    lines: usize,
}

struct CapOutcome {
    value: Value,
    total_bytes: Option<usize>,
}

/// Successful Direct tool output offload recorded at the cap point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OffloadRecord {
    pub tool: String,
    pub total_bytes: usize,
}

impl ToolContext {
    /// Create a Direct tool context rooted at the session's offload directory.
    pub fn new(offload_dir: PathBuf, max_inline_bytes: usize) -> Self {
        Self {
            capabilities: Arc::new(Capabilities {
                offload: OffloadSink {
                    dir: offload_dir,
                    max_inline_bytes,
                },
                records: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Return `content` inline when it fits, otherwise offload the full payload.
    pub fn cap_or_offload(&self, tool: &str, content: String) -> Value {
        let outcome = self.capabilities.offload.cap_or_offload(&content);
        if let Some(total_bytes) = outcome.total_bytes {
            self.capabilities
                .records
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(OffloadRecord {
                    tool: tool.to_string(),
                    total_bytes,
                });
        }
        outcome.value
    }

    /// Drain successful offloads recorded since the prior drain.
    pub fn drain_offloads(&self) -> Vec<OffloadRecord> {
        let mut records = self
            .capabilities
            .records
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        std::mem::take(&mut *records)
    }
}

impl OffloadSink {
    fn cap_or_offload(&self, content: &str) -> CapOutcome {
        let total_bytes = content.len();
        if total_bytes <= self.max_inline_bytes {
            return CapOutcome {
                value: Value::String(content.to_string()),
                total_bytes: None,
            };
        }

        let total_lines = content.lines().count();
        let head = head_within_cap(content, self.max_inline_bytes);
        match self.write(content) {
            Ok(path) => {
                let path = path.display().to_string();
                let head_lines = head.lines;
                let head = append_marker(
                    head.content,
                    &format!(
                        "[truncated: showing {head_lines} of {total_lines} lines; full output at {path}; Read with offset {} to continue]",
                        head_lines + 1,
                    ),
                );
                CapOutcome {
                    value: json!({
                        "offloaded": true,
                        "path": path,
                        "total_bytes": total_bytes,
                        "total_lines": total_lines,
                        "head_lines": head_lines,
                        "head": head,
                    }),
                    total_bytes: Some(total_bytes),
                }
            }
            Err(_) => CapOutcome {
                value: Value::String(append_marker(
                    head.content,
                    &format!("[truncated: showing {} of {total_lines} lines]", head.lines,),
                )),
                total_bytes: None,
            },
        }
    }

    fn write(&self, content: &str) -> io::Result<PathBuf> {
        std::fs::create_dir_all(&self.dir)?;
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let path = self.dir.join(format!("{hash}.txt"));
        if path.is_file() {
            return Ok(path);
        }
        let tmp = temp_path(&self.dir, &hash);
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }
}

fn temp_path(dir: &Path, hash: &str) -> PathBuf {
    dir.join(format!("{hash}.tmp"))
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

        if let Some(line_content) = line.strip_suffix('\n') {
            let next_without_newline = bytes + line_content.len();
            if next_without_newline <= cap {
                bytes = next_without_newline;
                lines += 1;
            }
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

//! `Grep` — regex search over workspace files, returning the matching
//! lines with `path:line:text` framing.

use std::path::PathBuf;

use loom_llm::{Tool, ToolOutput, tool::InvokeFuture};
use regex::Regex;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use tokio::task;
use walkdir::WalkDir;

use super::{ToolContext, parse_args, schema_for};

/// Default cap so a runaway match set does not blow up the agent's
/// transcript. Hard limit; the agent re-issues with a tighter pattern
/// when truncated.
const DEFAULT_MAX_MATCHES: usize = 1000;

/// Grep tool bound to a session context.
pub struct Grep {
    ctx: ToolContext,
}

impl Grep {
    pub fn new(ctx: ToolContext) -> Self {
        Self { ctx }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Args {
    /// Regex pattern, applied per line.
    pub pattern: String,
    /// Directory or file to search. Defaults to the current working
    /// directory of the runner (the bind-mounted workspace).
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// Optional `glob` filter on file names (matched via
    /// [`glob::Pattern`]).
    #[serde(default)]
    pub glob: Option<String>,
    /// Cap on returned match lines (default 1000).
    #[serde(default)]
    pub max_matches: Option<usize>,
}

impl Tool for Grep {
    fn name(&self) -> &str {
        "Grep"
    }

    fn description(&self) -> &str {
        "Search files for a regex `pattern`. Optional `path` restricts \
         the search root; optional `glob` filters file names."
    }

    fn input_schema(&self) -> Value {
        schema_for::<Args>()
    }

    fn invoke<'a>(&'a self, args: Value) -> InvokeFuture<'a> {
        Box::pin(async move {
            let parsed: Args = parse_args(args)?;
            let ctx = self.ctx.clone();
            task::spawn_blocking(move || search(parsed, ctx))
                .await
                .unwrap_or_else(|err| Ok(error(format!("join: {err}"))))
        })
    }
}

fn search(args: Args, ctx: ToolContext) -> Result<ToolOutput, loom_llm::LlmError> {
    let regex = match Regex::new(&args.pattern) {
        Ok(r) => r,
        Err(err) => return Ok(error(format!("invalid regex: {err}"))),
    };
    let glob_pat = match args.glob.as_deref().map(::glob::Pattern::new) {
        Some(Ok(p)) => Some(p),
        Some(Err(err)) => return Ok(error(format!("invalid glob: {err}"))),
        None => None,
    };
    let max = args.max_matches.unwrap_or(DEFAULT_MAX_MATCHES);
    let root = args.path.unwrap_or_else(|| PathBuf::from("."));
    let root = ctx.resolve_workspace_path(&root);

    let mut hits = Vec::new();
    let mut truncated = false;
    for entry in WalkDir::new(&root).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if let Some(pat) = &glob_pat
            && let Some(name) = path.file_name().and_then(|s| s.to_str())
            && !pat.matches(name)
        {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        for (lineno, line) in text.lines().enumerate() {
            if regex.is_match(line) {
                if hits.len() >= max {
                    truncated = true;
                    break;
                }
                hits.push(format!("{}:{}:{line}", path.display(), lineno + 1));
            }
        }
        if truncated {
            break;
        }
    }

    let mut content = hits.join("\n");
    if truncated {
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(&format!("[truncated at {max} matches]"));
    }
    Ok(ToolOutput {
        content: ctx.cap_or_offload("Grep", content)?,
        is_error: false,
    })
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

    fn grep_with(dir: &TempDir) -> Grep {
        Grep::new(ToolContext::new(dir.path().join("offload"), usize::MAX))
    }

    fn capped_grep_with(dir: &TempDir, cap: usize) -> Grep {
        Grep::new(ToolContext::new(dir.path().join("offload"), cap))
    }

    #[tokio::test]
    async fn grep_finds_matching_lines_in_directory() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "alpha\nbeta\ngamma").unwrap();
        std::fs::write(dir.path().join("b.txt"), "zeta\nbeta").unwrap();

        let out = grep_with(&dir)
            .invoke(json!({ "pattern": "beta", "path": dir.path() }))
            .await
            .expect("invoke");
        assert!(!out.is_error);
        let text = out.content.as_str().unwrap();
        assert!(text.contains("a.txt"), "{text}");
        assert!(text.contains("b.txt"), "{text}");
        assert_eq!(text.matches("beta").count(), 2);
    }

    #[tokio::test]
    async fn grep_glob_filters_by_filename() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "hit").unwrap();
        std::fs::write(dir.path().join("b.txt"), "hit").unwrap();

        let out = grep_with(&dir)
            .invoke(json!({ "pattern": "hit", "path": dir.path(), "glob": "*.rs" }))
            .await
            .expect("invoke");
        let text = out.content.as_str().unwrap();
        assert!(text.contains("a.rs"), "{text}");
        assert!(!text.contains("b.txt"), "{text}");
    }

    #[tokio::test]
    async fn grep_invalid_regex_returns_tool_error() {
        let dir = tempdir().unwrap();
        let out = grep_with(&dir)
            .invoke(json!({ "pattern": "(unclosed", "path": "." }))
            .await
            .expect("invoke");
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn grep_caps_results_at_max_matches() {
        let dir = tempdir().unwrap();
        let lines: String = (0..10).map(|i| format!("match {i}\n")).collect();
        std::fs::write(dir.path().join("big.txt"), lines).unwrap();

        let out = grep_with(&dir)
            .invoke(json!({
                "pattern": "match",
                "path": dir.path(),
                "max_matches": 3,
            }))
            .await
            .expect("invoke");
        let text = out.content.as_str().unwrap();
        assert!(text.contains("[truncated"), "{text}");
        assert_eq!(text.matches("match").count() - 1, 3, "{text}");
    }

    #[tokio::test]
    async fn grep_applies_inline_byte_cap_to_result_text() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("big.txt"), "match alpha\nmatch beta\n").unwrap();

        let out = capped_grep_with(&dir, 5)
            .invoke(json!({ "pattern": "match", "path": dir.path() }))
            .await
            .expect("invoke");

        assert!(!out.is_error);
        assert_eq!(out.content["offloaded"], json!(true));
        let path = out.content["path"].as_str().expect("offload path");
        let full = std::fs::read_to_string(path).unwrap();
        assert!(full.contains("match alpha"), "{full}");
        assert!(full.contains("match beta"), "{full}");
    }
}

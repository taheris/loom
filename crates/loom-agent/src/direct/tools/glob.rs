//! `Glob` — list workspace paths matching a shell-style glob pattern.

use std::path::PathBuf;

use loom_llm::{Tool, ToolOutput, tool::InvokeFuture};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use tokio::task;

use super::{ToolContext, parse_args, schema_for};

/// Glob tool bound to a session context.
pub struct Glob {
    ctx: ToolContext,
}

impl Glob {
    pub fn new(ctx: ToolContext) -> Self {
        Self { ctx }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Args {
    /// Shell-style glob (`*`, `?`, `**`, `[abc]`).
    pub pattern: String,
    /// Directory to resolve `pattern` against. Defaults to the runner's
    /// current working directory.
    #[serde(default)]
    pub path: Option<PathBuf>,
}

impl Tool for Glob {
    fn name(&self) -> &str {
        "Glob"
    }

    fn description(&self) -> &str {
        "List paths matching a shell-style glob `pattern`. Optional \
         `path` rebases the pattern against that directory."
    }

    fn input_schema(&self) -> Value {
        schema_for::<Args>()
    }

    fn invoke<'a>(&'a self, args: Value) -> InvokeFuture<'a> {
        Box::pin(async move {
            let parsed: Args = parse_args(args)?;
            let ctx = self.ctx.clone();
            task::spawn_blocking(move || expand(parsed, ctx))
                .await
                .unwrap_or_else(|err| Ok(error(format!("join: {err}"))))
        })
    }
}

fn expand(args: Args, ctx: ToolContext) -> Result<ToolOutput, loom_llm::LlmError> {
    let pattern = match args.path {
        Some(base) => ctx
            .resolve_workspace_path(&base)
            .join(&args.pattern)
            .to_string_lossy()
            .into_owned(),
        None => args.pattern.clone(),
    };
    let iter = match ::glob::glob(&pattern) {
        Ok(it) => it,
        Err(err) => return Ok(error(format!("invalid glob: {err}"))),
    };
    let mut paths = Vec::new();
    for entry in iter {
        match entry {
            Ok(p) => paths.push(p.display().to_string()),
            Err(err) => return Ok(error(format!("walk: {err}"))),
        }
    }
    Ok(ToolOutput {
        content: ctx.cap_or_offload("Glob", paths.join("\n"))?,
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

    fn glob_with(dir: &TempDir) -> Glob {
        Glob::new(ToolContext::new(dir.path().join("offload"), usize::MAX))
    }

    fn capped_glob_with(dir: &TempDir, cap: usize) -> Glob {
        Glob::new(ToolContext::new(dir.path().join("offload"), cap))
    }

    #[tokio::test]
    async fn glob_lists_files_matching_extension() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "").unwrap();
        std::fs::write(dir.path().join("b.rs"), "").unwrap();
        std::fs::write(dir.path().join("c.txt"), "").unwrap();

        let out = glob_with(&dir)
            .invoke(json!({ "pattern": "*.rs", "path": dir.path() }))
            .await
            .expect("invoke");
        assert!(!out.is_error);
        let text = out.content.as_str().unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines.iter().any(|l| l.ends_with("a.rs")));
        assert!(lines.iter().any(|l| l.ends_with("b.rs")));
        assert!(!lines.iter().any(|l| l.ends_with("c.txt")));
    }

    #[tokio::test]
    async fn glob_recursive_double_star_pattern() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub/deep")).unwrap();
        std::fs::write(dir.path().join("top.rs"), "").unwrap();
        std::fs::write(dir.path().join("sub/mid.rs"), "").unwrap();
        std::fs::write(dir.path().join("sub/deep/bot.rs"), "").unwrap();

        let out = glob_with(&dir)
            .invoke(json!({ "pattern": "**/*.rs", "path": dir.path() }))
            .await
            .expect("invoke");
        let text = out.content.as_str().unwrap();
        assert!(text.contains("top.rs"), "{text}");
        assert!(text.contains("mid.rs"), "{text}");
        assert!(text.contains("bot.rs"), "{text}");
    }

    #[tokio::test]
    async fn glob_no_matches_returns_empty_content() {
        let dir = tempdir().unwrap();
        let out = glob_with(&dir)
            .invoke(json!({ "pattern": "*.never", "path": dir.path() }))
            .await
            .expect("invoke");
        assert!(!out.is_error);
        assert_eq!(out.content, Value::String(String::new()));
    }

    #[tokio::test]
    async fn glob_invalid_pattern_returns_tool_error() {
        let dir = tempdir().unwrap();
        let out = glob_with(&dir)
            .invoke(json!({ "pattern": "[unclosed" }))
            .await
            .expect("invoke");
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn glob_applies_inline_byte_cap_to_result_text() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("alpha_long_name.rs"), "").unwrap();
        std::fs::write(dir.path().join("beta_long_name.rs"), "").unwrap();

        let out = capped_glob_with(&dir, 5)
            .invoke(json!({ "pattern": "*.rs", "path": dir.path() }))
            .await
            .expect("invoke");

        assert!(!out.is_error);
        assert_eq!(out.content["offloaded"], json!(true));
        let path = out.content["path"].as_str().expect("offload path");
        let full = std::fs::read_to_string(path).unwrap();
        assert!(full.contains("alpha_long_name.rs"), "{full}");
        assert!(full.contains("beta_long_name.rs"), "{full}");
    }
}

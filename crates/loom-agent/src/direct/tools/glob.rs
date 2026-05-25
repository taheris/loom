//! `Glob` — list workspace paths matching a shell-style glob pattern.

use std::path::PathBuf;

use loom_llm::{Tool, ToolOutput, tool::InvokeFuture};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use tokio::task;

use super::{parse_args, schema_for};

/// Zero-sized Glob tool.
pub struct Glob;

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
            let out = task::spawn_blocking(move || expand(parsed))
                .await
                .unwrap_or_else(|err| error(format!("join: {err}")));
            Ok(out)
        })
    }
}

fn expand(args: Args) -> ToolOutput {
    let pattern = match args.path {
        Some(base) => base.join(&args.pattern).to_string_lossy().into_owned(),
        None => args.pattern.clone(),
    };
    let iter = match ::glob::glob(&pattern) {
        Ok(it) => it,
        Err(err) => return error(format!("invalid glob: {err}")),
    };
    let mut paths = Vec::new();
    for entry in iter {
        match entry {
            Ok(p) => paths.push(p.display().to_string()),
            Err(err) => return error(format!("walk: {err}")),
        }
    }
    ToolOutput {
        content: Value::String(paths.join("\n")),
        is_error: false,
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
    use tempfile::tempdir;

    #[tokio::test]
    async fn glob_lists_files_matching_extension() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "").unwrap();
        std::fs::write(dir.path().join("b.rs"), "").unwrap();
        std::fs::write(dir.path().join("c.txt"), "").unwrap();

        let out = Glob
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

        let out = Glob
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
        let out = Glob
            .invoke(json!({ "pattern": "*.never", "path": dir.path() }))
            .await
            .expect("invoke");
        assert!(!out.is_error);
        assert_eq!(out.content, Value::String(String::new()));
    }

    #[tokio::test]
    async fn glob_invalid_pattern_returns_tool_error() {
        let out = Glob
            .invoke(json!({ "pattern": "[unclosed" }))
            .await
            .expect("invoke");
        assert!(out.is_error);
    }
}

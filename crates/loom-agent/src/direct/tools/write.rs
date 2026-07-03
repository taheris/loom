//! `Write` — overwrite a workspace file with new content, creating any
//! missing parent directories.

use std::path::PathBuf;

use loom_llm::{Tool, ToolOutput, tool::InvokeFuture};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use tokio::fs;

use super::{ToolContext, parse_args, schema_for};

/// Write tool bound to a session context.
pub struct Write {
    ctx: ToolContext,
}

impl Write {
    pub fn new(ctx: ToolContext) -> Self {
        Self { ctx }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Args {
    /// Destination path. Parent directories are created if absent.
    pub file_path: PathBuf,
    /// Full file contents to write. Any existing content is replaced.
    pub content: String,
}

impl Tool for Write {
    fn name(&self) -> &str {
        "Write"
    }

    fn description(&self) -> &str {
        "Write `content` to `file_path`, overwriting any existing file. \
         Creates parent directories as needed."
    }

    fn input_schema(&self) -> Value {
        schema_for::<Args>()
    }

    fn invoke<'a>(&'a self, args: Value) -> InvokeFuture<'a> {
        Box::pin(async move {
            let parsed: Args = parse_args(args)?;
            Ok(write_file(parsed, self.ctx.clone()).await)
        })
    }
}

async fn write_file(args: Args, ctx: ToolContext) -> ToolOutput {
    let display_path = args.file_path.display().to_string();
    let path = ctx.resolve_workspace_path(&args.file_path);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && let Err(err) = fs::create_dir_all(parent).await
    {
        return error(format!("mkdir {}: {err}", parent.display()));
    }

    match fs::write(&path, args.content).await {
        Ok(()) => ToolOutput {
            content: Value::String(format!("wrote {display_path}")),
            is_error: false,
        },
        Err(err) => error(format!("write {display_path}: {err}")),
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

    fn write_with(dir: &TempDir) -> Write {
        Write::new(ToolContext::new(dir.path().join("offload"), usize::MAX))
    }

    #[tokio::test]
    async fn write_creates_file_and_parent_directories() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("nested/deep/file.txt");

        let out = write_with(&dir)
            .invoke(json!({ "file_path": target, "content": "hello" }))
            .await
            .expect("invoke");
        assert!(!out.is_error, "{:?}", out.content);

        let actual = fs::read_to_string(&target).await.unwrap();
        assert_eq!(actual, "hello");
    }

    #[tokio::test]
    async fn write_overwrites_existing_file() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("over.txt");
        fs::write(&target, "old").await.unwrap();

        let out = write_with(&dir)
            .invoke(json!({ "file_path": target, "content": "new" }))
            .await
            .expect("invoke");
        assert!(!out.is_error);
        assert_eq!(fs::read_to_string(&target).await.unwrap(), "new");
    }

    #[tokio::test]
    async fn write_unwritable_path_returns_tool_error() {
        let dir = tempdir().unwrap();
        let out = write_with(&dir)
            .invoke(json!({
                "file_path": "/proc/cannot-create",
                "content": "x",
            }))
            .await
            .expect("invoke");
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn write_input_schema_requires_file_path_and_content() {
        let dir = tempdir().unwrap();
        let schema = write_with(&dir).input_schema();
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(required.contains(&"file_path"));
        assert!(required.contains(&"content"));
    }
}

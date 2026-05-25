//! `Edit` — find-and-replace `old_string` with `new_string` in a single
//! workspace file. When `replace_all` is false (the default), errors if
//! `old_string` is not unique in the file so the agent cannot
//! accidentally edit an unrelated occurrence.

use std::path::PathBuf;

use loom_llm::{Tool, ToolOutput, tool::InvokeFuture};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use tokio::fs;

use super::{parse_args, schema_for};

/// Zero-sized Edit tool.
pub struct Edit;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Args {
    /// Path to the file being edited.
    pub file_path: PathBuf,
    /// Exact substring to find. Whitespace and casing significant.
    pub old_string: String,
    /// Replacement substring.
    pub new_string: String,
    /// Replace every occurrence rather than requiring uniqueness.
    #[serde(default)]
    pub replace_all: bool,
}

impl Tool for Edit {
    fn name(&self) -> &str {
        "Edit"
    }

    fn description(&self) -> &str {
        "Replace `old_string` with `new_string` in `file_path`. Errors \
         when `old_string` is not unique unless `replace_all` is true."
    }

    fn input_schema(&self) -> Value {
        schema_for::<Args>()
    }

    fn invoke<'a>(&'a self, args: Value) -> InvokeFuture<'a> {
        Box::pin(async move {
            let parsed: Args = parse_args(args)?;
            Ok(edit_file(parsed).await)
        })
    }
}

async fn edit_file(args: Args) -> ToolOutput {
    let original = match fs::read_to_string(&args.file_path).await {
        Ok(text) => text,
        Err(err) => return error(format!("read {}: {err}", args.file_path.display())),
    };

    let count = original.matches(args.old_string.as_str()).count();
    if count == 0 {
        return error("old_string not found".to_string());
    }
    if count > 1 && !args.replace_all {
        return error(format!(
            "old_string is not unique ({count} matches); pass replace_all=true to apply to all",
        ));
    }

    let updated = if args.replace_all {
        original.replace(&args.old_string, &args.new_string)
    } else {
        original.replacen(&args.old_string, &args.new_string, 1)
    };

    match fs::write(&args.file_path, updated).await {
        Ok(()) => ToolOutput {
            content: Value::String(format!(
                "edited {} ({count} replacement{})",
                args.file_path.display(),
                if count == 1 { "" } else { "s" },
            )),
            is_error: false,
        },
        Err(err) => error(format!("write {}: {err}", args.file_path.display())),
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
    async fn edit_replaces_single_unique_occurrence() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.txt");
        fs::write(&path, "hello world").await.unwrap();

        let out = Edit
            .invoke(json!({
                "file_path": path,
                "old_string": "world",
                "new_string": "loom",
            }))
            .await
            .expect("invoke");
        assert!(!out.is_error, "{:?}", out.content);
        assert_eq!(fs::read_to_string(&path).await.unwrap(), "hello loom");
    }

    #[tokio::test]
    async fn edit_errors_when_old_string_not_unique_and_replace_all_false() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("dup.txt");
        fs::write(&path, "foo foo foo").await.unwrap();

        let out = Edit
            .invoke(json!({
                "file_path": path,
                "old_string": "foo",
                "new_string": "bar",
            }))
            .await
            .expect("invoke");
        assert!(out.is_error);
        assert_eq!(fs::read_to_string(&path).await.unwrap(), "foo foo foo");
    }

    #[tokio::test]
    async fn edit_replace_all_applies_to_every_occurrence() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("dup.txt");
        fs::write(&path, "foo foo foo").await.unwrap();

        let out = Edit
            .invoke(json!({
                "file_path": path,
                "old_string": "foo",
                "new_string": "bar",
                "replace_all": true,
            }))
            .await
            .expect("invoke");
        assert!(!out.is_error, "{:?}", out.content);
        assert_eq!(fs::read_to_string(&path).await.unwrap(), "bar bar bar");
    }

    #[tokio::test]
    async fn edit_errors_when_old_string_absent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.txt");
        fs::write(&path, "hello").await.unwrap();

        let out = Edit
            .invoke(json!({
                "file_path": path,
                "old_string": "absent",
                "new_string": "x",
            }))
            .await
            .expect("invoke");
        assert!(out.is_error);
    }
}

use std::fs;
use std::path::{Path, PathBuf};

use loom_driver::config::AgentSelection;

use crate::spawn::container_workspace_path;

pub(crate) struct Launch {
    pub(crate) argv: Vec<String>,
    pub(crate) session_dir: PathBuf,
}

pub(crate) fn prepare_launch(
    workspace: &Path,
    selection: &AgentSelection,
    scratch_dir: &Path,
) -> std::io::Result<Launch> {
    let session_dir = scratch_dir.join("pi-sessions");
    fs::create_dir_all(&session_dir)?;

    let extension_path = scratch_dir.join("loom-pi-repin-extension.js");
    let prompt_path = container_workspace_path(workspace, &scratch_dir.join("prompt.txt"));
    let scratchpad_path = container_workspace_path(workspace, &scratch_dir.join("scratch.md"));
    fs::write(
        &extension_path,
        repin_extension_source(&prompt_path, &scratchpad_path),
    )?;

    let container_session_dir = container_workspace_path(workspace, &session_dir);
    let container_extension_path = container_workspace_path(workspace, &extension_path);
    let argv = build_wrix_argv(
        workspace,
        &prompt_path,
        selection,
        &container_session_dir,
        &container_extension_path,
    );
    Ok(Launch { argv, session_dir })
}

pub(crate) fn repin_extension_source(prompt_path: &Path, scratchpad_path: &Path) -> String {
    let prompt_path = json_string(prompt_path);
    let scratchpad_path = json_string(scratchpad_path);
    format!(
        r###"import {{ readFileSync }} from "node:fs";

export default function(pi) {{
  const promptPath = {prompt_path};
  const scratchpadPath = {scratchpad_path};

  function readRequiredText(path, label) {{
    try {{
      return readFileSync(path, "utf8");
    }} catch (err) {{
      const message = err instanceof Error ? err.message : String(err);
      const detail = "Loom compaction recovery could not read " + label + " at " + path + ": " + message;
      throw new Error(detail);
    }}
  }}

  function contentText(content) {{
    if (typeof content === "string") return content;
    if (!Array.isArray(content)) return "";
    return content.map((block) => {{
      if (!block || typeof block !== "object") return "";
      if (block.type === "text") return block.text ?? block.content ?? "";
      if (block.type === "thinking") return block.thinking ?? "";
      if (block.type === "toolCall") return `${{block.name ?? "tool"}} ${{JSON.stringify(block.arguments ?? {{}})}}`;
      return "";
    }}).join("");
  }}

  function messageText(message) {{
    if (!message || typeof message !== "object") return "";
    return contentText(message.content);
  }}

  pi.on("context", async (event) => {{
    const prompt = readRequiredText(promptPath, "prompt");
    if (!prompt) {{
      throw new Error("Loom compaction recovery prompt is empty at " + promptPath);
    }}
    if (!Array.isArray(event.messages)) return;
    if (event.messages.some((message) => messageText(message).includes(prompt))) return;

    const scratchpad = readRequiredText(scratchpadPath, "scratchpad").trimEnd();
    const pinned = [
      "Loom post-compaction pinned context. Continue following this phase prompt and scratchpad exactly.",
      "",
      "## Original Loom prompt",
      prompt,
      "",
      "## Loom scratchpad",
      scratchpad || "(empty)",
    ].join("\n");

    return {{
      messages: [{{
        role: "custom",
        customType: "loom-repin",
        content: pinned,
        display: false,
        timestamp: Date.now(),
      }}, ...event.messages],
    }};
  }});
}}
"###
    )
}

pub(crate) fn build_wrix_argv(
    workspace: &Path,
    prompt_path: &Path,
    selection: &AgentSelection,
    session_dir: &Path,
    extension_path: &Path,
) -> Vec<String> {
    let mut argv = vec![
        "run".to_string(),
        workspace.to_string_lossy().into_owned(),
        "pi".to_string(),
        "--session-dir".to_string(),
        session_dir.to_string_lossy().into_owned(),
        "-e".to_string(),
        extension_path.to_string_lossy().into_owned(),
    ];
    if let Some(provider) = &selection.provider {
        argv.push("--provider".to_string());
        argv.push(provider.clone());
    }
    if let Some(model_id) = &selection.model_id {
        argv.push("--model".to_string());
        argv.push(model_id.clone());
    }
    if let Some(level) = selection.thinking_level {
        argv.push("--thinking".to_string());
        argv.push(level.as_str().to_string());
    }
    argv.push(format!("@{}", prompt_path.to_string_lossy()));
    argv
}

fn json_string(path: &Path) -> String {
    serde_json::Value::String(path.to_string_lossy().into_owned()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::agent::{AgentKind, ThinkingLevel};
    use loom_driver::identifier::ProfileName;

    fn pi_selection() -> AgentSelection {
        AgentSelection {
            profile: ProfileName::new("base"),
            kind: AgentKind::Pi,
            provider: Some("openai".to_string()),
            model_id: Some("gpt-4o".to_string()),
            thinking_level: Some(ThinkingLevel::High),
            claude_settings: None,
        }
    }

    #[test]
    fn argv_uses_wrix_run_with_session_extension_model_and_prompt_reference() {
        let argv = build_wrix_argv(
            &PathBuf::from("/work"),
            &PathBuf::from("/workspace/.loom/scratch/inbox/prompt.txt"),
            &pi_selection(),
            &PathBuf::from("/workspace/.loom/scratch/inbox/pi-sessions"),
            &PathBuf::from("/workspace/.loom/scratch/inbox/loom-pi-repin-extension.js"),
        );
        assert_eq!(argv[0], "run");
        assert_eq!(argv[1], "/work");
        assert_eq!(argv[2], "pi");
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--provider" && w[1] == "openai")
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--model" && w[1] == "gpt-4o")
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--thinking" && w[1] == "high")
        );
        assert!(argv.iter().any(|a| a == "-e"));
        assert!(argv.iter().any(|a| a == "--session-dir"));
        assert_eq!(
            argv.last().map(String::as_str),
            Some("@/workspace/.loom/scratch/inbox/prompt.txt")
        );
        assert!(!argv.iter().any(|a| a == "spawn"));
        assert!(!argv.iter().any(|a| a == "--stdio"));
    }

    #[test]
    fn repin_extension_raises_errors_when_repin_files_cannot_be_read() {
        let source = repin_extension_source(
            &PathBuf::from("/workspace/.loom/scratch/inbox/prompt.txt"),
            &PathBuf::from("/workspace/.loom/scratch/inbox/scratch.md"),
        );

        assert!(source.contains("readRequiredText(promptPath, \"prompt\")"));
        assert!(source.contains("readRequiredText(scratchpadPath, \"scratchpad\")"));
        assert!(source.contains("Loom compaction recovery could not read"));
        assert!(!source.contains("catch (_err)"));
    }
}

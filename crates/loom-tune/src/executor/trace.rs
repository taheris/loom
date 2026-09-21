use std::collections::{HashMap, HashSet};
use std::path::{Component, Path};

use loom_events::AgentEvent;

use super::{Error, Evidence, unavailable};

pub(super) enum Action {
    Read(String),
    Edit(String),
    Command(String),
}

pub(super) struct Operation {
    pub start: usize,
    pub end: usize,
    pub action: Action,
    pub success: bool,
}

/// Only paired, completed backend tool events establish execution or ordering.
pub(super) fn operations(evidence: &Evidence) -> Result<Vec<Operation>, Error> {
    let recorded = evidence
        .recorded
        .as_ref()
        .ok_or_else(|| unavailable("backend tool events were not captured"))?;
    let mut pending = HashMap::new();
    let mut completed = HashSet::new();
    let mut operations = Vec::new();
    for (index, event) in recorded.events.iter().enumerate() {
        match event {
            AgentEvent::ToolCall {
                id, tool, params, ..
            } => {
                if pending.contains_key(id) || completed.contains(id) {
                    return Err(unavailable("duplicate tool call identity"));
                }
                let action = match tool.as_str() {
                    "read" | "read_file" | "Read" => {
                        Action::Read(tool_path(params, &recorded.workspace)?)
                    }
                    "edit" | "edit_file" | "Edit" | "write" | "write_file" | "Write" => {
                        Action::Edit(tool_path(params, &recorded.workspace)?)
                    }
                    "bash" | "Bash" => Action::Command(
                        params
                            .get("command")
                            .and_then(serde_json::Value::as_str)
                            .ok_or_else(|| unavailable("command tool has no command string"))?
                            .to_owned(),
                    ),
                    _ => {
                        return Err(unavailable(format!(
                            "tool {tool:?} has no audited trace semantics"
                        )));
                    }
                };
                pending.insert(id, (index, action));
            }
            AgentEvent::ToolResult { id, is_error, .. } => {
                let (start, action) = pending
                    .remove(id)
                    .ok_or_else(|| unavailable("tool result has no unique preceding call"))?;
                completed.insert(id);
                operations.push(Operation {
                    start,
                    end: index,
                    action,
                    success: !is_error,
                });
            }
            _ => {}
        }
    }
    if !pending.is_empty() {
        return Err(unavailable("tool calls have no completed results"));
    }
    Ok(operations)
}

fn tool_path(params: &serde_json::Value, root: &Path) -> Result<String, Error> {
    let raw = params
        .get("path")
        .or_else(|| params.get("file_path"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| unavailable("file tool has no path"))?;
    let path = Path::new(raw);
    let relative = if path.is_absolute() {
        path.strip_prefix(root)
            .or_else(|_| path.strip_prefix("/workspace"))
            .map_err(|_| unavailable("tool path is outside the replay workspace"))?
    } else {
        path
    };
    let mut normalized = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => normalized.push(
                part.to_str()
                    .ok_or_else(|| unavailable("tool path is not UTF-8"))?,
            ),
            Component::CurDir => {}
            _ => {
                return Err(unavailable(
                    "tool path has unresolved parent/root components",
                ));
            }
        }
    }
    if normalized.is_empty() {
        return Err(unavailable("tool path is empty"));
    }
    Ok(normalized.join("/"))
}

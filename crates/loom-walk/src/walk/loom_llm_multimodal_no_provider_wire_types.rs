//! Public multimodal request types are owned by `loom-llm`; provider
//! wire structs must stay behind Client implementations.

use super::util::{is_comment, read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "loom_llm_multimodal_no_provider_wire_types — public multimodal request signatures do not expose provider wire structs";
const SRC: &str = "crates/loom-llm/src/request.rs";

const BANNED: &[&str] = &[
    "genai::",
    "ChatRequest",
    "ChatMessage",
    "ChatResponse",
    "ContentPart",
    "BinarySource",
    "ToolCall",
    "ToolResponse",
    "inline_data",
    "image_url",
    "input_file",
];

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let path = root.join(SRC);
    let Some(body) = read_to_string(&path) else {
        return verdict_from(RULE, vec![format!("{SRC}:1 unable to read request model")]);
    };

    let mut violations = Vec::new();
    for (idx, line) in body.lines().enumerate() {
        let trimmed = line.trim_start();
        if is_comment(trimmed) || !is_public_multimodal_line(trimmed) {
            continue;
        }
        for banned in BANNED {
            if trimmed.contains(banned) {
                violations.push(format!(
                    "{SRC}:{} public multimodal surface mentions provider wire token `{banned}`",
                    idx + 1,
                ));
            }
        }
    }

    verdict_from(RULE, violations)
}

fn is_public_multimodal_line(line: &str) -> bool {
    (line.starts_with("pub struct ")
        || line.starts_with("pub enum ")
        || line.starts_with("pub fn ")
        || line.starts_with("pub const ")
        || line.starts_with("pub "))
        && (line.contains("MessageContent")
            || line.contains("BinaryContent")
            || line.contains("MimeType")
            || line.contains("binary")
            || line.contains("mime_type")
            || line.contains("bytes"))
}

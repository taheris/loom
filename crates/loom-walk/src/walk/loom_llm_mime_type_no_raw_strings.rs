//! Public binary-content APIs must require validated `MimeType` values
//! rather than raw strings.

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "loom_llm_mime_type_no_raw_strings — public binary-content APIs accept MimeType, not raw String or &str MIME values";
const SRC: &str = "crates/loom-llm/src/request.rs";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let path = root.join(SRC);
    let Some(body) = read_to_string(&path) else {
        return verdict_from(RULE, vec![format!("{SRC}:1 unable to read request model")]);
    };

    let mut violations = Vec::new();
    for (line_no, signature) in public_binary_signatures(&body) {
        if signature.contains("mime_type: String")
            || signature.contains("mime_type: &str")
            || signature.contains("mime_type: impl Into<String>")
            || signature.contains("mime_type: impl AsRef<str>")
        {
            violations.push(format!(
                "{SRC}:{line_no} public binary API accepts an unvalidated MIME string — use MimeType"
            ));
        }
        if signature.contains("mime_type:") && !signature.contains("mime_type: MimeType") {
            violations.push(format!(
                "{SRC}:{line_no} public binary API MIME parameter must be `MimeType`"
            ));
        }
    }

    verdict_from(RULE, violations)
}

fn public_binary_signatures(body: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut lines = body.lines().enumerate();
    while let Some((idx, line)) = lines.next() {
        let trimmed = line.trim_start();
        let Some(start) = trimmed.find("pub fn ") else {
            continue;
        };
        if !trimmed[start..].contains("binary") {
            continue;
        }
        let mut signature = trimmed[start..].to_string();
        while !signature.contains('{') && !signature.contains(";") {
            let Some((_, next)) = lines.next() else {
                break;
            };
            signature.push(' ');
            signature.push_str(next.trim());
        }
        out.push((idx + 1, signature));
    }
    out
}

//! `loom-agent::direct` wraps `loom-llm::Conversation` to satisfy the
//! `Session` trait; the backend setup path also consumes the materialized
//! skill registry. `loom-agent` must therefore depend on `loom-llm`,
//! `loom-events`, and `loom-skill` directly.

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str =
    "loom_agent_deps — `loom-agent` depends on `loom-llm`, `loom-events`, and `loom-skill`";

const REQUIRED: &[&str] = &["loom-llm", "loom-events", "loom-skill"];

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let manifest = root.join("crates/loom-agent/Cargo.toml");
    let mut violations = Vec::new();

    let Some(body) = read_to_string(&manifest) else {
        violations.push("crates/loom-agent/Cargo.toml:1 manifest not readable".to_string());
        return verdict_from(RULE, violations);
    };

    let Some(section) = section_body(&body, "[dependencies]") else {
        violations
            .push("crates/loom-agent/Cargo.toml:1 [dependencies] section missing".to_string());
        return verdict_from(RULE, violations);
    };
    let keys = section_keys(section);

    for required in REQUIRED {
        if !keys.iter().any(|k| k == required) {
            violations.push(format!(
                "crates/loom-agent/Cargo.toml:1 missing required dependency `{required}` — `loom-agent` must depend on `loom-llm`, `loom-events`, and `loom-skill`",
            ));
        }
    }

    verdict_from(RULE, violations)
}

fn section_body<'a>(body: &'a str, header: &str) -> Option<&'a str> {
    let start = body.find(header)?;
    let after_header_offset = body[start..].find('\n').map(|n| start + n + 1)?;
    let tail = &body[after_header_offset..];
    let end_rel = tail.find("\n[").unwrap_or(tail.len());
    Some(&tail[..end_rel])
}

fn section_keys(section: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in section.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let key = match line.split_once('=') {
            Some((k, _)) => k.trim().trim_matches('"'),
            None => continue,
        };
        if key.is_empty() {
            continue;
        }
        out.push(key.to_string());
    }
    out
}

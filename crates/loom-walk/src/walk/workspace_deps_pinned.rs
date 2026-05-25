//! RS-2: every third-party dependency is pinned exactly once under
//! `[workspace.dependencies]` at the workspace root; member crates
//! consume them with `foo = { workspace = true }`. The walk checks the
//! pinning side: the section must exist with at least one entry, and
//! the named third-party deps from the loom-harness spec must each be
//! pinned.

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str =
    "workspace_deps_pinned — every shared dep is pinned once under [workspace.dependencies]";

const REQUIRED_DEPS: &[&str] = &[
    "tokio",
    "serde",
    "serde_json",
    "thiserror",
    "displaydoc",
    "anyhow",
    "tracing",
    "tracing-subscriber",
    "rusqlite",
    "toml",
    "askama",
    "clap",
    "gix",
    "fd-lock",
];

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let manifest = root.join("Cargo.toml");
    let mut violations = Vec::new();

    let Some(body) = read_to_string(&manifest) else {
        violations.push("Cargo.toml:1 workspace manifest not readable".to_string());
        return verdict_from(RULE, violations);
    };

    let Some(section) = section_body(&body, "[workspace.dependencies]") else {
        violations.push("Cargo.toml:1 [workspace.dependencies] section missing".to_string());
        return verdict_from(RULE, violations);
    };
    let keys = section_keys(section);
    for dep in REQUIRED_DEPS {
        if !keys.iter().any(|k| k == dep) {
            violations.push(format!(
                "Cargo.toml:1 `{dep}` not pinned in [workspace.dependencies]",
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

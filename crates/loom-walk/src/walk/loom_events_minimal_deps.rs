//! `loom-events` is the public-contract leaf crate that frontends, SSE
//! bridges, and log analyzers depend on. Its `[dependencies]` table is
//! kept tight to control the dependency surface every consumer ships:
//! exactly `futures-core`, `serde`, `serde_json`, and `thiserror`.
//! `futures-core` carries the `Stream` trait referenced by
//! `Session::Events`. The forbidden trio (`chrono`, `ulid`, `uuid`) flag
//! past temptations to expand the surface; future additions require a
//! spec change first.

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "loom_events_minimal_deps — runtime deps are exactly { futures-core, serde, serde_json, thiserror }";

const REQUIRED: &[&str] = &["futures-core", "serde", "serde_json", "thiserror"];
const FORBIDDEN: &[&str] = &["chrono", "ulid", "uuid"];

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let manifest = root.join("crates/loom-events/Cargo.toml");
    let mut violations = Vec::new();

    let Some(body) = read_to_string(&manifest) else {
        violations.push("crates/loom-events/Cargo.toml:1 manifest not readable".to_string());
        return verdict_from(RULE, violations);
    };

    let Some(section) = section_body(&body, "[dependencies]") else {
        violations
            .push("crates/loom-events/Cargo.toml:1 [dependencies] section missing".to_string());
        return verdict_from(RULE, violations);
    };
    let keys = section_keys(section);

    for required in REQUIRED {
        if !keys.iter().any(|k| k == required) {
            violations.push(format!(
                "crates/loom-events/Cargo.toml:1 missing required dependency `{required}`",
            ));
        }
    }
    for key in &keys {
        if !REQUIRED.iter().any(|r| r == key) {
            violations.push(format!(
                "crates/loom-events/Cargo.toml:1 unexpected dependency `{key}` — runtime deps must be exactly {{ futures-core, serde, serde_json, thiserror }}",
            ));
        }
    }
    for forbidden in FORBIDDEN {
        if body.lines().any(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with(forbidden)
                && trimmed[forbidden.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c == ' ' || c == '\t' || c == '=')
        }) {
            violations.push(format!(
                "crates/loom-events/Cargo.toml:1 forbidden dependency `{forbidden}` — `loom-events` must not depend on it",
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

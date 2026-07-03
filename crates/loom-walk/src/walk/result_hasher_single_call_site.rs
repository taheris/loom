//! `ResultHasher` is the shared canonicalization + BLAKE3-16 utility
//! both observers consume. The walk checks the live `Conversation` path
//! fingerprints each tool result once and fans the resulting
//! `ResultFingerprint` into both observers instead of letting each
//! observer canonicalize the same payload independently.

use std::path::Path;

use super::util::{
    parse_rs, read_to_string, rel, rs_files_recursive, verdict_from, workspace_root,
};
use super::{Verdict, WalkInput};

const RULE: &str = "result_hasher_single_call_site — live tool results are fingerprinted once before observer fan-out";

const SRC_DIR: &str = "crates/loom-llm/src";
const CONVERSATION: &str = "crates/loom-llm/src/conversation.rs";
const SYMBOL: &str = "ResultHasher";
const CENTRAL_FINGERPRINT_CALL: &str = "ResultHasher::result_fingerprint(&output.content)";
const OBSERVER_FANOUT_CALL: &str = ".observe_tool_result(&event_id, fingerprint)";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let src_dir = root.join(SRC_DIR);
    let mut violations = Vec::new();

    let mut observer_call_sites: Vec<String> = Vec::new();
    for path in rs_files_recursive(&src_dir) {
        let Some(body) = read_to_string(&path) else {
            continue;
        };
        if !contains_identifier(&body, SYMBOL) {
            continue;
        }
        if file_defines_type(&path, SYMBOL) || rel(&root, &path) == CONVERSATION {
            continue;
        }
        observer_call_sites.push(rel(&root, &path));
    }
    observer_call_sites.sort();

    if observer_call_sites.len() != 2 {
        let listing = if observer_call_sites.is_empty() {
            "<none>".to_string()
        } else {
            observer_call_sites.join(", ")
        };
        violations.push(format!(
            "{SRC_DIR}/observer.rs:1 expected exactly 2 observer files to reference `ResultHasher`, found {} ({listing})",
            observer_call_sites.len(),
        ));
    }

    let conversation_path = root.join(CONVERSATION);
    let Some(conversation) = read_to_string(&conversation_path) else {
        violations.push(format!("{CONVERSATION}:1 missing conversation live path"));
        return verdict_from(RULE, violations);
    };

    let central_count = count_occurrences(&conversation, CENTRAL_FINGERPRINT_CALL);
    if central_count != 1 {
        violations.push(format!(
            "{CONVERSATION}:1 expected exactly one live-path `{CENTRAL_FINGERPRINT_CALL}` call, found {central_count}",
        ));
    }

    let fanout_count = count_occurrences(&conversation, OBSERVER_FANOUT_CALL);
    if fanout_count != 2 {
        violations.push(format!(
            "{CONVERSATION}:1 expected shared fingerprint fan-out to both observers, found {fanout_count} `{OBSERVER_FANOUT_CALL}` calls",
        ));
    }

    match section_between(
        &conversation,
        "fn observe_tool_result",
        "fn next_observer_envelope",
    ) {
        Some(section) => {
            if section.contains(".emit(&event)") {
                violations.push(format!(
                    "{CONVERSATION}:1 observe_tool_result must not re-emit ToolResult events into observers; fan out the shared fingerprint instead",
                ));
            }
        }
        None => violations.push(format!(
            "{CONVERSATION}:1 missing observe_tool_result function before next_observer_envelope",
        )),
    }

    verdict_from(RULE, violations)
}

fn contains_identifier(body: &str, ident: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = body[start..].find(ident) {
        let abs = start + pos;
        let before_ok = abs
            .checked_sub(1)
            .and_then(|i| body.as_bytes().get(i))
            .is_none_or(|b| !is_ident_byte(*b));
        let after_ok = body
            .as_bytes()
            .get(abs + ident.len())
            .is_none_or(|b| !is_ident_byte(*b));
        if before_ok && after_ok {
            return true;
        }
        start = abs + ident.len();
    }
    false
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.match_indices(needle).count()
}

fn section_between<'a>(body: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let start_idx = body.find(start)?;
    let rest = body.get(start_idx..)?;
    let end_idx = rest.find(end)?;
    rest.get(..end_idx)
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn file_defines_type(path: &Path, name: &str) -> bool {
    let Some(file) = parse_rs(path) else {
        return false;
    };
    item_defines(&file.items, name)
}

fn item_defines(items: &[syn::Item], name: &str) -> bool {
    for item in items {
        let hit = match item {
            syn::Item::Struct(s) => s.ident == name,
            syn::Item::Enum(e) => e.ident == name,
            syn::Item::Trait(t) => t.ident == name,
            syn::Item::Type(t) => t.ident == name,
            syn::Item::Mod(m) => m
                .content
                .as_ref()
                .is_some_and(|(_, nested)| item_defines(nested, name)),
            _ => false,
        };
        if hit {
            return true;
        }
    }
    false
}

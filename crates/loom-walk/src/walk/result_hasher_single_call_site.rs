//! `ResultHasher` is the shared canonicalization + BLAKE3-16 utility
//! both observers consume. Per `specs/llm.md`, per-result
//! canonicalization happens once — the helper is invoked from exactly
//! the two observer call sites (`DoomLoopObserver` and
//! `DuplicateResultObserver`). The walk asserts that, across
//! `crates/loom-llm/src/`, the `ResultHasher` identifier appears in
//! exactly two non-defining source files. The defining file (the file
//! that contains `struct ResultHasher`) is excluded from the count so
//! its `impl` blocks don't inflate the call-site total.

use std::path::Path;

use super::util::{
    parse_rs, read_to_string, rel, rs_files_recursive, verdict_from, workspace_root,
};
use super::{Verdict, WalkInput};

const RULE: &str =
    "result_hasher_single_call_site — `ResultHasher` invoked from exactly two observer files";

const SRC_DIR: &str = "crates/loom-llm/src";
const SYMBOL: &str = "ResultHasher";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let src_dir = root.join(SRC_DIR);
    let mut violations = Vec::new();

    let mut call_sites: Vec<String> = Vec::new();
    for path in rs_files_recursive(&src_dir) {
        let Some(body) = read_to_string(&path) else {
            continue;
        };
        if !contains_identifier(&body, SYMBOL) {
            continue;
        }
        if file_defines_type(&path, SYMBOL) {
            continue;
        }
        call_sites.push(rel(&root, &path));
    }
    call_sites.sort();

    if call_sites.len() != 2 {
        let listing = if call_sites.is_empty() {
            "<none>".to_string()
        } else {
            call_sites.join(", ")
        };
        violations.push(format!(
            "{SRC_DIR}/observer.rs:1 expected exactly 2 non-defining files referencing `ResultHasher`, found {} ({listing})",
            call_sites.len(),
        ));
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

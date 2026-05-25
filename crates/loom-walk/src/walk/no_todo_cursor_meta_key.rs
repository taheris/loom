//! No `meta.todo_cursor:<label>` keys in the state-DB schema. The
//! per-spec cursor was replaced by the molecule's `loom.base_commit`
//! bead metadata; any surviving reference in `loom-driver/src/state/db.rs`
//! is dead schema that re-introduces the legacy cursor concept.

use super::util::{is_comment, parse_rs, read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

use syn::spanned::Spanned;

const RULE: &str = "no_todo_cursor_meta_key — state-DB schema must not reference `todo_cursor`";

const DB_RS: &str = "crates/loom-driver/src/state/db.rs";
const NEEDLE: &str = "todo_cursor";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let path = root.join(DB_RS);
    let mut violations = Vec::new();

    let Some(body) = read_to_string(&path) else {
        violations.push(format!("{DB_RS}:1 file not found"));
        return verdict_from(RULE, violations);
    };

    let test_ranges = test_line_ranges(&path);

    for (lineno, raw) in body.lines().enumerate() {
        let lineno = lineno + 1;
        if is_comment(raw) {
            continue;
        }
        if line_in_test(&test_ranges, lineno) {
            continue;
        }
        if raw.contains(NEEDLE) {
            violations.push(format!(
                "{DB_RS}:{lineno} `{NEEDLE}` — meta key removed; use `loom.base_commit` bead metadata",
            ));
        }
    }

    verdict_from(RULE, violations)
}

fn test_line_ranges(path: &std::path::Path) -> Vec<(usize, usize)> {
    let Some(file) = parse_rs(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in &file.items {
        if let syn::Item::Mod(m) = item
            && m.attrs.iter().any(is_cfg_test)
        {
            let start = m.span().start().line;
            let end = m.span().end().line;
            out.push((start, end));
        }
    }
    out
}

fn is_cfg_test(attr: &syn::Attribute) -> bool {
    if !attr.path().is_ident("cfg") {
        return false;
    }
    let mut found = false;
    let _ = attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("test") {
            found = true;
        }
        Ok(())
    });
    found
}

fn line_in_test(ranges: &[(usize, usize)], line: usize) -> bool {
    ranges.iter().any(|(s, e)| line >= *s && line <= *e)
}

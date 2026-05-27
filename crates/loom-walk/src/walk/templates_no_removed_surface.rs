//! Templates under `crates/loom-templates/templates/` must NOT direct
//! agents at subcommands the harness has removed. `loom run` was renamed
//! to `loom loop`; `loom check <X>` was renamed to `loom gate <X>`
//! (`loom check surface` / `loom check criteria` collapsed into
//! `loom gate verify`). A template that still names the old surface
//! tells the agent to run a command the binary no longer exposes —
//! Invariant 3 from `specs/gate.md`.

use std::path::PathBuf;

use walkdir::WalkDir;

use super::util::{narrow_to_loom_files, read_to_string, rel, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "templates_no_removed_surface — templates must not name \
                    `loom run` or `loom check` (use `loom loop` / `loom gate`)";

const TEMPLATE_DIR: &str = "crates/loom-templates/templates";

const REMOVED_TOKENS: &[(&str, &str)] = &[
    (
        "loom run",
        "renamed to `loom loop` per specs/harness.md Removed surface",
    ),
    (
        "loom check",
        "renamed to `loom gate` per specs/harness.md Removed surface",
    ),
];

pub fn run(input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let template_root = root.join(TEMPLATE_DIR);
    let scope: Vec<PathBuf> = WalkDir::new(&template_root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("md"))
        .map(|e| e.path().to_path_buf())
        .collect();
    let scope = narrow_to_loom_files(scope, input, &root);
    let mut violations = Vec::new();
    for path in scope {
        let Some(body) = read_to_string(&path) else {
            continue;
        };
        let rel_path = rel(&root, &path);
        for (lineno, line) in body.lines().enumerate() {
            for (needle, rename) in REMOVED_TOKENS {
                if has_word_boundary_match(line, needle) {
                    violations.push(format!(
                        "{}:{} `{}` — {}",
                        rel_path,
                        lineno + 1,
                        needle,
                        rename,
                    ));
                }
            }
        }
    }
    verdict_from(RULE, violations)
}

/// `true` iff `needle` appears in `haystack` followed by a non-alphanumeric
/// (or end-of-string) — so `loom run` matches `loom run -s` and `\`loom run\``
/// but not `loom runner` / `loom runtime`. Word-boundary intentionally only
/// checks the trailing edge; the leading edge is fenced by the literal
/// space between `loom` and the subcommand.
fn has_word_boundary_match(haystack: &str, needle: &str) -> bool {
    let mut search_from = 0;
    while let Some(idx) = haystack[search_from..].find(needle) {
        let absolute = search_from + idx;
        let after = absolute + needle.len();
        let next = haystack[after..].chars().next();
        if next.is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_') {
            return true;
        }
        search_from = after;
    }
    false
}

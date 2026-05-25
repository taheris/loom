//! RS-10: production code uses `#[expect(dead_code, reason = "...")]`,
//! never `#[allow(dead_code)]`. `expect` fails compilation if the
//! warning stops firing; `allow` silently rots and lets dead modules
//! accumulate. The criterion is scoped to non-test code — `tests/`
//! files and `#[cfg(test)]` blocks are out of scope.

use super::util::{
    is_comment, narrow_to_loom_files, read_to_string, rel, src_files, verdict_from, workspace_root,
};
use super::{Verdict, WalkInput};

const RULE: &str = concat!(
    "no_allow_dead_code — use #[expect",
    "(dead_code, reason = \"...\")] in production code",
);

const NEEDLE: &str = concat!("#[allow", "(dead_code)");

const REPLACEMENT: &str = concat!("#[expect", "(dead_code");

pub fn run(input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let scope = narrow_to_loom_files(src_files(&root), input, &root);
    let mut violations = Vec::new();
    for path in scope {
        let Some(body) = read_to_string(&path) else {
            continue;
        };
        let rel_path = rel(&root, &path);
        // The walk's own source carries the literal `#[allow(dead_code)`
        // needle inside a `concat!` argument; reading the file back
        // through the scan would self-flag every line.
        if rel_path == "crates/loom-walk/src/walk/no_allow_dead_code.rs" {
            continue;
        }
        for (lineno, line) in body.lines().enumerate() {
            if is_comment(line) {
                continue;
            }
            if line.contains(NEEDLE) {
                violations.push(format!(
                    "{}:{} `{}]` — use `{}, reason = \"...\")]`",
                    rel_path,
                    lineno + 1,
                    NEEDLE,
                    REPLACEMENT,
                ));
            }
        }
    }
    verdict_from(RULE, violations)
}

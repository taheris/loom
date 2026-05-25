//! RS-9: no `unwrap()` / `expect()` / `panic!()` / `todo!()` /
//! `unimplemented!()` / `unreachable!()` in production source. The
//! workspace clippy restriction lints (RS-3) catch most cases; this
//! walk is the belt-and-braces backstop that runs even when clippy is
//! skipped and that surfaces violations as a single per-criterion
//! verdict.
//!
//! Test code is exempt: `#[cfg(test)]` blocks are stripped before the
//! scan, and files under `tests/` are out of scope. The integration-
//! test binaries `bd-shim` and `mock-loom-agent` live under `src/bin/`
//! but exist solely as test fixtures, so they are skipped too.

use super::util::{
    is_comment, narrow_to_loom_files, read_to_string, rel, src_files, verdict_from, workspace_root,
};
use super::{Verdict, WalkInput};

const RULE: &str =
    "no_panics_in_production — production code returns typed errors instead of panicking";

const NEEDLE_UNWRAP: &str = concat!("unwrap", "(");
const NEEDLE_EXPECT: &str = concat!("expect", "(");
const NEEDLE_PANIC: &str = concat!("panic", "!(");
const NEEDLE_TODO: &str = concat!("todo", "!(");
const NEEDLE_UNIMPLEMENTED: &str = concat!("unimplemented", "!(");
const NEEDLE_UNREACHABLE: &str = concat!("unreachable", "!(");

const EXEMPT_BINS: &[&str] = &[
    "crates/loom/src/bin/bd-shim.rs",
    "crates/loom/src/bin/mock-loom-agent.rs",
];

pub fn run(input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let scope = narrow_to_loom_files(src_files(&root), input, &root);
    let mut violations = Vec::new();
    let needles: [(&str, &str); 6] = [
        (NEEDLE_UNWRAP, "unwrap"),
        (NEEDLE_EXPECT, "expect"),
        (NEEDLE_PANIC, "panic!"),
        (NEEDLE_TODO, "todo!"),
        (NEEDLE_UNIMPLEMENTED, "unimplemented!"),
        (NEEDLE_UNREACHABLE, "unreachable!"),
    ];
    for path in scope {
        let rel_path = rel(&root, &path);
        if EXEMPT_BINS.iter().any(|p| rel_path == *p) {
            continue;
        }
        // The walk's own source contains literal needle text inside
        // string constants; reading it back through the scan would
        // self-flag every entry.
        if rel_path == "crates/loom-walk/src/walk/no_panics_in_production.rs" {
            continue;
        }
        let Some(body) = read_to_string(&path) else {
            continue;
        };
        let mask = cfg_test_mask(&body);
        for (idx, raw) in body.lines().enumerate() {
            if mask[idx] {
                continue;
            }
            if is_comment(raw) || is_attribute_line(raw) {
                continue;
            }
            for (needle, label) in &needles {
                if raw.contains(*needle) {
                    violations.push(format!(
                        "{}:{} `{}` — return a typed error instead",
                        rel_path,
                        idx + 1,
                        label,
                    ));
                    break;
                }
            }
        }
    }
    verdict_from(RULE, violations)
}

/// One bool per source line: `true` when the line falls inside a
/// `#[cfg(test)]` block (and therefore is exempt from the scan), `false`
/// otherwise. The mask is aligned with `source.lines()` so violation
/// evidence keeps original line numbers.
///
/// Handles the common shape where additional attribute lines (e.g.
/// `#[expect(clippy::expect_used, ...)]`) sit between `#[cfg(test)]`
/// and the opening `mod tests {` — those intermediate attributes don't
/// carry braces, so the scanner stays in the pending state until it
/// reaches the opening brace.
fn cfg_test_mask(source: &str) -> Vec<bool> {
    let mut mask = Vec::new();
    let mut depth: i32 = 0;
    let mut pending = false;
    for raw in source.lines() {
        if depth > 0 {
            depth += brace_delta(raw);
            mask.push(true);
            if depth <= 0 {
                depth = 0;
            }
            continue;
        }
        if pending {
            mask.push(true);
            let delta = brace_delta(raw);
            if delta > 0 {
                depth = delta;
                pending = false;
            }
            continue;
        }
        let trimmed = raw.trim_start();
        if trimmed.starts_with("#[cfg(test)]") {
            pending = true;
            mask.push(true);
            continue;
        }
        mask.push(false);
    }
    mask
}

fn brace_delta(line: &str) -> i32 {
    let mut delta = 0i32;
    for c in line.chars() {
        match c {
            '{' => delta += 1,
            '}' => delta -= 1,
            _ => {}
        }
    }
    delta
}

fn is_attribute_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    let prefix_expect = concat!("#[", "expect(");
    let prefix_allow = concat!("#[", "allow(");
    let prefix_warn = concat!("#[", "warn(");
    let prefix_deny = concat!("#[", "deny(");
    let prefix_forbid = concat!("#[", "forbid(");
    trimmed.starts_with(prefix_expect)
        || trimmed.starts_with(prefix_allow)
        || trimmed.starts_with(prefix_warn)
        || trimmed.starts_with(prefix_deny)
        || trimmed.starts_with(prefix_forbid)
}

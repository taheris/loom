//! Contract walker for rubric suppression configuration.
//!
//! Rubric suppressions are accepted from `loom.toml`'s top-level
//! `[[suppress]]` registry only. Production suppression handling must not
//! grow a language-specific source-comment directive path.

use std::path::{Path, PathBuf};

use super::util::{
    is_comment, narrow_to_loom_files, read_to_string, rel, rs_files_recursive, verdict_from,
    workspace_root,
};
use super::{Verdict, WalkInput};

const RULE: &str = "no_inline_suppression_comment_contract — rubric suppressions must come from top-level `[[suppress]]` entries in `loom.toml`, not source-code comments";

const SUPPRESSION_DIRECTIVE_TOKENS: &[&str] = &[
    "loom-suppress",
    "loom:suppress",
    "loom_suppress",
    "inline-suppress",
    "inline_suppress",
    "inline suppression",
    "comment-suppress",
    "comment_suppress",
    "comment suppression",
    "suppress-comment",
    "suppress_comment",
    "suppression-comment",
    "suppression_comment",
];

const COMMENT_SCANNER_TOKENS: &[&str] = &[
    "strip_prefix(\"//\")",
    "starts_with(\"//\")",
    "trim_start_matches(\"//\")",
    "trim_start().starts_with(\"//\")",
    "strip_prefix(\"/*\")",
    "starts_with(\"/*\")",
    "trim_start_matches(\"/*\")",
    "strip_prefix(\"#\")",
    "starts_with(\"#\")",
    "trim_start_matches(\"#\")",
];

pub fn run(input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let scope = narrow_to_loom_files(contract_scope(&root), input, &root);
    let mut violations = Vec::new();
    for path in scope {
        let Some(body) = read_to_string(&path) else {
            continue;
        };
        let rel_path = rel(&root, &path);
        let test_mask = cfg_test_mask(&body);
        for (lineno, line) in body.lines().enumerate() {
            if test_mask.get(lineno).copied().unwrap_or(false) || is_comment(line) {
                continue;
            }
            collect_directive_token_violations(&mut violations, &rel_path, lineno + 1, line);
            collect_comment_scanner_violations(&mut violations, &rel_path, lineno + 1, line);
        }
    }
    verdict_from(RULE, violations)
}

fn contract_scope(root: &Path) -> Vec<PathBuf> {
    let mut out = rs_files_recursive(&root.join("crates/loom-driver/src"));
    out.extend(rs_files_recursive(&root.join("crates/loom-workflow/src")));
    out
}

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
        if raw.trim_start().starts_with("#[cfg(test)]") {
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

fn collect_directive_token_violations(
    violations: &mut Vec<String>,
    rel_path: &str,
    line_no: usize,
    line: &str,
) {
    let lower = line.to_ascii_lowercase();
    for token in SUPPRESSION_DIRECTIVE_TOKENS {
        if lower.contains(token) {
            violations.push(format!(
                "{rel_path}:{line_no} contains inline suppression directive token `{token}`"
            ));
        }
    }
}

fn collect_comment_scanner_violations(
    violations: &mut Vec<String>,
    rel_path: &str,
    line_no: usize,
    line: &str,
) {
    let lower = line.to_ascii_lowercase();
    if !lower.contains("suppress") {
        return;
    }
    for token in COMMENT_SCANNER_TOKENS {
        if lower.contains(token) {
            violations.push(format!(
                "{rel_path}:{line_no} scans comment syntax while handling suppression via `{token}`"
            ));
        }
    }
}

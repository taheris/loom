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
    run_with_root(input, &workspace_root())
}

fn run_with_root(input: &WalkInput, root: &Path) -> Verdict {
    let scope = narrow_to_loom_files(contract_scope(root), input, root);
    let mut violations = Vec::new();
    for path in scope {
        let Some(body) = read_to_string(&path) else {
            continue;
        };
        let rel_path = rel(root, &path);
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
    let mut out = rs_files_recursive(&root.join("crates/loom/src"));
    out.extend(rs_files_recursive(&root.join("crates/loom-driver/src")));
    out.extend(rs_files_recursive(&root.join("crates/loom-workflow/src")));
    out
}

fn cfg_test_mask(source: &str) -> Vec<bool> {
    let mut mask = Vec::new();
    let mut state = CfgTestMask::Inactive;
    for raw in source.lines() {
        if let Some(suffix) = cfg_test_suffix(raw) {
            mask.push(true);
            state = state_after_test_item_line(suffix);
            continue;
        }
        match state {
            CfgTestMask::Inactive => mask.push(false),
            CfgTestMask::PendingItem => {
                mask.push(true);
                state = state_after_test_item_line(raw);
            }
            CfgTestMask::BracedItem(depth) => {
                mask.push(true);
                state = state_after_braced_item_line(depth, raw);
            }
        }
    }
    mask
}

#[derive(Clone, Copy)]
enum CfgTestMask {
    Inactive,
    PendingItem,
    BracedItem(i32),
}

fn cfg_test_suffix(line: &str) -> Option<&str> {
    line.trim_start().strip_prefix("#[cfg(test)]")
}

fn state_after_test_item_line(line: &str) -> CfgTestMask {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return CfgTestMask::PendingItem;
    }
    let delta = brace_delta(line);
    if delta > 0 {
        return CfgTestMask::BracedItem(delta);
    }
    if line.contains('{') || line.contains(';') {
        return CfgTestMask::Inactive;
    }
    CfgTestMask::PendingItem
}

fn state_after_braced_item_line(depth: i32, line: &str) -> CfgTestMask {
    let next = depth + brace_delta(line);
    if next > 0 {
        CfgTestMask::BracedItem(next)
    } else {
        CfgTestMask::Inactive
    }
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::fs;
    use std::path::Path;

    use super::*;

    fn write(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn fails_when_loom_cli_suppression_path_scans_source_comments() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("crates/loom/src/main.rs"),
            r#"
pub fn suppressed(lines: &[&str]) -> bool {
    lines.iter().any(|line| line.trim_start().starts_with("//") && line.contains("loom-suppress"))
}
"#,
        );

        let verdict = run_with_root(&WalkInput::default(), dir.path());

        assert!(!verdict.pass, "comment suppression scanner must fail");
        assert!(
            verdict.evidence.contains("crates/loom/src/main.rs"),
            "evidence names the live CLI suppression surface: {}",
            verdict.evidence,
        );
    }

    #[test]
    fn passes_for_top_level_loom_toml_suppression_registry() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("loom.toml"),
            r#"
[[suppress]]
id = "v1:criterion:verifier-too-narrow:gate#verifier-honesty"
reason = "fixture"
"#,
        );
        write(
            &dir.path().join("crates/loom/src/main.rs"),
            "pub fn configured_count(config: &Config) -> usize { config.suppress.len() }\n",
        );

        let verdict = run_with_root(&WalkInput::default(), dir.path());

        assert!(
            verdict.pass,
            "top-level loom.toml suppressions are supported"
        );
    }
}

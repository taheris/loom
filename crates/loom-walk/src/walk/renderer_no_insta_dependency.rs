//! Snapshot-discipline: the run-time renderer (`loom-render`) must not
//! depend on `insta`. Renderer output is a flexibility surface —
//! terminal tool-call lines, status colors, truncation evolve with
//! product feedback; pinning the layout in a snapshot would churn the
//! file on every cosmetic tweak. Renderer tests use substring +
//! structural assertions instead.
//!
//! The walk flags two things in `crates/loom-render/`:
//!
//! - An `insta` row in any `Cargo.toml` `[dependencies]` /
//!   `[dev-dependencies]` table.
//! - `insta::` paths or a `use insta…` import in any `.rs` file.

use std::path::{Path, PathBuf};

use super::util::{
    is_comment, narrow_to_loom_files, read_to_string, rel, verdict_from, workspace_root,
};
use super::{Verdict, WalkInput};

const RULE: &str =
    "renderer_no_insta_dependency — loom-render uses substring/structural assertions, not insta";

const RENDERER_DIR: &str = "crates/loom-render";

const PREFIX_USE_INSTA: &str = concat!("use ", "insta");
const PREFIX_USE_ROOT_INSTA: &str = concat!("use ::", "insta");
const PREFIX_EXTERN_INSTA: &str = concat!("extern crate ", "insta");
const NEEDLE_PATH: &str = concat!("insta", "::");

fn rule(input: &WalkInput) -> Verdict {
    let root = workspace_root(input);
    let scope = narrow_to_loom_files(paths(input, &root), input, &root);
    let mut violations = Vec::new();
    for path in scope {
        let rel_path = rel(&root, &path);
        let Some(body) = read_to_string(input, &path) else {
            continue;
        };
        if is_cargo_manifest(&path) {
            scan_manifest(&rel_path, &body, &mut violations);
        } else if is_rust_file(&path) {
            scan_rust(&rel_path, &body, &mut violations);
        }
    }
    verdict_from(RULE, violations)
}

pub(super) fn paths(input: &WalkInput, root: &Path) -> Vec<PathBuf> {
    let mut paths = renderer_paths(input, &root.join(RENDERER_DIR));
    let logging_test = root.join("crates/loom-driver/tests/logging.rs");
    if super::util::is_file(input, &logging_test) {
        paths.push(logging_test);
    }
    paths
}

fn renderer_paths(input: &WalkInput, renderer_root: &Path) -> Vec<PathBuf> {
    if !super::util::is_dir(input, renderer_root) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for p in super::util::files_recursive(input, renderer_root, None) {
        if is_rust_file(&p) || is_cargo_manifest(&p) {
            out.push(p);
        }
    }
    out.sort();
    out
}

fn is_rust_file(p: &Path) -> bool {
    p.extension().and_then(|s| s.to_str()) == Some("rs")
}

fn is_cargo_manifest(p: &Path) -> bool {
    p.file_name().and_then(|s| s.to_str()) == Some("Cargo.toml")
}

fn scan_rust(rel_path: &str, body: &str, violations: &mut Vec<String>) {
    for (lineno, line) in body.lines().enumerate() {
        if is_comment(line) {
            continue;
        }
        let trimmed = line.trim_start();
        let hit = trimmed.starts_with(PREFIX_USE_INSTA)
            || trimmed.starts_with(PREFIX_USE_ROOT_INSTA)
            || trimmed.starts_with(PREFIX_EXTERN_INSTA)
            || line.contains(NEEDLE_PATH);
        if hit {
            violations.push(format!(
                "{}:{} `insta` usage in renderer test — use substring + structural assertions instead",
                rel_path,
                lineno + 1,
            ));
        }
    }
}

fn scan_manifest(rel_path: &str, body: &str, violations: &mut Vec<String>) {
    let mut in_deps = false;
    for (lineno, raw) in body.lines().enumerate() {
        let line = raw.trim();
        if line.starts_with('[') {
            in_deps = matches!(
                line,
                "[dependencies]" | "[dev-dependencies]" | "[build-dependencies]"
            );
            continue;
        }
        if !in_deps {
            continue;
        }
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let key = line.split('=').next().unwrap_or("").trim();
        if key == "insta" {
            violations.push(format!(
                "{}:{} `insta` declared as a dependency of `loom-render` — renderer must not snapshot terminal output",
                rel_path,
                lineno + 1,
            ));
        }
    }
}

/// Standalone and checked dispatch execute the identical rule body.
pub fn run(input: &WalkInput) -> Verdict {
    input.finish(rule(input))
}

super::source::registration!("renderer_no_insta_dependency");

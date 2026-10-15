//! Shared scanning helpers for `[check]`-tier walks.
//!
//! The verifier-runner contract gives walks one [`WalkInput`]; this
//! module turns that into a concrete file set, parses Rust syntax, and
//! materialises [`Verdict`] envelopes from accumulated violation lines.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use syn::Attribute;

use super::{Verdict, WalkInput};

/// Standalone discovery is isolated from captured execution.
pub fn workspace_root(input: &WalkInput) -> PathBuf {
    if let Some(root) = &input.root {
        return root.clone();
    }
    if input.source.is_some() {
        return PathBuf::new();
    }
    super::source::workspace_root(input)
}

pub fn resolve_input_files(root: &Path, input: &WalkInput) -> Option<Vec<PathBuf>> {
    Some(
        input
            .files
            .as_ref()?
            .iter()
            .map(|p| {
                if p.is_absolute() {
                    p.clone()
                } else {
                    root.join(p)
                }
            })
            .collect(),
    )
}

pub fn src_files(input: &WalkInput, root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for crate_dir in immediate_children(input, &root.join("crates")) {
        let src = crate_dir.join("src");
        if is_dir(input, &src) {
            out.extend(rs_files_recursive(input, &src));
        }
    }
    out
}

pub fn test_files(input: &WalkInput, root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let self_test = root.join("crates/loom-walk/tests/fixture.rs");
    for crate_dir in immediate_children(input, &root.join("crates")) {
        let tests = crate_dir.join("tests");
        if is_dir(input, &tests) {
            out.extend(rs_files_recursive_except(input, &tests, &self_test));
        }
    }
    out
}

pub fn all_rs_files(input: &WalkInput, root: &Path) -> Vec<PathBuf> {
    let mut out = src_files(input, root);
    out.extend(test_files(input, root));
    out
}

pub fn all_rs_files_including_verifiers(input: &WalkInput, root: &Path) -> Vec<PathBuf> {
    let mut out = all_rs_files(input, root);
    let path = root.join("crates/loom-walk/tests/fixture.rs");
    if is_file(input, &path) {
        out.push(path);
    }
    out
}

pub fn narrow_to_loom_files(scope: Vec<PathBuf>, input: &WalkInput, root: &Path) -> Vec<PathBuf> {
    let Some(filter) = resolve_input_files(root, input) else {
        return scope;
    };
    let normalize = |path: &Path| {
        input
            .record(path, input.reader().normalize(path))
            .unwrap_or_else(|| path.to_path_buf())
    };
    let filter: HashSet<_> = filter.iter().map(|path| normalize(path)).collect();
    scope
        .into_iter()
        .filter(|path| filter.contains(&normalize(path)))
        .collect()
}

/// Errors are retained even when a rule continues after a failed read.
pub fn read_to_string(input: &WalkInput, path: &Path) -> Option<String> {
    input.record(path, input.reader().read(path))
}

pub fn parse_rs(input: &WalkInput, path: &Path) -> Option<syn::File> {
    let body = read_to_string(input, path)?;
    input.record(path, syn::parse_file(&body).map_err(std::io::Error::other))
}

pub fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

pub fn immediate_children(input: &WalkInput, dir: &Path) -> Vec<PathBuf> {
    input
        .record(dir, input.reader().directory(dir))
        .unwrap_or_default()
}

pub fn files_recursive(input: &WalkInput, dir: &Path, extension: Option<&str>) -> Vec<PathBuf> {
    input
        .record(dir, input.reader().scan(dir, extension))
        .unwrap_or_default()
}

pub fn rs_files_recursive(input: &WalkInput, dir: &Path) -> Vec<PathBuf> {
    files_recursive(input, dir, Some("rs"))
}

/// Exclude a namespace prefix before probing files, rather than scanning an
/// undeclared file and dropping it after an authorization error.
pub fn rs_files_recursive_except(input: &WalkInput, dir: &Path, excluded: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if !is_dir(input, dir) {
        return files;
    }
    for path in immediate_children(input, dir) {
        if path
            .to_string_lossy()
            .starts_with(excluded.to_string_lossy().as_ref())
        {
            continue;
        }
        if is_dir(input, &path) {
            files.extend(rs_files_recursive_except(input, &path, excluded));
        } else if path.extension().is_some_and(|ext| ext == "rs") && is_file(input, &path) {
            files.push(path);
        }
    }
    files
}

pub fn is_file(input: &WalkInput, path: &Path) -> bool {
    input
        .record(path, input.reader().is_file(path))
        .unwrap_or(false)
}

pub fn is_dir(input: &WalkInput, path: &Path) -> bool {
    input
        .record(path, input.reader().is_dir(path))
        .unwrap_or(false)
}

/// `true` when the trimmed line begins with a Rust comment marker.
/// Walks use this to avoid flagging `// foo` literals on their own
/// banned-pattern check.
pub fn is_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with("/*")
}

/// Line number a syn AST node begins on.
pub fn line_of<T: syn::spanned::Spanned>(node: &T) -> usize {
    node.span().start().line
}

/// Idents listed inside a `derive(...)` attribute.
pub fn derive_idents(attr: &Attribute) -> Vec<String> {
    let mut out = Vec::new();
    let _ = attr.parse_nested_meta(|meta| {
        if let Some(ident) = meta.path.get_ident() {
            out.push(ident.to_string());
        }
        Ok(())
    });
    out
}

/// Build a passing verdict with a brief description of what was
/// checked (kept for parity with failure paths).
pub fn pass(description: &str) -> Verdict {
    Verdict {
        pass: true,
        evidence: description.to_string(),
    }
}

/// Build a failing verdict whose `evidence` is the joined violation
/// lines plus the rule citation. Returns a passing verdict when
/// `violations` is empty.
#[expect(
    clippy::needless_pass_by_value,
    reason = "the completed violation buffer is handed off by 99 callers and consumed here as the terminal verdict"
)]
pub fn verdict_from(rule: &str, violations: Vec<String>) -> Verdict {
    if violations.is_empty() {
        return pass(rule);
    }
    let joined = violations.join("\n");
    Verdict {
        pass: false,
        evidence: format!("{joined}\n{rule}"),
    }
}

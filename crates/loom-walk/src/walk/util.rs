//! Shared scanning helpers for `[check]`-tier walks.
//!
//! The verifier-runner contract gives walks one [`WalkInput`]; this
//! module turns that into a concrete file set, parses Rust syntax, and
//! materialises [`Verdict`] envelopes from accumulated violation lines.

use std::path::{Path, PathBuf};

use syn::Attribute;
use walkdir::WalkDir;

use super::{Verdict, WalkInput};

/// Locate the cargo workspace this binary was invoked against by
/// walking up from the current directory looking for a `Cargo.toml`
/// whose `[workspace]` table lists `crates/loom-driver` as a member.
/// Returns the CWD itself when no marker matches — walks that scan
/// absolute paths via `LOOM_FILES` don't depend on the result.
pub fn workspace_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for ancestor in cwd.ancestors() {
        let manifest = ancestor.join("Cargo.toml");
        let Ok(body) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        if body.contains("[workspace]") && body.contains("crates/loom-driver") {
            return ancestor.to_path_buf();
        }
    }
    cwd
}

/// Convert the walk's input file set into a vector of absolute paths.
/// `WalkInput::files` may contain repo-relative paths (the gate's
/// `--files` scope produces them) — they're joined against `root` so
/// downstream code can read them without further normalisation.
pub fn resolve_input_files(root: &Path, input: &WalkInput) -> Option<Vec<PathBuf>> {
    let files = input.files.as_ref()?;
    let resolved = files
        .iter()
        .map(|p| {
            if p.is_absolute() {
                p.clone()
            } else {
                root.join(p)
            }
        })
        .collect();
    Some(resolved)
}

/// Production source files under `crates/*/src/**/*.rs`. The
/// generic default when `LOOM_FILES` is unset and the walk targets
/// production code.
pub fn src_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for crate_dir in immediate_children(&root.join("crates")) {
        let src = crate_dir.join("src");
        if !src.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&src).into_iter().filter_map(Result::ok) {
            let p = entry.path();
            if is_rust_file(p) {
                out.push(p.to_path_buf());
            }
        }
    }
    out
}

/// Test files under `crates/*/tests/**/*.rs`, excluding the
/// `tests/fixture.rs` of `loom-walk` itself (pattern-matches its own
/// rules under synthetic fixtures so a self-walk would yield false
/// positives).
pub fn test_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let self_test = root.join("crates/loom-walk/tests/fixture.rs");
    let legacy_self_test = root.join("crates/loom/tests/style.rs");
    for crate_dir in immediate_children(&root.join("crates")) {
        let tests = crate_dir.join("tests");
        if !tests.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&tests).into_iter().filter_map(Result::ok) {
            let p = entry.path();
            if is_rust_file(p) && p != self_test && p != legacy_self_test {
                out.push(p.to_path_buf());
            }
        }
    }
    out
}

/// Union of [`src_files`] and [`test_files`].
pub fn all_rs_files(root: &Path) -> Vec<PathBuf> {
    let mut out = src_files(root);
    out.extend(test_files(root));
    out
}

/// Production and test Rust files, including verifier fixtures.
pub fn all_rs_files_including_verifiers(root: &Path) -> Vec<PathBuf> {
    let mut out = all_rs_files(root);
    for path in [
        root.join("crates/loom-walk/tests/fixture.rs"),
        root.join("crates/loom/tests/style.rs"),
    ] {
        if path.is_file() {
            out.push(path);
        }
    }
    out
}

/// Take the walk's chosen scope (e.g. [`src_files`]) and narrow it to
/// the `LOOM_FILES` set when one is present. Caller-supplied paths
/// outside the scope are silently dropped — the gate decides what to
/// surface; the walk only reports on its own jurisdiction.
pub fn narrow_to_loom_files(scope: Vec<PathBuf>, input: &WalkInput, root: &Path) -> Vec<PathBuf> {
    let Some(filter) = resolve_input_files(root, input) else {
        return scope;
    };
    scope
        .into_iter()
        .filter(|p| filter.iter().any(|q| q == p))
        .collect()
}

/// Eagerly read a path into a string. Returns `None` for unreadable
/// paths so a walk can skip rather than panic on a missing file.
pub fn read_to_string(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Parse a Rust source file. Returns `None` on parse failures so the
/// walk surfaces the path/line in `Verdict::evidence` rather than
/// aborting the whole scan.
pub fn parse_rs(path: &Path) -> Option<syn::File> {
    let body = read_to_string(path)?;
    syn::parse_file(&body).ok()
}

/// Workspace-relative form of `path`. Used to keep `Verdict::evidence`
/// portable across machines.
pub fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// Immediate child entries of a directory, sorted lexicographically so
/// walks emit deterministic violation orderings across runs.
pub fn immediate_children(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            out.push(entry.path());
        }
    }
    out.sort();
    out
}

/// `.rs` files directly under `dir` (no recursion).
pub fn rs_files_in(dir: &Path) -> Vec<PathBuf> {
    immediate_children(dir)
        .into_iter()
        .filter(|p| p.is_file() && is_rust_file(p))
        .collect()
}

/// `.rs` files anywhere under `dir`.
pub fn rs_files_recursive(dir: &Path) -> Vec<PathBuf> {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.path().is_file() && is_rust_file(e.path()))
        .map(|e| e.path().to_path_buf())
        .collect()
}

fn is_rust_file(p: &Path) -> bool {
    p.extension().and_then(|s| s.to_str()) == Some("rs")
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

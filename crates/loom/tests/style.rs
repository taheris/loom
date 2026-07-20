//! AST + filesystem style enforcement for the Loom workspace.
//!
//! These checks cover the architectural rules from `specs/tests.md` that
//! clippy can't express. Each rule is one `#[test]`; failures print one line
//! per violation in `<path>:<line> <rule>` form so reviewers can click into
//! the offending site.
//!
//! Implementation strategy:
//! - `syn` parses `.rs` files when the rule needs AST shape (newtype layout,
//!   `derive(From)/derive(Into)` detection, template-struct pairing,
//!   `LitStr` value inspection).
//! - `walkdir` enumerates directories when the rule is purely structural
//!   (forbidden file names, template fan-out).
//! - Text scans cover line-precise grep-like rules (banned time patterns,
//!   `gix::` / `Command::new("git")` import sites, `#[ignore]` allowlist)
//!   where AST lookups would be overkill but line numbers still matter.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use syn::visit::Visit;
use syn::{Attribute, Expr, ExprLit, Fields, ItemStruct, Lit, Meta};
use walkdir::WalkDir;

// ---------------------------------------------------------------------------
// Existing rule: renderer test files do not depend on `insta` (NFR §Snapshot
// Testing — the renderer is a flexibility surface; substring + structural
// assertions are the contract, not snapshot diffs).
// ---------------------------------------------------------------------------

const RENDERER_TEST_FILES: &[&str] = &["crates/loom-driver/tests/logging.rs"];

#[test]
fn renderer_no_insta_dependency() {
    let root = loom_workspace_root();
    let mut violations: Vec<String> = Vec::new();
    for rel_path in RENDERER_TEST_FILES {
        let path = root.join(rel_path);
        let body = read_to_string(&path);
        for (lineno, line) in body.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("///") {
                continue;
            }
            if line.contains("insta::") || line.contains("use insta") {
                violations.push(format!("{}:{}: {}", rel_path, lineno + 1, line));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "renderer test files must not depend on `insta` (use substring + \
         structural assertions instead — see specs/tests.md \
         §Snapshot Testing). Violations:\n{}",
        violations.join("\n"),
    );
}

// ---------------------------------------------------------------------------
// Rule: no `derive(From)` or `derive(Into)` on tuple-struct newtypes in
// `crates/*/src/**/*.rs`. Newtypes must enter via their `new()`
// constructor so per-identifier validation is enforced at the boundary.
// ---------------------------------------------------------------------------

#[test]
fn no_derive_from_on_newtypes() {
    let root = loom_workspace_root();
    let mut violations: Vec<String> = Vec::new();
    for path in src_files(&root) {
        let file = parse_rs(&path);
        let mut visitor = NewtypeDeriveVisitor {
            violations: &mut violations,
            rel_path: rel(&root, &path),
        };
        visitor.visit_file(&file);
    }
    assert_violations(
        "no `derive(From)` / `derive(Into)` on tuple-struct newtypes — \
         values must enter via the newtype's `new()` so parse rules are \
         enforced (specs/tests.md §Style Enforcement)",
        &violations,
    );
}

struct NewtypeDeriveVisitor<'a> {
    violations: &'a mut Vec<String>,
    rel_path: String,
}

impl<'ast> Visit<'ast> for NewtypeDeriveVisitor<'_> {
    fn visit_item_struct(&mut self, node: &'ast ItemStruct) {
        if !matches!(node.fields, Fields::Unnamed(_)) {
            return;
        }
        for attr in &node.attrs {
            if !attr.path().is_ident("derive") {
                continue;
            }
            let derived = derive_idents(attr);
            for forbidden in ["From", "Into"] {
                if derived.iter().any(|i| i == forbidden) {
                    let line = line_of(attr);
                    self.violations.push(format!(
                        "{}:{} derive({forbidden}) on tuple struct `{}`",
                        self.rel_path, line, node.ident,
                    ));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Rule: no `crates/*/src/{types,error}.rs` files at crate roots. Each
// crate uses nested `domain/{type,error}.rs` modules so the family layout
// stays additive (NF-5).
// ---------------------------------------------------------------------------

#[test]
fn nested_module_structure() {
    let root = loom_workspace_root();
    let crates_dir = root.join("crates");
    let mut violations: Vec<String> = Vec::new();
    for crate_dir in immediate_children(&crates_dir) {
        let src = crate_dir.join("src");
        for forbidden in ["types.rs", "error.rs"] {
            let candidate = src.join(forbidden);
            if candidate.is_file() {
                violations.push(format!(
                    "{}:1 forbidden central `{}` at crate root — split into \
                     nested `<domain>/{}` modules",
                    rel(&root, &candidate),
                    forbidden,
                    forbidden,
                ));
            }
        }
    }
    assert_violations(
        "no `types.rs` / `error.rs` files directly under \
         `crates/*/src/` (NF-5: nested module structure)",
        &violations,
    );
}

// ---------------------------------------------------------------------------
// Rule: `gix::*` and `Command::new("git")` only inside `loom-driver/src/git/`.
// Production code outside that module must reach git through `GitClient`.
// Tests are exempted: `git_client.rs` and `parallel.rs` seed real repos by
// invoking the system `git` binary, which is the only way to assert against
// real refs/index/merge state (specs/tests.md NFR #8).
// ---------------------------------------------------------------------------

#[test]
fn git_client_encapsulation() {
    let root = loom_workspace_root();
    let allowed_prefix = Path::new("crates/loom-driver/src/git/");
    let mut violations: Vec<String> = Vec::new();
    for path in src_files(&root) {
        let rel_path = rel(&root, &path);
        if Path::new(&rel_path).starts_with(allowed_prefix) {
            continue;
        }
        let body = read_to_string(&path);
        for (lineno, line) in body.lines().enumerate() {
            if is_comment(line) {
                continue;
            }
            if line.contains("Command::new(\"git\")") {
                violations.push(format!(
                    "{}:{} `Command::new(\"git\")` — route through \
                     `loom_driver::git::GitClient`",
                    rel_path,
                    lineno + 1,
                ));
            }
            if has_gix_import(line) {
                violations.push(format!(
                    "{}:{} `gix` import — only `loom-driver/src/git/` may \
                     depend on `gix`",
                    rel_path,
                    lineno + 1,
                ));
            }
        }
    }
    assert_violations(
        "GitClient is the only module that imports `gix` or invokes the \
         `git` CLI (specs/tests.md §Architecture / Style Enforcement)",
        &violations,
    );
}

fn has_gix_import(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("use gix")
        || trimmed.starts_with("use ::gix")
        || trimmed.starts_with("extern crate gix")
}

// ---------------------------------------------------------------------------
// Rule: renderer + log writer share one event source — i.e., there is a
// single tee-style sink (`LogSink`) whose `emit` fans out to BOTH the
// on-disk file writer AND the terminal renderer. The current architecture
// satisfies this by construction (no broadcast/mpsc fan-out task — emit()
// drives both writers in the same call). This test pins that invariant by
// asserting `LogSink::emit` references both `self.file` and the renderer
// in one method body.
// ---------------------------------------------------------------------------

#[test]
fn run_single_event_channel() {
    let root = loom_workspace_root();
    let sink_path = root.join("crates/loom-render/src/sink/mod.rs");
    let source = read_to_string(&sink_path);
    let rel_path = rel(&root, &sink_path);
    let file =
        syn::parse_file(&source).unwrap_or_else(|e| panic!("parse {}: {e}", sink_path.display()));

    let mut found_emit = false;
    let mut writes_file = false;
    let mut drives_renderer = false;

    for item in &file.items {
        let syn::Item::Impl(item_impl) = item else {
            continue;
        };
        let syn::Type::Path(type_path) = &*item_impl.self_ty else {
            continue;
        };
        let Some(seg) = type_path.path.segments.last() else {
            continue;
        };
        if seg.ident != "LogSink" {
            continue;
        }
        for impl_item in &item_impl.items {
            let syn::ImplItem::Fn(method) = impl_item else {
                continue;
            };
            if method.sig.ident != "emit" {
                continue;
            }
            found_emit = true;
            let block_text = source_slice(&source, &method.block);
            if block_text.contains("self.file") {
                writes_file = true;
            }
            if block_text.contains("renderer") {
                drives_renderer = true;
            }
        }
    }

    let mut violations: Vec<String> = Vec::new();
    if !found_emit {
        violations.push(format!(
            "{}:1 LogSink::emit method not found — the tee-style sink is \
             the single subscriber that both the renderer and the on-disk \
             log share",
            rel_path
        ));
    }
    if found_emit && !writes_file {
        violations.push(format!(
            "{}:1 LogSink::emit must write to `self.file` so the on-disk \
             log subscribes to the same event call as the renderer",
            rel_path
        ));
    }
    if found_emit && !drives_renderer {
        violations.push(format!(
            "{}:1 LogSink::emit must drive the renderer in the same call \
             as the on-disk write — fan-out from a separate task would \
             create two independent subscribers",
            rel_path
        ));
    }
    assert_violations(
        "renderer + log writer subscribe to a single event source via \
         `LogSink::emit` (specs/tests.md §Run Logger)",
        &violations,
    );
}

/// Slice the source covered by `node`'s span. Used in lieu of pulling in
/// `quote`/`proc-macro2` for a token-stream round-trip.
fn source_slice<T: syn::spanned::Spanned>(source: &str, node: &T) -> String {
    let span = node.span();
    let start = span.start();
    let end = span.end();
    let mut out = String::new();
    for (idx, line) in source.lines().enumerate() {
        let lineno = idx + 1;
        if lineno < start.line || lineno > end.line {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// Rule: domain identifiers in `loom-driver/src/identifier/` are tuple-struct
// newtypes wrapping a single inner field (no record structs, no tuple
// structs with multiple fields). Error structs (`*Error`) are exempted
// because the identifier crate also defines `Parse*Error` payload structs.
// ---------------------------------------------------------------------------

#[test]
fn newtypes_for_identifiers() {
    let root = loom_workspace_root();
    let dir = root.join("crates/loom-driver/src/identifier");
    let mut violations: Vec<String> = Vec::new();
    for path in rs_files_in(&dir) {
        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if file_name == "mod.rs" {
            continue;
        }
        let parsed = parse_rs(&path);
        let rel_path = rel(&root, &path);
        for item in &parsed.items {
            let syn::Item::Struct(s) = item else { continue };
            if s.ident.to_string().ends_with("Error") {
                continue;
            }
            let line = line_of(&s.ident);
            match &s.fields {
                Fields::Unnamed(unnamed) if unnamed.unnamed.len() == 1 => {}
                _ => violations.push(format!(
                    "{}:{} identifier `{}` must be a single-field tuple \
                     struct newtype",
                    rel_path, line, s.ident,
                )),
            }
        }
    }
    assert_violations(
        "domain identifiers under `loom-driver/src/identifier/` are \
         tuple-struct newtypes (specs/tests.md §Style Enforcement)",
        &violations,
    );
}

// ---------------------------------------------------------------------------
// Rule: every Askama template under `loom-templates/templates/` (excluding
// `partial/` includes) has a typed `#[derive(Template)]` context struct in
// `loom-templates/src/`. A missing pairing means the template renders with
// untyped values — defeating the compile-time field check Askama provides.
// ---------------------------------------------------------------------------

#[test]
fn template_context_structs() {
    let root = loom_workspace_root();
    let templates_dir = root.join("crates/loom-templates/templates");
    let src_dir = root.join("crates/loom-templates/src");
    let template_files = top_level_templates(&templates_dir);
    let context_paths = collect_template_context_paths(&src_dir);

    let mut violations: Vec<String> = Vec::new();
    for tpl in &template_files {
        let name = tpl
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        if !context_paths.contains(&name) {
            violations.push(format!(
                "{}:1 template `{}` has no `#[derive(Template)] \
                 #[template(path = \"{}\")]` context struct in \
                 loom-templates/src/",
                rel(&root, tpl),
                name,
                name,
            ));
        }
    }
    assert_violations(
        "every Askama template has a typed context struct so missing \
         fields surface at compile time (specs/tests.md \
         §Style Enforcement)",
        &violations,
    );
}

fn top_level_templates(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in immediate_children(dir) {
        if entry.is_file() && entry.extension().and_then(|s| s.to_str()) == Some("md") {
            out.push(entry);
        }
    }
    out
}

fn collect_template_context_paths(src_dir: &Path) -> std::collections::HashSet<String> {
    let mut paths = std::collections::HashSet::new();
    for path in rs_files_recursive(src_dir) {
        let parsed = parse_rs(&path);
        for item in &parsed.items {
            let syn::Item::Struct(s) = item else { continue };
            let has_derive_template = s.attrs.iter().any(|a| {
                a.path().is_ident("derive") && derive_idents(a).iter().any(|i| i == "Template")
            });
            if !has_derive_template {
                continue;
            }
            for attr in &s.attrs {
                if !attr.path().is_ident("template") {
                    continue;
                }
                if let Some(p) = template_path_attr(attr) {
                    paths.insert(p);
                }
            }
        }
    }
    paths
}

fn template_path_attr(attr: &Attribute) -> Option<String> {
    let Meta::List(list) = &attr.meta else {
        return None;
    };
    let parsed: syn::punctuated::Punctuated<Meta, syn::Token![,]> = list
        .parse_args_with(syn::punctuated::Punctuated::parse_terminated)
        .ok()?;
    for nested in parsed {
        let Meta::NameValue(nv) = nested else {
            continue;
        };
        if !nv.path.is_ident("path") {
            continue;
        }
        let Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) = nv.value
        else {
            continue;
        };
        return Some(s.value());
    }
    None
}

// ---------------------------------------------------------------------------
// Rule: tests under `crates/*/tests/**/*.rs` and `#[cfg(test)] mod`
// blocks use `tempfile::tempdir`, never hardcoded `/tmp/...` paths. Nix's
// Darwin build sandbox doesn't grant access to the host's `/tmp`, so any
// test that hardcodes a path under it fails to even start under
// `nix flake check`.
// ---------------------------------------------------------------------------

#[test]
fn no_hardcoded_tmp_paths() {
    let root = loom_workspace_root();
    let mut violations: Vec<String> = Vec::new();
    for path in test_files_and_cfg_test_blocks(&root) {
        let file = parse_rs(&path);
        let mut visitor = TmpPathVisitor {
            violations: &mut violations,
            rel_path: rel(&root, &path),
            inside_test_module: path.components().any(|c| c.as_os_str() == "tests"),
        };
        visitor.visit_file(&file);
    }
    assert_violations(
        "tests must use `tempfile::tempdir` instead of hardcoded \
         `/tmp/...` paths (Nix Darwin sandbox forbids `/tmp` access)",
        &violations,
    );
}

struct TmpPathVisitor<'a> {
    violations: &'a mut Vec<String>,
    rel_path: String,
    inside_test_module: bool,
}

impl<'ast> Visit<'ast> for TmpPathVisitor<'_> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let was_in_test = self.inside_test_module;
        if has_cfg_test(&node.attrs) {
            self.inside_test_module = true;
        }
        syn::visit::visit_item_mod(self, node);
        self.inside_test_module = was_in_test;
    }

    fn visit_lit_str(&mut self, node: &'ast syn::LitStr) {
        if !self.inside_test_module {
            return;
        }
        let value = node.value();
        // Skip embedded JSON / multi-line bodies — only flag standalone
        // path-shaped literals that begin with `/tmp/`.
        if !is_tmp_path_literal(&value) {
            return;
        }
        let line = node.span().start().line;
        self.violations.push(format!(
            "{}:{} hardcoded `/tmp/` path literal `{}` — use \
             `tempfile::tempdir()` instead",
            self.rel_path, line, value
        ));
    }
}

fn is_tmp_path_literal(value: &str) -> bool {
    if !value.starts_with("/tmp/") && value != "/tmp" {
        return false;
    }
    // Skip composite payloads that happen to embed `/tmp/` somewhere
    // inside a larger blob (JSON test fixtures, multi-line strings).
    !value.contains('{') && !value.contains('\n')
}

fn has_cfg_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        let Meta::List(list) = &a.meta else {
            return false;
        };
        if !list.path.is_ident("cfg") {
            return false;
        }
        list.tokens.to_string().contains("test")
    })
}

// ---------------------------------------------------------------------------
// Rule RS-12: `EventEnvelope`, `BeadId`, and `AgentEvent` carry no
// `placeholder()` / `default()` constructor. The parser-to-stamper
// split moved unstamped events to the sibling `ParsedAgentEvent` type,
// and the session layer is the only constructor of `AgentEvent` (via
// `AgentEvent::from_parsed`). Re-introducing a sentinel constructor
// would reopen the "sentinel `wx-pending` bead id slipping into
// production logs" regression class. The compiler enforces the
// structural rule; this test guards against the named constructors
// getting added back.
//
// Also bans `EventEnvelope::default()` (RS-13) — `impl Default`
// requires callers to overwrite before use, which is the same
// sentinel-by-another-name pattern.
// ---------------------------------------------------------------------------

#[test]
fn no_envelope_default_outside_parser() {
    let root = loom_workspace_root();
    let mut violations: Vec<String> = Vec::new();
    for path in src_files(&root) {
        let rel_path = rel(&root, &path);
        let body = read_to_string(&path);
        for (lineno, line) in body.lines().enumerate() {
            if is_comment(line) {
                continue;
            }
            let line_no = lineno + 1;
            if line.contains("EventEnvelope::placeholder(")
                || line.contains("BeadId::placeholder(")
                || line.contains("AgentEvent::placeholder(")
            {
                violations.push(format!(
                    "{}:{} `*::placeholder()` — RS-12 removed sentinel \
                     constructors; parser emits `ParsedAgentEvent` and \
                     session layer joins via `AgentEvent::from_parsed`",
                    rel_path, line_no,
                ));
            }
            if line.contains("EventEnvelope::default(") {
                violations.push(format!(
                    "{}:{} `EventEnvelope::default()` — `impl Default` was \
                     removed (RS-13); construct an `EventEnvelope` via \
                     `EnvelopeBuilder` or struct-literal in test fixtures",
                    rel_path, line_no,
                ));
            }
        }
    }
    assert_violations(
        "no `*::placeholder()` constructors on event types (RS-12) and \
         no `EventEnvelope::default()` (RS-13)",
        &violations,
    );
}

// ---------------------------------------------------------------------------
// Rule: `#[ignore]` cannot hide flaky, optional, or omitted coverage. The only
// exceptions are child-process entry points invoked by a non-ignored parent
// during the ordinary suite because process isolation is part of the behavior.
//
// The allowlist names every exception. Each entry requires a process-boundary
// justification; every unenumerated marker fails the audit.
// ---------------------------------------------------------------------------

const IGNORE_ALLOWLIST: &[(&str, &str)] = &[
    // Process boundary: the parent must outlive this re-executed child to
    // observe the kernel releasing its flock when the child exits.
    (
        "crates/loom-driver/tests/lock_manager.rs",
        "crash_helper_take_lock_then_exit",
    ),
];

#[test]
fn no_ignore_for_flake() {
    let root = loom_workspace_root();
    let mut violations: Vec<String> = Vec::new();
    for path in all_rs_files(&root) {
        let parsed = parse_rs(&path);
        let rel_path = rel(&root, &path);
        let mut visitor = IgnoreVisitor {
            violations: &mut violations,
            rel_path: rel_path.clone(),
        };
        visitor.visit_file(&parsed);
    }
    assert_violations(
        "no unaudited `#[ignore]` markers (specs/tests.md §Determinism: \
         fix or delete skipped coverage; enumerate process helpers)",
        &violations,
    );
}

struct IgnoreVisitor<'a> {
    violations: &'a mut Vec<String>,
    rel_path: String,
}

impl<'ast> Visit<'ast> for IgnoreVisitor<'_> {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if has_ignore_attr(&node.attrs) {
            let fn_name = node.sig.ident.to_string();
            let allowed = IGNORE_ALLOWLIST
                .iter()
                .any(|(p, n)| *p == self.rel_path && *n == fn_name);
            if !allowed {
                let line = line_of(&node.sig.ident);
                self.violations.push(format!(
                    "{}:{} `#[ignore]` on `{}` — fix the test or delete it",
                    self.rel_path, line, fn_name
                ));
            }
        }
        syn::visit::visit_item_fn(self, node);
    }
}

fn has_ignore_attr(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("ignore"))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn loom_workspace_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .expect("workspace root above crates/loom")
}

fn read_to_string(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn parse_rs(path: &Path) -> syn::File {
    let body = read_to_string(path);
    syn::parse_file(&body).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// Production source files: `crates/*/src/**/*.rs`.
fn src_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for crate_dir in immediate_children(&root.join("crates")) {
        let src = crate_dir.join("src");
        if !src.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&src).into_iter().filter_map(Result::ok) {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("rs") && p.is_file() {
                out.push(p.to_path_buf());
            }
        }
    }
    out
}

/// Test files: `crates/*/tests/**/*.rs`. The lint file itself (this
/// `style.rs`) is excluded — it pattern-matches on the very rules it
/// enforces, so a self-walk would yield false positives.
fn test_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let self_path = root.join("crates/loom/tests/style.rs");
    for crate_dir in immediate_children(&root.join("crates")) {
        let tests = crate_dir.join("tests");
        if !tests.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&tests).into_iter().filter_map(Result::ok) {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("rs")
                && p.is_file()
                && p != self_path.as_path()
            {
                out.push(p.to_path_buf());
            }
        }
    }
    out
}

/// Test files plus production source files (for visiting `#[cfg(test)]` blocks).
fn test_files_and_cfg_test_blocks(root: &Path) -> Vec<PathBuf> {
    let mut out = test_files(root);
    out.extend(src_files(root));
    out
}

fn all_rs_files(root: &Path) -> Vec<PathBuf> {
    let mut out = src_files(root);
    out.extend(test_files(root));
    out
}

fn immediate_children(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            out.push(entry.path());
        }
    }
    out.sort();
    out
}

fn rs_files_in(dir: &Path) -> Vec<PathBuf> {
    immediate_children(dir)
        .into_iter()
        .filter(|p| p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("rs"))
        .collect()
}

fn rs_files_recursive(dir: &Path) -> Vec<PathBuf> {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| {
            e.path().is_file() && e.path().extension().and_then(|s| s.to_str()) == Some("rs")
        })
        .map(|e| e.path().to_path_buf())
        .collect()
}

fn is_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//") || trimmed.starts_with("*") || trimmed.starts_with("/*")
}

fn line_of<T: syn::spanned::Spanned>(node: &T) -> usize {
    node.span().start().line
}

fn derive_idents(attr: &Attribute) -> Vec<String> {
    let mut out = Vec::new();
    let _ = attr.parse_nested_meta(|meta| {
        if let Some(ident) = meta.path.get_ident() {
            out.push(ident.to_string());
        }
        Ok(())
    });
    out
}

fn assert_violations(rule: &str, violations: &[String]) {
    assert!(
        violations.is_empty(),
        "{rule}\nViolations:\n{}",
        violations.join("\n")
    );
}

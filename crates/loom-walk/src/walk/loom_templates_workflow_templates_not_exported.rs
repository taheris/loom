//! Loom's own workflow template bodies (`plan_new.md`, `plan_update.md`,
//! `todo_new.md`, `todo_update.md`, `run.md`, `review.md`, `msg.md`) are
//! internal — consumers compose with the partials and typed contexts, not
//! with the workflow shells. The walk asserts no `pub const … =
//! include_str!("…/<workflow>.md")` (or `&'static str`) exists in
//! `crates/loom-templates/src/`. `#[derive(Template)]` attribute use of
//! these same template paths is unaffected — only `pub const` export
//! is flagged.

use std::path::Path;

use proc_macro2::TokenTree;

use super::util::{parse_rs, rs_files_recursive, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "loom_templates_workflow_templates_not_exported — workflow template bodies are not re-exported as `pub const` strings";

const SRC_DIR: &str = "crates/loom-templates/src";

const WORKFLOW_TEMPLATES: &[&str] = &[
    "plan_new.md",
    "plan_update.md",
    "todo_new.md",
    "todo_update.md",
    "run.md",
    "review.md",
    "msg.md",
];

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let src_dir = root.join(SRC_DIR);

    let mut violations = Vec::new();
    for path in rs_files_recursive(&src_dir) {
        scan_file(&root, &path, &mut violations);
    }
    verdict_from(RULE, violations)
}

fn scan_file(root: &Path, path: &Path, violations: &mut Vec<String>) {
    let Some(file) = parse_rs(path) else {
        return;
    };
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();
    scan_items(&rel, &file.items, violations);
}

fn scan_items(rel: &str, items: &[syn::Item], violations: &mut Vec<String>) {
    for item in items {
        match item {
            syn::Item::Const(c) if matches!(c.vis, syn::Visibility::Public(_)) => {
                if let Some(path) = include_str_arg(&c.expr)
                    && let Some(workflow) = matches_workflow_template(&path)
                {
                    let lineno = const_line(c);
                    violations.push(format!(
                        "{rel}:{lineno} `pub const {ident}` re-exports workflow template `{workflow}` via `include_str!(\"{path}\")` — workflow templates are internal",
                        ident = c.ident,
                    ));
                }
            }
            syn::Item::Mod(m) => {
                if let Some((_, nested)) = &m.content {
                    scan_items(rel, nested, violations);
                }
            }
            _ => {}
        }
    }
}

fn matches_workflow_template(path: &str) -> Option<&'static str> {
    for workflow in WORKFLOW_TEMPLATES {
        let suffix = format!("templates/{workflow}");
        if path.ends_with(&suffix) && !path.contains("partial/") {
            return Some(workflow);
        }
    }
    None
}

fn include_str_arg(expr: &syn::Expr) -> Option<String> {
    let syn::Expr::Macro(m) = expr else {
        return None;
    };
    let last = m.mac.path.segments.last()?;
    if last.ident != "include_str" {
        return None;
    }
    for tok in m.mac.tokens.clone() {
        if let TokenTree::Literal(lit) = tok {
            let raw = lit.to_string();
            if let Some(s) = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn const_line(c: &syn::ItemConst) -> usize {
    use syn::spanned::Spanned;
    c.span().start().line
}

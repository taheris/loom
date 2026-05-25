//! Each Askama partial under
//! `crates/loom-templates/templates/partial/` is part of `templates`'
//! public contract: external consumers compose them into their own
//! templates. The walk asserts that every such partial has a matching
//! `pub const NAME: &str = include_str!("…/partial/<file>.md")` (or
//! `&'static str`) somewhere in `crates/loom-templates/src/`. The const's
//! `include_str!` argument must end with `partial/<file>.md`; the const
//! name itself is not constrained — only the body's `include_str!` path.

use std::collections::HashSet;
use std::path::Path;

use proc_macro2::TokenTree;

use super::util::{immediate_children, parse_rs, rs_files_recursive, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "loom_templates_public_partial_constants — every templates/partial/<name>.md has a `pub const … = include_str!(\"…/partial/<name>.md\")` in loom-templates";

const SRC_DIR: &str = "crates/loom-templates/src";
const PARTIAL_DIR: &str = "crates/loom-templates/templates/partial";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let partial_dir = root.join(PARTIAL_DIR);
    let src_dir = root.join(SRC_DIR);

    let partials: Vec<String> = immediate_children(&partial_dir)
        .into_iter()
        .filter(|p| p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("md"))
        .filter_map(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        })
        .collect();
    let exposed = collect_public_include_paths(&src_dir);

    let mut violations = Vec::new();
    for partial in &partials {
        let suffix = format!("partial/{partial}");
        let found = exposed.iter().any(|path| path.ends_with(&suffix));
        if !found {
            violations.push(format!(
                "{SRC_DIR}/lib.rs:1 `partial/{partial}` is not exposed as a `pub const … = include_str!(\"…/{suffix}\")`",
            ));
        }
    }
    verdict_from(RULE, violations)
}

fn collect_public_include_paths(src_dir: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
    for path in rs_files_recursive(src_dir) {
        let Some(file) = parse_rs(&path) else {
            continue;
        };
        collect_from_items(&file.items, &mut out);
    }
    out
}

fn collect_from_items(items: &[syn::Item], out: &mut HashSet<String>) {
    for item in items {
        match item {
            syn::Item::Const(c) if matches!(c.vis, syn::Visibility::Public(_)) => {
                if let Some(path) = include_str_arg(&c.expr) {
                    out.insert(path);
                }
            }
            syn::Item::Mod(m) => {
                if let Some((_, nested)) = &m.content {
                    collect_from_items(nested, out);
                }
            }
            _ => {}
        }
    }
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

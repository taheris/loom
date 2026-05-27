//! `templates` is a public-contract crate. Per its spec, the
//! following typed building blocks are part of the consumer-facing
//! surface and must be defined as publicly reachable `pub struct` or
//! `pub enum` items under `crates/loom-templates/src/`: `PreviousFailure`,
//! `VerifierFailure`, `ReviewConcernKind`, `DriverNoticeCause`,
//! `LoopContext`, `ReviewContext`, `PinnedContext`. A `pub use`
//! re-export from any source file under that crate also counts.

use std::collections::HashSet;
use std::path::Path;

use super::util::{parse_rs, rs_files_recursive, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "loom_templates_public_types — PreviousFailure, VerifierFailure, ReviewConcernKind, DriverNoticeCause, LoopContext, ReviewContext, PinnedContext are publicly exposed by loom-templates";

const REQUIRED: &[&str] = &[
    "PreviousFailure",
    "VerifierFailure",
    "ReviewConcernKind",
    "DriverNoticeCause",
    "LoopContext",
    "ReviewContext",
    "PinnedContext",
];

const SRC_DIR: &str = "crates/loom-templates/src";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let src_dir = root.join(SRC_DIR);
    let exposed = collect_public_names(&src_dir);
    let mut violations = Vec::new();
    for name in REQUIRED {
        if !exposed.contains(*name) {
            violations.push(format!(
                "{SRC_DIR}/lib.rs:1 `{name}` is not publicly exposed — declare it as `pub struct`/`pub enum` or re-export via `pub use`",
            ));
        }
    }
    verdict_from(RULE, violations)
}

fn collect_public_names(src_dir: &Path) -> HashSet<String> {
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
            syn::Item::Struct(s) if matches!(s.vis, syn::Visibility::Public(_)) => {
                out.insert(s.ident.to_string());
            }
            syn::Item::Enum(e) if matches!(e.vis, syn::Visibility::Public(_)) => {
                out.insert(e.ident.to_string());
            }
            syn::Item::Type(t) if matches!(t.vis, syn::Visibility::Public(_)) => {
                out.insert(t.ident.to_string());
            }
            syn::Item::Use(u) if matches!(u.vis, syn::Visibility::Public(_)) => {
                collect_from_use_tree(&u.tree, out);
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

fn collect_from_use_tree(tree: &syn::UseTree, out: &mut HashSet<String>) {
    match tree {
        syn::UseTree::Path(p) => collect_from_use_tree(&p.tree, out),
        syn::UseTree::Name(n) => {
            out.insert(n.ident.to_string());
        }
        syn::UseTree::Rename(r) => {
            out.insert(r.rename.to_string());
        }
        syn::UseTree::Group(g) => {
            for nested in &g.items {
                collect_from_use_tree(nested, out);
            }
        }
        syn::UseTree::Glob(_) => {}
    }
}

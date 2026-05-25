//! The `Idle`/`Active` typestate is an internal mechanic of the
//! subprocess-driving backends — it must not leak through the public
//! `Session` trait that lives in `loom-events`. The walk locates the
//! `pub trait Session` in `loom-events`, walks its item signatures
//! (associated types, methods, supertraits, generics) via the syn AST,
//! and flags any reference to `Idle`, `Active`, or `AgentSession`.

use std::path::Path;

use super::util::{parse_rs, rel, rs_files_recursive, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

use syn::spanned::Spanned;
use syn::visit::Visit;

const RULE: &str = "session_trait_does_not_expose_typestate — `Session` trait surface does not reference Idle / Active / AgentSession";

const EVENTS_SRC: &str = "crates/loom-events/src";
const FORBIDDEN: &[&str] = &["Idle", "Active", "AgentSession"];

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let events_src = root.join(EVENTS_SRC);
    let mut violations = Vec::new();

    let Some((path, item)) = find_session_trait(&events_src) else {
        violations.push(format!(
            "{EVENTS_SRC}/lib.rs:1 `pub trait Session` not found — cannot audit its surface",
        ));
        return verdict_from(RULE, violations);
    };

    let lineno = item.span().start().line;
    let path_rel = rel(&root, &path);
    let mut collector = IdentCollector::default();
    collector.visit_item_trait(&item);
    for ident in collector.idents {
        if let Some(banned) = FORBIDDEN.iter().find(|b| ident == **b) {
            violations.push(format!(
                "{path_rel}:{lineno} `Session` trait surface references `{banned}` — typestate is private to subprocess backends, not part of the public trait",
            ));
        }
    }

    verdict_from(RULE, violations)
}

#[derive(Default)]
struct IdentCollector {
    idents: Vec<String>,
}

impl<'ast> Visit<'ast> for IdentCollector {
    fn visit_path(&mut self, node: &'ast syn::Path) {
        for segment in &node.segments {
            self.idents.push(segment.ident.to_string());
        }
        syn::visit::visit_path(self, node);
    }
}

fn find_session_trait(events_src: &Path) -> Option<(std::path::PathBuf, syn::ItemTrait)> {
    for path in rs_files_recursive(events_src) {
        let Some(file) = parse_rs(&path) else {
            continue;
        };
        if let Some(item) = find_in_items(&file.items) {
            return Some((path, item));
        }
    }
    None
}

fn find_in_items(items: &[syn::Item]) -> Option<syn::ItemTrait> {
    for item in items {
        match item {
            syn::Item::Trait(t)
                if t.ident == "Session" && matches!(t.vis, syn::Visibility::Public(_)) =>
            {
                return Some(t.clone());
            }
            syn::Item::Mod(m) => {
                if let Some((_, nested)) = &m.content
                    && let Some(hit) = find_in_items(nested)
                {
                    return Some(hit);
                }
            }
            _ => {}
        }
    }
    None
}

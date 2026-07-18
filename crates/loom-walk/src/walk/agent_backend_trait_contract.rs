//! Agent backend trait contract verifier.
//!
//! The backend trait is the static-dispatch seam between workflow code and
//! concrete agent runtimes. This walk parses the trait surface and verifies it
//! still exposes an associated `spawn` function without reintroducing a
//! backend-level `SUPPORTS_STEERING` gate.

use super::util::{line_of, parse_rs, rel, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str =
    "agent_backend_trait_contract — AgentBackend exposes spawn and no SUPPORTS_STEERING constant";
const BACKEND_SRC: &str = "crates/loom-driver/src/agent/backend.rs";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let path = root.join(BACKEND_SRC);
    let Some(file) = parse_rs(&path) else {
        return verdict_from(
            RULE,
            vec![format!(
                "{BACKEND_SRC}:1 could not parse backend trait source"
            )],
        );
    };
    let mut violations = Vec::new();
    let Some(item) = find_agent_backend_trait(&file.items) else {
        violations.push(format!(
            "{BACKEND_SRC}:1 `pub trait AgentBackend` not found — cannot audit backend contract",
        ));
        return verdict_from(RULE, violations);
    };

    let trait_line = line_of(&item);
    let path_rel = rel(&root, &path);
    let spawn = item.items.iter().find_map(|trait_item| match trait_item {
        syn::TraitItem::Fn(func) if func.sig.ident == "spawn" => Some(func),
        _ => None,
    });
    match spawn {
        Some(func) => {
            if func
                .sig
                .inputs
                .iter()
                .any(|arg| matches!(arg, syn::FnArg::Receiver(_)))
            {
                violations.push(format!(
                    "{path_rel}:{} `AgentBackend::spawn` must be associated, not a receiver method",
                    line_of(func),
                ));
            }
        }
        None => violations.push(format!(
            "{path_rel}:{trait_line} `AgentBackend` must expose associated `spawn`",
        )),
    }

    for trait_item in &item.items {
        if let syn::TraitItem::Const(item_const) = trait_item
            && item_const.ident == "SUPPORTS_STEERING"
        {
            violations.push(format!(
                "{path_rel}:{} `SUPPORTS_STEERING` must not be part of AgentBackend; steering belongs to the session contract",
                line_of(item_const),
            ));
        }
    }

    verdict_from(RULE, violations)
}

fn find_agent_backend_trait(items: &[syn::Item]) -> Option<syn::ItemTrait> {
    for item in items {
        match item {
            syn::Item::Trait(trait_item)
                if trait_item.ident == "AgentBackend"
                    && matches!(trait_item.vis, syn::Visibility::Public(_)) =>
            {
                return Some(trait_item.clone());
            }
            syn::Item::Mod(module) => {
                if let Some((_, nested)) = &module.content
                    && let Some(hit) = find_agent_backend_trait(nested)
                {
                    return Some(hit);
                }
            }
            _ => {}
        }
    }
    None
}

//! `loom-llm` owns provider/tool-loop primitives only. Skill discovery,
//! native registration, and materialized registries belong to `loom-skills`
//! and `loom-agent`, so the public LLM surface must not expose skill
//! registry names, features, or dependencies.

use syn::Visibility;

use super::util::{
    parse_rs, read_to_string, rel, rs_files_recursive, verdict_from, workspace_root,
};
use super::{Verdict, WalkInput};

const RULE: &str = "loom_llm_has_no_skill_registry_surface — `loom-llm` exposes no `loom-skills` dependency or native skill registry surface";

const MANIFEST_REL: &str = "crates/loom-llm/Cargo.toml";
const SRC_DIR: &str = "crates/loom-llm/src";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let mut violations = Vec::new();

    check_manifest(&root, &mut violations);
    for path in rs_files_recursive(&root.join(SRC_DIR)) {
        let Some(file) = parse_rs(&path) else {
            continue;
        };
        let rel_path = rel(&root, &path);
        collect_public_skill_surface(&file.items, &rel_path, &mut violations);
    }

    verdict_from(RULE, violations)
}

fn check_manifest(root: &std::path::Path, violations: &mut Vec<String>) {
    let manifest = root.join(MANIFEST_REL);
    let Some(body) = read_to_string(&manifest) else {
        violations.push(format!("{MANIFEST_REL}:1 manifest not readable"));
        return;
    };
    let Ok(value) = toml::from_str::<toml::Value>(&body) else {
        violations.push(format!("{MANIFEST_REL}:1 manifest not valid TOML"));
        return;
    };
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if value
            .get(section)
            .and_then(toml::Value::as_table)
            .is_some_and(|deps| deps.contains_key("loom-skills"))
        {
            violations.push(format!(
                "{MANIFEST_REL}:1 forbidden `{section}` entry `loom-skills` — skill registries stay outside `loom-llm`",
            ));
        }
    }
    if let Some(features) = value.get("features").and_then(toml::Value::as_table) {
        for name in features.keys() {
            if forbidden_surface_name(name) {
                violations.push(format!(
                    "{MANIFEST_REL}:1 forbidden feature `{name}` — `loom-llm` has no native skill registry surface",
                ));
            }
        }
    }
}

fn collect_public_skill_surface(items: &[syn::Item], rel_path: &str, violations: &mut Vec<String>) {
    for item in items {
        match item {
            syn::Item::Struct(item) if is_public(&item.vis) => {
                check_ident(&item.ident, rel_path, violations);
            }
            syn::Item::Enum(item) if is_public(&item.vis) => {
                check_ident(&item.ident, rel_path, violations);
            }
            syn::Item::Trait(item) if is_public(&item.vis) => {
                check_ident(&item.ident, rel_path, violations);
            }
            syn::Item::Type(item) if is_public(&item.vis) => {
                check_ident(&item.ident, rel_path, violations);
            }
            syn::Item::Fn(item) if is_public(&item.vis) => {
                check_ident(&item.sig.ident, rel_path, violations);
            }
            syn::Item::Mod(item) => {
                if is_public(&item.vis) {
                    check_ident(&item.ident, rel_path, violations);
                }
                if let Some((_, nested)) = &item.content {
                    collect_public_skill_surface(nested, rel_path, violations);
                }
            }
            syn::Item::Use(item) if is_public(&item.vis) => {
                check_use_tree(&item.tree, rel_path, violations);
            }
            syn::Item::Impl(item) => {
                for impl_item in &item.items {
                    if let syn::ImplItem::Fn(function) = impl_item
                        && is_public(&function.vis)
                    {
                        check_ident(&function.sig.ident, rel_path, violations);
                    }
                }
            }
            _ => {}
        }
    }
}

fn is_public(vis: &Visibility) -> bool {
    matches!(vis, Visibility::Public(_))
}

fn check_ident(ident: &syn::Ident, rel_path: &str, violations: &mut Vec<String>) {
    let name = ident.to_string();
    if forbidden_surface_name(&name) {
        violations.push(format!(
            "{rel_path}:1 forbidden public skill registry surface `{name}` in `loom-llm`",
        ));
    }
}

fn check_use_tree(tree: &syn::UseTree, rel_path: &str, violations: &mut Vec<String>) {
    match tree {
        syn::UseTree::Path(path) => check_use_tree(&path.tree, rel_path, violations),
        syn::UseTree::Name(name) => check_ident(&name.ident, rel_path, violations),
        syn::UseTree::Rename(rename) => check_ident(&rename.rename, rel_path, violations),
        syn::UseTree::Group(group) => {
            for nested in &group.items {
                check_use_tree(nested, rel_path, violations);
            }
        }
        syn::UseTree::Glob(_) => {}
    }
}

fn forbidden_surface_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("skill") || lower.contains("registrar")
}

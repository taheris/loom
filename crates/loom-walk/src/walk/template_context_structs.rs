//! Architectural: every top-level Askama template under
//! `loom-templates/templates/` has a matching `#[derive(Template)]`
//! context struct in `loom-templates/src/`. A missing pairing means
//! the template renders with untyped values, defeating the
//! compile-time field check Askama provides.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use syn::{Attribute, Expr, ExprLit, Lit, Meta};

use super::util::{
    derive_idents, immediate_children, parse_rs, rel, rs_files_recursive, verdict_from,
    workspace_root,
};
use super::{Verdict, WalkInput};

const RULE: &str = "template_context_structs — every template has a typed context struct";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let templates_dir = root.join("crates/loom-templates/templates");
    let src_dir = root.join("crates/loom-templates/src");
    let template_files = top_level_templates(&templates_dir);
    let context_paths = collect_template_context_paths(&src_dir);

    let mut violations = Vec::new();
    for tpl in &template_files {
        let name = tpl
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        if !context_paths.contains(&name) {
            violations.push(format!(
                "{}:1 template `{}` has no `#[derive(Template)] #[template(path = \"{}\")]` context struct in loom-templates/src/",
                rel(&root, tpl),
                name,
                name,
            ));
        }
    }
    verdict_from(RULE, violations)
}

fn top_level_templates(dir: &Path) -> Vec<PathBuf> {
    immediate_children(dir)
        .into_iter()
        .filter(|p| p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("md"))
        .collect()
}

fn collect_template_context_paths(src_dir: &Path) -> HashSet<String> {
    let mut paths = HashSet::new();
    for path in rs_files_recursive(src_dir) {
        let Some(parsed) = parse_rs(&path) else {
            continue;
        };
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

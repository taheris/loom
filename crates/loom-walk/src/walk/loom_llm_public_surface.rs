//! `llm` is a public-contract crate; its consumer-facing surface must
//! include the typed building blocks defined in `specs/llm.md` —
//! `LlmClient`, `LlmClientExt`, `CompletionRequest`, `Message`,
//! `MessageContent`, `BinaryContent`, `MimeType`, `ModelId`,
//! `SchemaKind`, `CacheControl`, `Tool`, `Conversation`, `LlmError`,
//! `LlmCapability`, `RetryAdvice`, `ApiKey`. Each must be publicly
//! reachable from `crates/loom-llm/src/` via a `pub trait`, `pub struct`,
//! `pub enum`, or `pub use` re-export.
//!
//! The same walk pins the structured-output trait split: `LlmClient`
//! owns the object-safe `complete_structured_raw` method, while the
//! generic `complete_structured::<T>` method lives on `LlmClientExt`.
//! It also pins the dyn-safe `emit_event` hook that lets
//! `Conversation` forward observer driver events into a Client's sink
//! chain.

use std::collections::HashSet;
use std::path::Path;

use super::util::{line_of, parse_rs, rel, rs_files_recursive, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "loom_llm_public_surface — typed LLM public surface including multimodal content types is exposed by loom-llm";

const REQUIRED: &[&str] = &[
    "LlmClient",
    "LlmClientExt",
    "CompletionRequest",
    "Message",
    "MessageContent",
    "BinaryContent",
    "MimeType",
    "ModelId",
    "SchemaKind",
    "CacheControl",
    "Tool",
    "Conversation",
    "LlmError",
    "LlmCapability",
    "RetryAdvice",
    "ApiKey",
];

const LLM_CLIENT_METHODS: &[&str] = &[
    "schema",
    "supports",
    "emit_event",
    "complete",
    "complete_structured_raw",
];

const SRC_DIR: &str = "crates/loom-llm/src";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let src_dir = root.join(SRC_DIR);
    let surface = collect_public_surface(&root, &src_dir);
    let mut violations = Vec::new();
    for name in REQUIRED {
        if !surface.names.contains(*name) {
            violations.push(format!(
                "{SRC_DIR}/lib.rs:1 `{name}` is not publicly exposed — declare it as `pub trait`/`pub struct`/`pub enum` or re-export via `pub use`",
            ));
        }
    }
    check_llm_client_contract(&surface, &mut violations);
    check_llm_client_ext_contract(&surface, &mut violations);
    verdict_from(RULE, violations)
}

#[derive(Default)]
struct Surface {
    names: HashSet<String>,
    traits: Vec<TraitInfo>,
}

impl Surface {
    fn trait_named(&self, name: &str) -> Option<&TraitInfo> {
        self.traits.iter().find(|item| item.name == name)
    }
}

struct TraitInfo {
    name: String,
    rel_path: String,
    line: usize,
    supertraits: HashSet<String>,
    methods: Vec<MethodInfo>,
}

impl TraitInfo {
    fn method(&self, name: &str) -> Option<&MethodInfo> {
        self.methods.iter().find(|method| method.name == name)
    }

    fn extends(&self, name: &str) -> bool {
        self.supertraits.contains(name)
    }
}

struct MethodInfo {
    name: String,
    line: usize,
    has_receiver: bool,
    has_type_or_const_generics: bool,
    has_self_sized_bound: bool,
}

fn collect_public_surface(root: &Path, src_dir: &Path) -> Surface {
    let mut out = Surface::default();
    for path in rs_files_recursive(src_dir) {
        let Some(file) = parse_rs(&path) else {
            continue;
        };
        let rel_path = rel(root, &path);
        collect_from_items(&file.items, &rel_path, &mut out);
    }
    out
}

fn collect_from_items(items: &[syn::Item], rel_path: &str, out: &mut Surface) {
    for item in items {
        match item {
            syn::Item::Struct(s) if matches!(s.vis, syn::Visibility::Public(_)) => {
                out.names.insert(s.ident.to_string());
            }
            syn::Item::Enum(e) if matches!(e.vis, syn::Visibility::Public(_)) => {
                out.names.insert(e.ident.to_string());
            }
            syn::Item::Trait(t) if matches!(t.vis, syn::Visibility::Public(_)) => {
                out.names.insert(t.ident.to_string());
                out.traits.push(trait_surface(t, rel_path));
            }
            syn::Item::Type(t) if matches!(t.vis, syn::Visibility::Public(_)) => {
                out.names.insert(t.ident.to_string());
            }
            syn::Item::Use(u) if matches!(u.vis, syn::Visibility::Public(_)) => {
                collect_from_use_tree(&u.tree, &mut out.names);
            }
            syn::Item::Mod(m) => {
                if let Some((_, nested)) = &m.content {
                    collect_from_items(nested, rel_path, out);
                }
            }
            _ => {}
        }
    }
}

fn trait_surface(item: &syn::ItemTrait, rel_path: &str) -> TraitInfo {
    let methods = item
        .items
        .iter()
        .filter_map(|trait_item| {
            let syn::TraitItem::Fn(method) = trait_item else {
                return None;
            };
            Some(MethodInfo {
                name: method.sig.ident.to_string(),
                line: line_of(method),
                has_receiver: method
                    .sig
                    .inputs
                    .iter()
                    .any(|arg| matches!(arg, syn::FnArg::Receiver(_))),
                has_type_or_const_generics: has_type_or_const_generics(&method.sig.generics),
                has_self_sized_bound: method_requires_self_sized(&method.sig),
            })
        })
        .collect();
    TraitInfo {
        name: item.ident.to_string(),
        rel_path: rel_path.to_string(),
        line: line_of(item),
        supertraits: item.supertraits.iter().filter_map(bound_ident).collect(),
        methods,
    }
}

fn check_llm_client_contract(surface: &Surface, violations: &mut Vec<String>) {
    let Some(client) = surface.trait_named("LlmClient") else {
        return;
    };
    for method in LLM_CLIENT_METHODS {
        if client.method(method).is_none() {
            violations.push(format!(
                "{}:{} `pub trait LlmClient` is missing `{method}`",
                client.rel_path, client.line,
            ));
        }
    }
    if client.method("complete_structured").is_some() {
        violations.push(format!(
            "{}:{} `LlmClient::complete_structured` must live on `LlmClientExt`; the base trait keeps only dyn-safe methods",
            client.rel_path, client.line,
        ));
    }
    let Some(raw) = client.method("complete_structured_raw") else {
        return;
    };
    if !raw.has_receiver {
        violations.push(format!(
            "{}:{} `LlmClient::complete_structured_raw` must take a receiver so `dyn LlmClient` can dispatch it",
            client.rel_path, raw.line,
        ));
    }
    if raw.has_type_or_const_generics {
        violations.push(format!(
            "{}:{} `LlmClient::complete_structured_raw` must be type-erased, not generic",
            client.rel_path, raw.line,
        ));
    }
    if raw.has_self_sized_bound {
        violations.push(format!(
            "{}:{} `LlmClient::complete_structured_raw` must not require `Self: Sized`; dyn callers need this method",
            client.rel_path, raw.line,
        ));
    }
}

fn check_llm_client_ext_contract(surface: &Surface, violations: &mut Vec<String>) {
    let Some(ext) = surface.trait_named("LlmClientExt") else {
        return;
    };
    if !ext.extends("LlmClient") {
        violations.push(format!(
            "{}:{} `pub trait LlmClientExt` must extend `LlmClient`",
            ext.rel_path, ext.line,
        ));
    }
    let Some(method) = ext.method("complete_structured") else {
        violations.push(format!(
            "{}:{} `pub trait LlmClientExt` is missing `complete_structured`",
            ext.rel_path, ext.line,
        ));
        return;
    };
    if !method.has_receiver {
        violations.push(format!(
            "{}:{} `LlmClientExt::complete_structured` must take a receiver",
            ext.rel_path, method.line,
        ));
    }
    if !method.has_type_or_const_generics {
        violations.push(format!(
            "{}:{} `LlmClientExt::complete_structured` must be generic over the structured output type",
            ext.rel_path, method.line,
        ));
    }
}

fn has_type_or_const_generics(generics: &syn::Generics) -> bool {
    generics.params.iter().any(|param| {
        matches!(
            param,
            syn::GenericParam::Type(_) | syn::GenericParam::Const(_)
        )
    })
}

fn method_requires_self_sized(sig: &syn::Signature) -> bool {
    sig.generics
        .where_clause
        .as_ref()
        .is_some_and(|where_clause| {
            where_clause
                .predicates
                .iter()
                .any(where_predicate_requires_self_sized)
        })
}

fn where_predicate_requires_self_sized(predicate: &syn::WherePredicate) -> bool {
    let syn::WherePredicate::Type(predicate_type) = predicate else {
        return false;
    };
    type_is_self(&predicate_type.bounded_ty) && predicate_type.bounds.iter().any(bound_is_sized)
}

fn type_is_self(ty: &syn::Type) -> bool {
    let syn::Type::Path(path) = ty else {
        return false;
    };
    path.path.is_ident("Self")
}

fn bound_is_sized(bound: &syn::TypeParamBound) -> bool {
    let syn::TypeParamBound::Trait(trait_bound) = bound else {
        return false;
    };
    trait_bound
        .path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "Sized")
}

fn bound_ident(bound: &syn::TypeParamBound) -> Option<String> {
    let syn::TypeParamBound::Trait(trait_bound) = bound else {
        return None;
    };
    Some(trait_bound.path.segments.last()?.ident.to_string())
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

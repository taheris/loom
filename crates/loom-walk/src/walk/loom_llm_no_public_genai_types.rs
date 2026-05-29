//! Wrapper Thickness invariant: `genai` is an internal implementation
//! dependency of `loom-llm`; the public surface must not name any
//! `genai::*` type. A future `genai` swap or vendoring stays a
//! single-crate internal change only when this invariant holds.
//!
//! The walk parses every `.rs` file under `crates/loom-llm/src/`,
//! skipping `#[cfg(test)]` items, and flags:
//!
//! - `pub use genai::...` re-exports (any path whose first segment is
//!   `genai`).
//! - `pub fn` signatures whose argument or return types mention any
//!   path rooted at `genai::*`.
//! - `pub trait` method signatures (`fn ...;`) mentioning `genai::*`.
//! - `pub struct` and `pub enum` `pub` fields whose type mentions
//!   `genai::*`.
//! - `pub type` aliases that mention `genai::*`.
//! - Public methods of inherent `impl` blocks (`impl Foo { pub fn ... }`)
//!   that mention `genai::*` in their signature.
//!
//! Trait `impl` blocks (`impl Trait for Type`) inherit visibility from
//! the trait, so the public surface check is captured by the
//! corresponding `pub trait` declaration; this walk does not re-scan
//! them. Non-`pub use genai::...` lines are accepted (internal
//! implementation usage).

use super::util::{line_of, parse_rs, rel, rs_files_recursive, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "loom_llm_no_public_genai_types — no public Client constructor or method signature references genai::Client, genai::Error, or other genai types";

const SRC_DIR: &str = "crates/loom-llm/src";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let src_dir = root.join(SRC_DIR);
    let mut violations = Vec::new();
    for path in rs_files_recursive(&src_dir) {
        let Some(file) = parse_rs(&path) else {
            continue;
        };
        let rel_path = rel(&root, &path);
        inspect_items(&file.items, &rel_path, &mut violations);
    }
    verdict_from(RULE, violations)
}

fn inspect_items(items: &[syn::Item], rel_path: &str, violations: &mut Vec<String>) {
    for item in items {
        if item_has_cfg_test(item) {
            continue;
        }
        match item {
            syn::Item::Mod(m) => {
                if let Some((_, nested)) = &m.content {
                    inspect_items(nested, rel_path, violations);
                }
            }
            syn::Item::Use(u)
                if matches!(u.vis, syn::Visibility::Public(_))
                    && use_tree_mentions_genai(&u.tree) =>
            {
                violations.push(format!(
                    "{rel_path}:{} `pub use` re-exports a genai item — wrap or define the type inside loom-llm instead",
                    line_of(u),
                ));
            }
            syn::Item::Fn(f) if matches!(f.vis, syn::Visibility::Public(_)) => {
                check_sig(&f.sig, rel_path, &f.sig.ident.to_string(), violations);
            }
            syn::Item::Trait(t) if matches!(t.vis, syn::Visibility::Public(_)) => {
                for trait_item in &t.items {
                    if let syn::TraitItem::Fn(tf) = trait_item {
                        let label = format!("{}::{}", t.ident, tf.sig.ident);
                        check_sig(&tf.sig, rel_path, &label, violations);
                    }
                }
            }
            syn::Item::Struct(s) if matches!(s.vis, syn::Visibility::Public(_)) => {
                for field in &s.fields {
                    if !matches!(field.vis, syn::Visibility::Public(_)) {
                        continue;
                    }
                    if type_mentions_genai(&field.ty) {
                        let field_name = field
                            .ident
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_else(|| "<unnamed>".to_string());
                        violations.push(format!(
                            "{rel_path}:{} `pub struct {}` field `{}` exposes a genai type",
                            line_of(field),
                            s.ident,
                            field_name,
                        ));
                    }
                }
            }
            syn::Item::Enum(e) if matches!(e.vis, syn::Visibility::Public(_)) => {
                for variant in &e.variants {
                    for field in &variant.fields {
                        if type_mentions_genai(&field.ty) {
                            let field_name = field
                                .ident
                                .as_ref()
                                .map(ToString::to_string)
                                .unwrap_or_else(|| "<unnamed>".to_string());
                            violations.push(format!(
                                "{rel_path}:{} `pub enum {}::{}` field `{}` exposes a genai type",
                                line_of(field),
                                e.ident,
                                variant.ident,
                                field_name,
                            ));
                        }
                    }
                }
            }
            syn::Item::Type(t)
                if matches!(t.vis, syn::Visibility::Public(_)) && type_mentions_genai(&t.ty) =>
            {
                violations.push(format!(
                    "{rel_path}:{} `pub type {}` aliases a genai type",
                    line_of(t),
                    t.ident,
                ));
            }
            syn::Item::Impl(impl_block) if impl_block.trait_.is_none() => {
                let owner =
                    self_type_name(&impl_block.self_ty).unwrap_or_else(|| "<unknown>".to_string());
                for impl_item in &impl_block.items {
                    let syn::ImplItem::Fn(f) = impl_item else {
                        continue;
                    };
                    if !matches!(f.vis, syn::Visibility::Public(_)) {
                        continue;
                    }
                    let label = format!("{owner}::{}", f.sig.ident);
                    check_sig(&f.sig, rel_path, &label, violations);
                }
            }
            _ => {}
        }
    }
}

fn check_sig(sig: &syn::Signature, rel_path: &str, label: &str, violations: &mut Vec<String>) {
    for arg in &sig.inputs {
        if let syn::FnArg::Typed(pat_ty) = arg
            && type_mentions_genai(&pat_ty.ty)
        {
            violations.push(format!(
                "{rel_path}:{} `pub fn {label}` parameter references a genai type",
                line_of(pat_ty),
            ));
        }
    }
    if let syn::ReturnType::Type(_, ty) = &sig.output
        && type_mentions_genai(ty)
    {
        violations.push(format!(
            "{rel_path}:{} `pub fn {label}` return type references a genai type",
            line_of(sig),
        ));
    }
}

fn type_mentions_genai(ty: &syn::Type) -> bool {
    match ty {
        syn::Type::Path(tp) => {
            if let Some(first) = tp.path.segments.first()
                && first.ident == "genai"
            {
                return true;
            }
            tp.path
                .segments
                .iter()
                .any(|seg| path_args_mention_genai(&seg.arguments))
        }
        syn::Type::Reference(r) => type_mentions_genai(&r.elem),
        syn::Type::Ptr(p) => type_mentions_genai(&p.elem),
        syn::Type::Array(a) => type_mentions_genai(&a.elem),
        syn::Type::Slice(s) => type_mentions_genai(&s.elem),
        syn::Type::Group(g) => type_mentions_genai(&g.elem),
        syn::Type::Paren(p) => type_mentions_genai(&p.elem),
        syn::Type::Tuple(t) => t.elems.iter().any(type_mentions_genai),
        syn::Type::BareFn(bf) => {
            bf.inputs.iter().any(|arg| type_mentions_genai(&arg.ty))
                || matches!(&bf.output, syn::ReturnType::Type(_, ty) if type_mentions_genai(ty))
        }
        syn::Type::TraitObject(to) => to.bounds.iter().any(bound_mentions_genai),
        syn::Type::ImplTrait(it) => it.bounds.iter().any(bound_mentions_genai),
        _ => false,
    }
}

fn bound_mentions_genai(bound: &syn::TypeParamBound) -> bool {
    match bound {
        syn::TypeParamBound::Trait(t) => {
            if let Some(first) = t.path.segments.first()
                && first.ident == "genai"
            {
                return true;
            }
            t.path
                .segments
                .iter()
                .any(|seg| path_args_mention_genai(&seg.arguments))
        }
        _ => false,
    }
}

fn path_args_mention_genai(args: &syn::PathArguments) -> bool {
    match args {
        syn::PathArguments::AngleBracketed(ab) => ab.args.iter().any(|arg| match arg {
            syn::GenericArgument::Type(t) => type_mentions_genai(t),
            syn::GenericArgument::AssocType(at) => type_mentions_genai(&at.ty),
            _ => false,
        }),
        syn::PathArguments::Parenthesized(p) => {
            p.inputs.iter().any(type_mentions_genai)
                || matches!(&p.output, syn::ReturnType::Type(_, ty) if type_mentions_genai(ty))
        }
        syn::PathArguments::None => false,
    }
}

fn use_tree_mentions_genai(tree: &syn::UseTree) -> bool {
    match tree {
        syn::UseTree::Path(p) => p.ident == "genai" || use_tree_mentions_genai(&p.tree),
        syn::UseTree::Name(n) => n.ident == "genai",
        syn::UseTree::Rename(r) => r.ident == "genai",
        syn::UseTree::Group(g) => g.items.iter().any(use_tree_mentions_genai),
        syn::UseTree::Glob(_) => false,
    }
}

fn self_type_name(ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(p) = ty else { return None };
    Some(p.path.segments.last()?.ident.to_string())
}

fn item_has_cfg_test(item: &syn::Item) -> bool {
    item_attrs(item).is_some_and(attrs_have_cfg_test)
}

fn item_attrs(item: &syn::Item) -> Option<&[syn::Attribute]> {
    match item {
        syn::Item::Const(i) => Some(&i.attrs),
        syn::Item::Enum(i) => Some(&i.attrs),
        syn::Item::ExternCrate(i) => Some(&i.attrs),
        syn::Item::Fn(i) => Some(&i.attrs),
        syn::Item::ForeignMod(i) => Some(&i.attrs),
        syn::Item::Impl(i) => Some(&i.attrs),
        syn::Item::Macro(i) => Some(&i.attrs),
        syn::Item::Mod(i) => Some(&i.attrs),
        syn::Item::Static(i) => Some(&i.attrs),
        syn::Item::Struct(i) => Some(&i.attrs),
        syn::Item::Trait(i) => Some(&i.attrs),
        syn::Item::TraitAlias(i) => Some(&i.attrs),
        syn::Item::Type(i) => Some(&i.attrs),
        syn::Item::Union(i) => Some(&i.attrs),
        syn::Item::Use(i) => Some(&i.attrs),
        _ => None,
    }
}

fn attrs_have_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        if !a.path().is_ident("cfg") {
            return false;
        }
        let Ok(meta) = a.parse_args::<syn::Meta>() else {
            return false;
        };
        meta.path().is_ident("test")
    })
}

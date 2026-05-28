//! Per-schema Client constructors take typed credentials, not raw
//! strings. `AnthropicClient::new`, `OpenAiClient::new`,
//! `GeminiClient::new` accept [`ApiKey`]; `OpenAiCompatClient::new`
//! accepts [`url::Url`] + `Option<ApiKey>`. No constructor parameter
//! is allowed to be `String` or `&str` for a credential or base URL.
//!
//! The walk parses `crates/loom-llm/src/client/`, locates every `pub
//! fn new(...)` inside an impl whose type ends in `Client`, and asserts
//! every parameter type matches an allowlist:
//!
//! - `ApiKey`
//! - `Option<ApiKey>`
//! - `Url`
//! - `&self` / `self` / `&mut self` (none expected on `new`, but
//!   acceptable as receiver if syn surfaces it that way)
//!
//! Any `String`, `&str`, `&String`, or `Option<String>` parameter is
//! flagged as a Parse-Don't-Validate violation (RS-6 / RS-7): invalid
//! input must be rejected at the boundary, not silently carried into
//! the Client.

use super::util::{parse_rs, rs_files_recursive, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "loom_llm_client_constructors_use_newtypes — per-schema Client `new(...)` parameters use ApiKey / Url newtypes, never raw String for credentials or base URL";

const SRC_DIR: &str = "crates/loom-llm/src/client";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let src_dir = root.join(SRC_DIR);

    let mut violations = Vec::new();

    for path in rs_files_recursive(&src_dir) {
        let Some(file) = parse_rs(&path) else {
            continue;
        };
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        inspect_items(&file.items, &rel, &mut violations);
    }

    verdict_from(RULE, violations)
}

fn inspect_items(items: &[syn::Item], rel: &str, violations: &mut Vec<String>) {
    for item in items {
        match item {
            syn::Item::Impl(impl_block) if impl_block.trait_.is_none() => {
                let Some(self_name) = type_name(&impl_block.self_ty) else {
                    continue;
                };
                if !self_name.ends_with("Client") {
                    continue;
                }
                for impl_item in &impl_block.items {
                    let syn::ImplItem::Fn(f) = impl_item else {
                        continue;
                    };
                    if !matches!(f.vis, syn::Visibility::Public(_)) {
                        continue;
                    }
                    if f.sig.ident != "new" {
                        continue;
                    }
                    for arg in &f.sig.inputs {
                        let syn::FnArg::Typed(pat_ty) = arg else {
                            continue;
                        };
                        let ty_text = render_type(&pat_ty.ty);
                        if is_banned_credential_type(&ty_text) {
                            violations.push(format!(
                                "{rel}: `{self_name}::new` parameter has type `{ty_text}` — use ApiKey / Url newtypes, never raw String for credentials or base URL",
                            ));
                        }
                    }
                }
            }
            syn::Item::Mod(m) => {
                if let Some((_, nested)) = &m.content {
                    inspect_items(nested, rel, violations);
                }
            }
            _ => {}
        }
    }
}

fn type_name(ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(p) = ty else { return None };
    Some(p.path.segments.last()?.ident.to_string())
}

fn render_type(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(p) => {
            let segs: Vec<String> = p.path.segments.iter().map(render_path_segment).collect();
            segs.join("::")
        }
        syn::Type::Reference(r) => {
            let inner = render_type(&r.elem);
            let mutability = if r.mutability.is_some() { "mut " } else { "" };
            format!("&{mutability}{inner}")
        }
        syn::Type::Tuple(t) => {
            let parts: Vec<String> = t.elems.iter().map(render_type).collect();
            format!("({})", parts.join(","))
        }
        _ => "<other>".to_string(),
    }
}

fn render_path_segment(seg: &syn::PathSegment) -> String {
    let name = seg.ident.to_string();
    match &seg.arguments {
        syn::PathArguments::None => name,
        syn::PathArguments::AngleBracketed(ab) => {
            let inner: Vec<String> = ab
                .args
                .iter()
                .filter_map(|arg| match arg {
                    syn::GenericArgument::Type(t) => Some(render_type(t)),
                    _ => None,
                })
                .collect();
            format!("{name}<{}>", inner.join(","))
        }
        syn::PathArguments::Parenthesized(_) => name,
    }
}

fn is_banned_credential_type(rendered: &str) -> bool {
    matches!(
        rendered,
        "String" | "&String" | "&str" | "Option<String>" | "Option<&String>" | "Option<&str>"
    )
}

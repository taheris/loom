//! `loom-llm` ships exactly one `pub struct *Client` per `SchemaKind`
//! variant, each carrying a `pub const SCHEMA: SchemaKind` whose value
//! matches the corresponding `SchemaKind` arm.
//!
//! The walk parses `crates/loom-llm/src/`, collects every public struct
//! whose name ends in `Client` and that carries a `pub const SCHEMA`
//! associated constant, and asserts:
//!
//! 1. Each `SchemaKind` variant (`Anthropic`, `OpenAi`, `Gemini`) has at
//!    least one corresponding `pub struct *Client` whose `SCHEMA` const
//!    is set to `SchemaKind::<variant>`.
//! 2. No two Client structs declare the same `SCHEMA` value (1:1
//!    mapping).
//!
//! `OpenAiCompatClient` is gated behind the `openai-compat` Cargo
//! feature; this walk does not require it, so its absence does not
//! fail.

use std::collections::HashMap;

use super::util::{parse_rs, rs_files_recursive, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "loom_llm_client_types_per_schema_kind — one `pub struct *Client` with `pub const SCHEMA: SchemaKind` per SchemaKind variant";

const SRC_DIR: &str = "crates/loom-llm/src/client";

const REQUIRED_SCHEMAS: &[&str] = &["Anthropic", "OpenAi", "Gemini"];

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let src_dir = root.join(SRC_DIR);

    let mut schema_to_clients: HashMap<String, Vec<String>> = HashMap::new();
    let mut clients_without_schema_const: Vec<String> = Vec::new();

    for path in rs_files_recursive(&src_dir) {
        let Some(file) = parse_rs(&path) else {
            continue;
        };
        collect_from_items(
            &file.items,
            &mut schema_to_clients,
            &mut clients_without_schema_const,
        );
    }

    let mut violations = Vec::new();

    for required in REQUIRED_SCHEMAS {
        match schema_to_clients.get(*required) {
            Some(clients) if !clients.is_empty() => {}
            _ => violations.push(format!(
                "{SRC_DIR}: no `pub struct *Client` declares `pub const SCHEMA: SchemaKind = SchemaKind::{required}`",
            )),
        }
    }

    for (schema, clients) in &schema_to_clients {
        if clients.len() > 1 {
            let names = clients.join(", ");
            violations.push(format!(
                "{SRC_DIR}: SchemaKind::{schema} has multiple Client types ({names}); 1:1 mapping required",
            ));
        }
    }

    for client in &clients_without_schema_const {
        violations.push(format!(
            "{SRC_DIR}: `pub struct {client}` declared but no matching `pub const SCHEMA: SchemaKind` in its impl block",
        ));
    }

    verdict_from(RULE, violations)
}

fn collect_from_items(
    items: &[syn::Item],
    schema_to_clients: &mut HashMap<String, Vec<String>>,
    clients_without_schema_const: &mut Vec<String>,
) {
    let public_client_structs: Vec<String> = items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Struct(s)
                if matches!(s.vis, syn::Visibility::Public(_))
                    && s.ident.to_string().ends_with("Client") =>
            {
                Some(s.ident.to_string())
            }
            _ => None,
        })
        .collect();

    let impl_schemas: HashMap<String, String> = items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Impl(impl_block) if impl_block.trait_.is_none() => {
                let self_name = type_name(&impl_block.self_ty)?;
                let schema = find_pub_const_schema(&impl_block.items)?;
                Some((self_name, schema))
            }
            _ => None,
        })
        .collect();

    for client in &public_client_structs {
        match impl_schemas.get(client) {
            Some(schema) => {
                schema_to_clients
                    .entry(schema.clone())
                    .or_default()
                    .push(client.clone());
            }
            None => clients_without_schema_const.push(client.clone()),
        }
    }

    for item in items {
        if let syn::Item::Mod(m) = item
            && let Some((_, nested)) = &m.content
        {
            collect_from_items(nested, schema_to_clients, clients_without_schema_const);
        }
    }
}

fn type_name(ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(p) = ty else { return None };
    Some(p.path.segments.last()?.ident.to_string())
}

fn find_pub_const_schema(items: &[syn::ImplItem]) -> Option<String> {
    for item in items {
        let syn::ImplItem::Const(c) = item else {
            continue;
        };
        if !matches!(c.vis, syn::Visibility::Public(_)) {
            continue;
        }
        if c.ident != "SCHEMA" {
            continue;
        }
        return schema_kind_value(&c.expr);
    }
    None
}

fn schema_kind_value(expr: &syn::Expr) -> Option<String> {
    let syn::Expr::Path(path_expr) = expr else {
        return None;
    };
    let segments = &path_expr.path.segments;
    if segments.len() < 2 {
        return None;
    }
    let parent = &segments[segments.len() - 2].ident;
    if parent != "SchemaKind" {
        return None;
    }
    let leaf = &segments[segments.len() - 1].ident;
    Some(leaf.to_string())
}

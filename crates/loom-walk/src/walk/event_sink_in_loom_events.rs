//! `EventSink` is the typed event channel the driver fans out to —
//! renderers, log sinks, custom consumers. It belongs to the public
//! contract crate alongside `Session`. The walk asserts:
//!
//! - `pub trait EventSink` is defined in `loom-events/src/lib.rs`
//! - the `SessionCommand` enum (also in `loom-events`) carries the two
//!   steering variants `Steer(String)` and `Abort(String)` so
//!   `react()` callers can drive the agent without leaking driver
//!   types.

use std::path::Path;

use super::util::{parse_rs, rel, rs_files_recursive, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

use syn::spanned::Spanned;

const RULE: &str = "event_sink_in_loom_events — `EventSink` + `SessionCommand{Steer(String),Abort(String)}` live in loom-events";

const EVENTS_CRATE: &str = "crates/loom-events/src";
const EVENTS_LIB_REL: &str = "crates/loom-events/src/lib.rs";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let mut violations = Vec::new();

    let lib_path = root.join(EVENTS_LIB_REL);
    if !trait_defined(&lib_path, "EventSink") {
        violations.push(format!(
            "{EVENTS_LIB_REL}:1 `pub trait EventSink` not defined — declare it in the public-contract leaf crate",
        ));
    }

    let crate_root = root.join(EVENTS_CRATE);
    match locate_enum(&root, &crate_root, "SessionCommand") {
        None => violations.push(format!(
            "{EVENTS_LIB_REL}:1 `pub enum SessionCommand` not defined in loom-events",
        )),
        Some((path_rel, variants, lineno)) => {
            let steer = variants
                .iter()
                .find(|(name, _)| name == "Steer")
                .map(|(_, ty)| ty.as_str());
            let abort = variants
                .iter()
                .find(|(name, _)| name == "Abort")
                .map(|(_, ty)| ty.as_str());
            if steer != Some("String") {
                violations.push(format!(
                    "{path_rel}:{lineno} `SessionCommand` missing variant `Steer(String)` (got {steer:?})",
                ));
            }
            if abort != Some("String") {
                violations.push(format!(
                    "{path_rel}:{lineno} `SessionCommand` missing variant `Abort(String)` (got {abort:?})",
                ));
            }
        }
    }

    verdict_from(RULE, violations)
}

fn trait_defined(path: &Path, name: &str) -> bool {
    let Some(file) = parse_rs(path) else {
        return false;
    };
    file.items.iter().any(|item| {
        matches!(item, syn::Item::Trait(t)
            if t.ident == name && matches!(t.vis, syn::Visibility::Public(_)))
    })
}

fn locate_enum(
    root: &Path,
    crate_root: &Path,
    name: &str,
) -> Option<(String, Vec<(String, String)>, usize)> {
    for path in rs_files_recursive(crate_root) {
        let Some(file) = parse_rs(&path) else {
            continue;
        };
        for item in &file.items {
            if let syn::Item::Enum(e) = item
                && e.ident == name
                && matches!(e.vis, syn::Visibility::Public(_))
            {
                let variants = e.variants.iter().map(variant_signature).collect();
                return Some((rel(root, &path), variants, e.span().start().line));
            }
        }
    }
    None
}

fn variant_signature(v: &syn::Variant) -> (String, String) {
    let payload = match &v.fields {
        syn::Fields::Unnamed(u) if u.unnamed.len() == 1 => u
            .unnamed
            .first()
            .map(|ty| type_ident_string(&ty.ty))
            .unwrap_or_default(),
        syn::Fields::Unit => String::new(),
        _ => "<unsupported>".to_string(),
    };
    (v.ident.to_string(), payload)
}

fn type_ident_string(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(p) => p
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

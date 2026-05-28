//! `LlmError` is the typed transport-failure surface external consumers
//! drive retry policy off; the variant set is spec'd in `specs/llm.md`
//! § LlmError and must match exactly so additive future variants are a
//! minor bump and a removal / rename is caught here.
//!
//! The walk parses `crates/loom-llm/src/client/mod.rs`, locates the
//! `LlmError` enum, and asserts:
//!
//! 1. The enum carries `#[non_exhaustive]` so consumers cannot rely on
//!    exhaustive matching outside the crate.
//! 2. Its variant set is exactly the nine spec'd names:
//!    `Transport`, `Timeout`, `RateLimited`, `AuthFailed`,
//!    `ProviderHttp`, `MalformedJson`, `SchemaViolation`,
//!    `IncompatibleModel`, `Provider`.

use std::collections::BTreeSet;

use super::util::{parse_rs, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "loom_llm_error_variant_set — LlmError is #[non_exhaustive] and carries the nine spec'd variants";

const SRC: &str = "crates/loom-llm/src/client/mod.rs";

const REQUIRED: &[&str] = &[
    "Transport",
    "Timeout",
    "RateLimited",
    "AuthFailed",
    "ProviderHttp",
    "MalformedJson",
    "SchemaViolation",
    "IncompatibleModel",
    "Provider",
];

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let path = root.join(SRC);
    let Some(file) = parse_rs(&path) else {
        return verdict_from(
            RULE,
            vec![format!(
                "{SRC}:1 unable to parse — LlmError enum cannot be inspected"
            )],
        );
    };

    let mut violations = Vec::new();
    let Some(en) = find_enum(&file.items, "LlmError") else {
        violations.push(format!(
            "{SRC}:1 `LlmError` enum not found — refactor must keep the public type in this module"
        ));
        return verdict_from(RULE, violations);
    };

    if !has_non_exhaustive(&en.attrs) {
        violations.push(format!(
            "{SRC}:1 `LlmError` is missing `#[non_exhaustive]` — required so future variants are additive"
        ));
    }

    let present: BTreeSet<String> = en.variants.iter().map(|v| v.ident.to_string()).collect();
    let required: BTreeSet<String> = REQUIRED.iter().map(|s| (*s).to_string()).collect();

    for missing in required.difference(&present) {
        violations.push(format!(
            "{SRC}:1 `LlmError::{missing}` variant missing — spec'd in specs/llm.md § LlmError"
        ));
    }
    for extra in present.difference(&required) {
        violations.push(format!(
            "{SRC}:1 `LlmError::{extra}` is not part of the spec'd variant set — remove or move loop-control concerns to ConversationError"
        ));
    }

    verdict_from(RULE, violations)
}

fn find_enum<'a>(items: &'a [syn::Item], name: &str) -> Option<&'a syn::ItemEnum> {
    for item in items {
        if let syn::Item::Enum(en) = item
            && en.ident == name
        {
            return Some(en);
        }
        if let syn::Item::Mod(m) = item
            && let Some((_, nested)) = &m.content
            && let Some(found) = find_enum(nested, name)
        {
            return Some(found);
        }
    }
    None
}

fn has_non_exhaustive(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("non_exhaustive"))
}

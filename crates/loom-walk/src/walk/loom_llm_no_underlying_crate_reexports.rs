//! `llm` is a typed wrapper — not a thin re-export of the
//! underlying multi-provider LLM crate. Each public type spec'd in
//! `specs/llm.md` (`LlmClient`, `LlmClientExt`, `CompletionRequest`,
//! `Message`, `MessageContent`, `BinaryContent`, `MimeType`, `ModelId`,
//! `SchemaKind`, `CacheControl`, `Tool`, `Conversation`, `LlmError`,
//! `LlmCapability`, `RetryAdvice`, `ApiKey`) must be **defined** inside
//! `crates/loom-llm/src/` (a `pub`/private `struct`, `enum`, or
//! `trait` with that ident), not only re-exported from an external
//! crate. This walk asserts every name has at least one in-crate
//! definition; a pure `pub use external::Name` would leave the
//! consumer-facing surface tied to whichever provider crate happens
//! to underlie loom-llm.

use std::collections::HashSet;
use std::path::Path;

use super::util::{parse_rs, rs_files_recursive, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "loom_llm_no_underlying_crate_reexports — public types defined in loom-llm, not re-exported from the underlying multi-provider crate";

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

const SRC_DIR: &str = "crates/loom-llm/src";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let src_dir = root.join(SRC_DIR);
    let defined = collect_definitions(&src_dir);
    let mut violations = Vec::new();
    for name in REQUIRED {
        if !defined.contains(*name) {
            violations.push(format!(
                "{SRC_DIR}/lib.rs:1 `{name}` has no in-crate definition — loom-llm must define this type, not re-export it from the underlying multi-provider crate",
            ));
        }
    }
    verdict_from(RULE, violations)
}

fn collect_definitions(src_dir: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
    for path in rs_files_recursive(src_dir) {
        let Some(file) = parse_rs(&path) else {
            continue;
        };
        collect_from_items(&file.items, &mut out);
    }
    out
}

fn collect_from_items(items: &[syn::Item], out: &mut HashSet<String>) {
    for item in items {
        match item {
            syn::Item::Struct(s) => {
                out.insert(s.ident.to_string());
            }
            syn::Item::Enum(e) => {
                out.insert(e.ident.to_string());
            }
            syn::Item::Trait(t) => {
                out.insert(t.ident.to_string());
            }
            syn::Item::Mod(m) => {
                if let Some((_, nested)) = &m.content {
                    collect_from_items(nested, out);
                }
            }
            _ => {}
        }
    }
}

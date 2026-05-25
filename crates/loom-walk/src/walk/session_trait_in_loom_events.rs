//! `Session` is part of the `loom-events` public contract: agents
//! consume the trait from the leaf crate, not from `loom-driver`. The
//! walk asserts `pub trait Session` is defined in `loom-events/src/lib.rs`
//! and does NOT appear as a trait definition anywhere under `loom-driver/`.

use std::path::Path;

use super::util::{parse_rs, rel, rs_files_recursive, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

use syn::spanned::Spanned;

const RULE: &str =
    "session_trait_in_loom_events — `pub trait Session` lives in loom-events, not loom-driver";

const EVENTS_LIB: &str = "crates/loom-events/src/lib.rs";
const DRIVER_SRC: &str = "crates/loom-driver/src";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let mut violations = Vec::new();

    let events_path = root.join(EVENTS_LIB);
    if !trait_defined(&events_path, "Session") {
        violations.push(format!(
            "{EVENTS_LIB}:1 `pub trait Session` not defined — declare it in the public-contract leaf crate",
        ));
    }

    let driver_dir = root.join(DRIVER_SRC);
    for path in rs_files_recursive(&driver_dir) {
        if let Some(lineno) = trait_def_line(&path, "Session") {
            violations.push(format!(
                "{}:{lineno} `pub trait Session` defined in loom-driver — move it to loom-events",
                rel(&root, &path),
            ));
        }
    }

    verdict_from(RULE, violations)
}

fn trait_defined(path: &Path, name: &str) -> bool {
    trait_def_line(path, name).is_some()
}

fn trait_def_line(path: &Path, name: &str) -> Option<usize> {
    let file = parse_rs(path)?;
    for item in &file.items {
        if let syn::Item::Trait(t) = item
            && t.ident == name
            && matches!(t.vis, syn::Visibility::Public(_))
        {
            return Some(t.span().start().line);
        }
    }
    None
}

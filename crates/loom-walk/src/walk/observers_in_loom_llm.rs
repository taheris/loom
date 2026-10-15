//! `DoomLoopObserver` and `DuplicateResultObserver` ship in
//! `llm`'s observer module so consumers driving via
//! `Conversation::run` get them by default. The walk asserts each
//! observer type is defined in `crates/loom-llm/src/` and not defined
//! a second time anywhere else in the workspace (which would split the
//! agent-safety nets across crates).

use std::path::Path;

use super::util::{parse_rs, rel, rs_files_recursive, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "observers_in_loom_llm — `DoomLoopObserver` and `DuplicateResultObserver` are defined in loom-llm's observer module, not duplicated elsewhere";

const LLM_SRC: &str = "crates/loom-llm/src";
const CRATES_DIR: &str = "crates";
const OBSERVERS: &[&str] = &["DoomLoopObserver", "DuplicateResultObserver"];

fn rule(input: &WalkInput) -> Verdict {
    let root = workspace_root(input);
    let mut violations = Vec::new();

    let llm_src = root.join(LLM_SRC);
    let crates_dir = root.join(CRATES_DIR);
    let llm_files = rs_files_recursive(input, &llm_src);

    for observer in OBSERVERS {
        let defined_in_llm = llm_files
            .iter()
            .filter(|p| defines_struct(input, p, observer))
            .map(|p| rel(&root, p))
            .next()
            .is_some();
        if !defined_in_llm {
            violations.push(format!(
                "{LLM_SRC}/observer.rs:1 `{observer}` not defined in loom-llm — it must ship in loom-llm's observer module",
            ));
        }

        for crate_dir in crate_dirs(input, &crates_dir) {
            if crate_dir == llm_src {
                continue;
            }
            for path in rs_files_recursive(input, &crate_dir) {
                if defines_struct(input, &path, observer) {
                    violations.push(format!(
                        "{}:1 `{observer}` also defined outside loom-llm — observers must not be duplicated",
                        rel(&root, &path),
                    ));
                }
            }
        }
    }

    verdict_from(RULE, violations)
}

fn defines_struct(input: &WalkInput, path: &Path, name: &str) -> bool {
    let Some(file) = parse_rs(input, path) else {
        return false;
    };
    item_defines(&file.items, name)
}

fn item_defines(items: &[syn::Item], name: &str) -> bool {
    for item in items {
        let hit = match item {
            syn::Item::Struct(s) => s.ident == name,
            syn::Item::Enum(e) => e.ident == name,
            syn::Item::Mod(m) => m
                .content
                .as_ref()
                .is_some_and(|(_, nested)| item_defines(nested, name)),
            _ => false,
        };
        if hit {
            return true;
        }
    }
    false
}

fn crate_dirs(input: &WalkInput, crates_dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for path in super::util::immediate_children(input, crates_dir) {
        let src = path.join("src");
        if super::util::is_dir(input, &src) {
            out.push(src);
        }
    }
    out.sort();
    out
}

/// Standalone and checked dispatch execute the identical rule body.
pub fn run(input: &WalkInput) -> Verdict {
    input.finish(rule(input))
}

super::source::registration!("observers_in_loom_llm");

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

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let mut violations = Vec::new();

    let llm_src = root.join(LLM_SRC);
    let crates_dir = root.join(CRATES_DIR);
    let llm_files = rs_files_recursive(&llm_src);

    for observer in OBSERVERS {
        let in_llm: Vec<String> = llm_files
            .iter()
            .filter(|p| defines_struct(p, observer))
            .map(|p| rel(&root, p))
            .collect();
        if in_llm.is_empty() {
            violations.push(format!(
                "{LLM_SRC}/observer.rs:1 `{observer}` not defined in loom-llm — it must ship in loom-llm's observer module",
            ));
        }

        for crate_dir in crate_dirs(&crates_dir) {
            if crate_dir == llm_src {
                continue;
            }
            for path in rs_files_recursive(&crate_dir) {
                if defines_struct(&path, observer) {
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

fn defines_struct(path: &Path, name: &str) -> bool {
    let Some(file) = parse_rs(path) else {
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

fn crate_dirs(crates_dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(crates_dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let src = path.join("src");
        if src.is_dir() {
            out.push(src);
        }
    }
    out.sort();
    out
}

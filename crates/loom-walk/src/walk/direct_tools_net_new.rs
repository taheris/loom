//! Direct registers six sandbox-aware tools — `Read`, `Write`, `Edit`,
//! `Bash`, `Grep`, `Glob` — and the spec is explicit that these are
//! **net-new implementations** in `loom-agent::direct::tools`, not
//! re-exports from another crate (Claude Code's tools are closed-source;
//! no code to share). The walk asserts each tool name has a struct
//! definition (or impl Tool for that type) somewhere under
//! `crates/loom-agent/src/direct/tools/`.

use std::collections::HashSet;
use std::path::Path;

use super::util::{parse_rs, rs_files_recursive, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "direct_tools_net_new — Read/Write/Edit/Bash/Grep/Glob defined in loom-agent::direct::tools, not re-exported";

const TOOLS_DIR: &str = "crates/loom-agent/src/direct/tools";
const REQUIRED: &[&str] = &["Read", "Write", "Edit", "Bash", "Grep", "Glob"];

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let tools_dir = root.join(TOOLS_DIR);
    let mut violations = Vec::new();

    let defined = collect_local_types(&tools_dir);
    for name in REQUIRED {
        if !defined.contains(*name) {
            violations.push(format!(
                "{TOOLS_DIR}/mod.rs:1 `{name}` has no struct/enum definition under `{TOOLS_DIR}/` — Direct tools must be net-new implementations, not re-exports",
            ));
        }
    }

    verdict_from(RULE, violations)
}

fn collect_local_types(dir: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
    for path in rs_files_recursive(dir) {
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
            syn::Item::Mod(m) => {
                if let Some((_, nested)) = &m.content {
                    collect_from_items(nested, out);
                }
            }
            _ => {}
        }
    }
}

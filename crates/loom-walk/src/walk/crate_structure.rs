//! Crate-structure sentinel: the seven named member crates from the
//! loom-harness spec must exist with their canonical files. `loom` is
//! the binary crate (`src/main.rs`); the rest are libraries
//! (`src/lib.rs`). Each carries a `Cargo.toml`.

use std::path::Path;

use super::util::{verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "crate_structure — the seven named crates must exist with Cargo.toml + lib/main";

const LIBRARY_CRATES: &[&str] = &[
    "loom-events",
    "loom-driver",
    "loom-render",
    "loom-agent",
    "loom-workflow",
    "loom-templates",
];

const BINARY_CRATE: &str = "loom";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let crates = root.join("crates");
    let mut violations = Vec::new();

    check_crate(&crates, BINARY_CRATE, "src/main.rs", &mut violations);
    for name in LIBRARY_CRATES {
        check_crate(&crates, name, "src/lib.rs", &mut violations);
    }

    verdict_from(RULE, violations)
}

fn check_crate(crates: &Path, name: &str, entry: &str, violations: &mut Vec<String>) {
    let dir = crates.join(name);
    if !dir.is_dir() {
        violations.push(format!("crates/{name}:1 missing crate directory"));
        return;
    }
    let manifest = dir.join("Cargo.toml");
    if !manifest.is_file() {
        violations.push(format!("crates/{name}/Cargo.toml:1 missing manifest"));
    }
    let entry_path = dir.join(entry);
    if !entry_path.is_file() {
        violations.push(format!("crates/{name}/{entry}:1 missing entry source"));
    }
}

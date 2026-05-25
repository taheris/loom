//! Architectural: `gix::*` and `Command::new("git")` may only appear
//! inside `loom-driver/src/git/`. Production code outside that module
//! reaches git through `GitClient`.

use std::path::Path;

use super::util::{
    is_comment, narrow_to_loom_files, read_to_string, rel, src_files, verdict_from, workspace_root,
};
use super::{Verdict, WalkInput};

const RULE: &str = "git_client_encapsulation — only loom-driver/src/git/ may use gix or `git` CLI";

const NEEDLE_CMD: &str = concat!("Command::new(\"", "git\")");
const PREFIX_USE_GIX: &str = concat!("use ", "gix");
const PREFIX_USE_ROOT_GIX: &str = concat!("use ::", "gix");
const PREFIX_EXTERN_GIX: &str = concat!("extern crate ", "gix");

pub fn run(input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let allowed_prefix = Path::new("crates/loom-driver/src/git/");
    let scope = narrow_to_loom_files(src_files(&root), input, &root);
    let mut violations = Vec::new();
    for path in scope {
        let rel_path = rel(&root, &path);
        if Path::new(&rel_path).starts_with(allowed_prefix) {
            continue;
        }
        let Some(body) = read_to_string(&path) else {
            continue;
        };
        for (lineno, line) in body.lines().enumerate() {
            if is_comment(line) {
                continue;
            }
            if line.contains(NEEDLE_CMD) {
                violations.push(format!(
                    "{}:{} `{}` — route through `loom_driver::git::GitClient`",
                    rel_path,
                    lineno + 1,
                    NEEDLE_CMD,
                ));
            }
            if has_gix_import(line) {
                violations.push(format!(
                    "{}:{} `gix` import — only `loom-driver/src/git/` may depend on `gix`",
                    rel_path,
                    lineno + 1,
                ));
            }
        }
    }
    verdict_from(RULE, violations)
}

fn has_gix_import(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with(PREFIX_USE_GIX)
        || trimmed.starts_with(PREFIX_USE_ROOT_GIX)
        || trimmed.starts_with(PREFIX_EXTERN_GIX)
}

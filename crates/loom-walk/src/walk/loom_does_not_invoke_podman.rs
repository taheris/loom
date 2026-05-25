//! Architectural: loom production source never spawns `podman`
//! directly. Both agent backends drive the wrapper via `wrapix spawn`
//! — the wrapper owns container construction and lifecycle. A future
//! refactor that bypassed the wrapper to call `podman` directly would
//! either reintroduce a `Command::new("podman")` site or drop the
//! `wrapix spawn` argv; this walk catches the negative half of that
//! contract. Comments and doc strings may legitimately mention podman
//! (e.g. explaining what the wrapper does on top), so the rule only
//! flags actual subprocess-spawn shapes.

use super::util::{
    is_comment, narrow_to_loom_files, read_to_string, rel, src_files, verdict_from, workspace_root,
};
use super::{Verdict, WalkInput};

const RULE: &str = "loom_does_not_invoke_podman — drive containers through `wrapix spawn`, never `podman` directly";

const NEEDLE_COMMAND_NEW: &str = concat!("Command::new(\"", "podman\"");
const NEEDLE_SPAWN: &str = concat!("spawn(\"", "podman\"");
const NEEDLE_EXEC: &str = concat!("exec(\"", "podman\"");

pub fn run(input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let scope = narrow_to_loom_files(src_files(&root), input, &root);
    let mut violations = Vec::new();
    let needles = [NEEDLE_COMMAND_NEW, NEEDLE_SPAWN, NEEDLE_EXEC];
    for path in scope {
        let Some(body) = read_to_string(&path) else {
            continue;
        };
        let rel_path = rel(&root, &path);
        for (lineno, line) in body.lines().enumerate() {
            if is_comment(line) {
                continue;
            }
            for needle in &needles {
                if line.contains(*needle) {
                    violations.push(format!(
                        "{}:{} `{}` — spawn through `wrapix spawn`, never `podman` directly",
                        rel_path,
                        lineno + 1,
                        needle,
                    ));
                    break;
                }
            }
        }
    }
    verdict_from(RULE, violations)
}

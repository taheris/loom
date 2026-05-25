//! The `loom` binary must NOT expose `sync` or `tune` subcommands.
//! Askama compiled templates make per-project sync unnecessary; tune
//! belongs to a separate (long-removed) workflow. The walk reads
//! `loom/src/main.rs` and flags any clap subcommand surface that names
//! `sync` or `tune`.

use super::util::{is_comment, read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str =
    "no_sync_or_tune_command — `loom` exposes neither `sync` nor `tune` as a subcommand";

const MAIN_RS: &str = "crates/loom/src/main.rs";

const FORBIDDEN_NAMES: &[&str] = &["sync", "tune"];

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let main_path = root.join(MAIN_RS);
    let mut violations = Vec::new();

    let Some(body) = read_to_string(&main_path) else {
        violations.push(format!("{MAIN_RS}:1 binary entry point not found"));
        return verdict_from(RULE, violations);
    };

    for (lineno, raw) in body.lines().enumerate() {
        if is_comment(raw) {
            continue;
        }
        let trimmed = raw.trim_start();
        for name in FORBIDDEN_NAMES {
            let needle_command_attr = format!("#[command(name = \"{name}\")]");
            if trimmed.contains(&needle_command_attr) {
                violations.push(format!(
                    "{MAIN_RS}:{} forbidden subcommand `{}` declared via #[command(name = ...)]",
                    lineno + 1,
                    name,
                ));
                continue;
            }
            let capitalised = capitalise(name);
            let bare = format!("{capitalised},");
            let bare_braced = format!("{capitalised} {{");
            let bare_parens = format!("{capitalised}(");
            if trimmed.starts_with(&bare)
                || trimmed.starts_with(&bare_braced)
                || trimmed.starts_with(&bare_parens)
            {
                violations.push(format!(
                    "{MAIN_RS}:{} forbidden subcommand variant `{}` — drop the `{}` surface",
                    lineno + 1,
                    capitalised,
                    name,
                ));
            }
        }
    }

    verdict_from(RULE, violations)
}

fn capitalise(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

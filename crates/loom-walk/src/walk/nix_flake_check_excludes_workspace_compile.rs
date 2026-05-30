//! Fast-tier composition (pre-commit spec): `nix flake check` excludes
//! the workspace Rust compile chain. The `loom.bin`, `loom.clippy`,
//! and `loom.nextest` derivations move out of `checks.<system>` so
//! `nix flake check` can hit the sub-10s warm target; their work runs
//! as per-hook pre-push entries against the host's warm cache.

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "nix_flake_check_excludes_workspace_compile — nix/flake/checks.nix must not expose loom.bin / loom.clippy / loom.nextest";

const CHECKS_REL: &str = "nix/flake/checks.nix";
const BANNED: &[&str] = &["bin", "clippy", "nextest"];

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let path = root.join(CHECKS_REL);
    let Some(body) = read_to_string(&path) else {
        return Verdict {
            pass: false,
            evidence: format!("{CHECKS_REL}: file not readable\n{RULE}"),
        };
    };

    let mut violations = Vec::new();
    for (lineno, raw) in body.lines().enumerate() {
        let line = raw.trim_start();
        if line.starts_with('#') {
            continue;
        }
        if let Some(idx) = line.find("inherit (loom)") {
            let rest = &line[idx + "inherit (loom)".len()..];
            let tail = rest.split(';').next().unwrap_or("");
            let inherited: Vec<&str> = tail
                .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
                .filter(|t| !t.is_empty())
                .collect();
            for name in &inherited {
                if BANNED.contains(name) {
                    violations.push(format!(
                        "{}:{} `inherit (loom) {}` exposes the workspace compile under `flake check` — move it to a pre-push hook",
                        CHECKS_REL,
                        lineno + 1,
                        name,
                    ));
                }
            }
        }
        for name in BANNED {
            let needle = format!("loom.{name}");
            if line.contains(&needle) {
                violations.push(format!(
                    "{}:{} `{}` exposes the workspace compile under `flake check` — move it to a pre-push hook",
                    CHECKS_REL,
                    lineno + 1,
                    needle,
                ));
            }
        }
    }
    verdict_from(RULE, violations)
}

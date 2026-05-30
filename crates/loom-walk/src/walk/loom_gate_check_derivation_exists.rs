//! Fast-tier composition (pre-commit spec): `nix/flake/checks.nix`
//! exposes a `loom gate check` derivation distinct from the slow-tier
//! per-hook entries. That derivation is what `nix flake check` runs
//! against the `[check]`-tier verifiers + integrity gate +
//! surface-conformance audit under the sub-10s warm budget.

use super::util::{read_to_string, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "loom_gate_check_derivation_exists — nix/flake/checks.nix must declare a derivation that runs `loom gate check`";

const CHECKS_REL: &str = "nix/flake/checks.nix";
const NEEDLE: &str = "loom gate check";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let path = root.join(CHECKS_REL);
    let Some(body) = read_to_string(&path) else {
        return Verdict {
            pass: false,
            evidence: format!("{CHECKS_REL}: file not readable\n{RULE}"),
        };
    };

    if body.contains(NEEDLE) {
        return Verdict {
            pass: true,
            evidence: RULE.to_string(),
        };
    }
    Verdict {
        pass: false,
        evidence: format!(
            "{CHECKS_REL}: no derivation runs `{NEEDLE}` — add a flake-check derivation that invokes it\n{RULE}"
        ),
    }
}

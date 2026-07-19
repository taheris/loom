//! Static contract for the Nix test, smoke, and on-demand fuzz surfaces.

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "test_nix_surface_contract — test tiers, platform smoke branches, and on-demand fuzz wiring stay composed";

struct FileContract {
    path: &'static str,
    required: &'static [&'static str],
}

const CONTRACTS: &[FileContract] = &[
    FileContract {
        path: "flake.nix",
        required: &[
            "\"aarch64-darwin\"",
            "\"aarch64-linux\"",
            "\"x86_64-darwin\"",
            "\"x86_64-linux\"",
        ],
    },
    FileContract {
        path: "nix/flake/lib.nix",
        required: &[
            "smokeSandbox = wrixLib.mkSandbox",
            "agentPkg = smokeMockPi;",
            "smokeProfileManifest = wrixLib.mkProfileImages",
        ],
    },
    FileContract {
        path: "nix/flake/tests.nix",
        required: &["packages.loom-tests = testsDeriv.rustChecks.loom-tests;"],
    },
    FileContract {
        path: "nix/workspace.nix",
        required: &["\"flake.nix\" = \"${src}/flake.nix\";"],
    },
    FileContract {
        path: "nix/flake/apps.nix",
        required: &[
            "test = {",
            "smoke = {",
            "fuzz-loom = {",
            "text = builtins.readFile ../../scripts/full-test.sh;",
        ],
    },
    FileContract {
        path: "tests/default.nix",
        required: &["loom-tests = loomDeriv.loomTests;", "loom-smoke"],
    },
    FileContract {
        path: "tests/loom/default.nix",
        required: &[
            "loom gate check --tree",
            "loom gate test --tree",
            "pkgs.prek",
            "LOOM_TEST_PROFILE_CONFIG",
            "LOOM_WRIX_SPAWN_BIN",
            "optionalAttrs isLinux",
            "optionalAttrs (!isLinux)",
            "container smoke not available on Darwin",
        ],
    },
    FileContract {
        path: "scripts/full-test.sh",
        required: &[
            "nix flake check --no-warn-dirty",
            "cargo clippy --workspace --all-targets -- -D warnings",
            "cargo nextest run --workspace",
            "loom gate system --tree",
        ],
    },
    FileContract {
        path: "tests/run-tests.sh",
        required: &[
            "LOOM_TEST_PROFILE_CONFIG",
            "unset WRIX_AGENT",
            "--agent pi loop \"$BEAD_ID\"",
            "if [[ \"$ELAPSED\" -gt 30 ]]",
        ],
    },
];

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let mut violations = Vec::new();
    for contract in CONTRACTS {
        let Some(body) = read_to_string(&root.join(contract.path)) else {
            violations.push(format!("{}: file not readable", contract.path));
            continue;
        };
        for required in contract.required {
            if !active_line_contains(&body, required) {
                violations.push(format!(
                    "{}: missing active test-surface wiring `{required}`",
                    contract.path,
                ));
            }
        }
    }

    let checks_path = "nix/flake/checks.nix";
    match read_to_string(&root.join(checks_path)) {
        Some(body) => {
            for forbidden in ["loom-tests", "fuzz-loom", "loomTests"] {
                if active_line_contains(&body, forbidden) {
                    violations.push(format!(
                        "{checks_path}: `{forbidden}` belongs outside flake checks",
                    ));
                }
            }
        }
        None => violations.push(format!("{checks_path}: file not readable")),
    }

    verdict_from(RULE, violations)
}

fn active_line_contains(body: &str, needle: &str) -> bool {
    body.lines()
        .map(str::trim_start)
        .any(|line| !line.starts_with('#') && line.contains(needle))
}

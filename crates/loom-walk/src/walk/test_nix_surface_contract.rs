//! Static contract for the Nix test, smoke, and on-demand fuzz surfaces.

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "test_nix_surface_contract — test tiers, platform smoke branches, and on-demand fuzz wiring stay composed";

struct FileContract {
    path: &'static str,
    required: &'static [&'static str],
}

const CHECKS_PATH: &str = "nix/flake/checks.nix";

/// Execution and declared reads share one source of truth, including mock-agent scripts.
pub(super) fn inputs(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    CONTRACTS
        .iter()
        .map(|contract| contract.path)
        .chain(std::iter::once(CHECKS_PATH))
        .map(|path| root.join(path))
        .collect()
}

const CONTRACTS: &[FileContract] = &[
    FileContract {
        path: "flake.nix",
        required: &[
            "\"aarch64-darwin\"",
            "\"aarch64-linux\"",
            "\"x86_64-linux\"",
        ],
    },
    FileContract {
        path: "nix/flake/lib.nix",
        required: &[
            "smokeWrixLib = wrixModules.library",
            "nix = import ../native-smoke-nix.nix",
            "smokeSandbox = smokeWrixLib.mkSandbox",
            "agentPkg = smokeMockPi;",
            "smokeProfileManifest = wrixLib.mkProfileImages",
            "nextestOverlay = import ../nextest-overlay.nix;",
            "overlays = [ nextestOverlay ];",
        ],
    },
    FileContract {
        path: "nix/native-smoke-nix.nix",
        required: &[
            "nix = import ./native-container-nix.nix",
            "${nix}/bin/nix \"$@\" > \"$document\"",
            "exec ${restriction}/bin/deny-namespaces ${nix}/bin/nix \"$@\"",
            "exec ${nix}/bin/nix \"$@\"",
        ],
    },
    FileContract {
        path: "nix/nextest-overlay.nix",
        required: &[
            "cargo-nextest = prev.cargo-nextest.overrideAttrs",
            "./patches/nextest-capture-handoff.patch",
            "doCheck = true;",
            "nextest run --locked --cargo-profile release",
            "test(capture_closes_while_unrelated_sibling_is_paused_before_exec)",
        ],
    },
    FileContract {
        path: "nix/flake/tests.nix",
        required: &["packages.loom-tests = testsDeriv.rustChecks.loom-tests;"],
    },
    FileContract {
        path: "nix/workspace.nix",
        required: &[
            "nextest = checked.cargo {",
            "extraResources =",
            "checked.resource {",
            "\"flake.nix\"",
            "\"nix\"",
            "extraSrcs = builtins.listToAttrs",
            "cp -r ${path}",
        ],
    },
    FileContract {
        path: "nix/flake/apps.nix",
        required: &[
            "test = app testApp \"test\"",
            "test-required = app requiredTestApp \"test-required\"",
            "smoke = app smokeApp \"smoke\"",
            "fuzz-loom = app fuzzApp \"fuzz-loom\"",
            "smokePrekHooks = wrixLib.prekHooks;",
            "fullScript = source \"scripts/full-test.sh\";",
            "requiredScript = source \"scripts/required-test.sh\";",
            "text = builtins.readFile fullScript.path;",
            "text = builtins.readFile requiredScript.path;",
            "evaluation = [ fullScript ];",
            "evaluation = [ requiredScript ];",
        ],
    },
    FileContract {
        path: "tests/default.nix",
        required: &["loom-tests = loomDeriv.loomTests;", "loom-smoke"],
    },
    FileContract {
        path: "tests/loom/default.nix",
        required: &[
            "loomTests = loomPackage.nextest;",
            "pkgs.prek",
            "LOOM_TEST_PROFILE_CONFIG",
            "LOOM_TEST_PRE_PUSH_CHECKS",
            "export LOOM_TEST_LOOP=${../smoke/loop.sh}",
            "export LOOM_TEST_WAIT_DOLT=${../smoke/wait-dolt.sh}",
            "LOOM_WRIX_BIN",
            "LOOM_WRIX_SPAWN_BIN",
            "WRIX_PREK_HOOKS",
            "optionalAttrs isLinux",
            "optionalAttrs (!isLinux)",
            "container smoke not available on Darwin",
            "exit 69",
        ],
    },
    FileContract {
        path: "scripts/full-test.sh",
        required: &[
            "nix flake check --no-warn-dirty",
            "cargo clippy --workspace -- -D warnings",
            "cargo clippy --workspace --all-targets -- -D warnings",
            "cargo nextest run --workspace",
        ],
    },
    FileContract {
        path: "scripts/required-test.sh",
        required: &[
            "export GIT_CONFIG_GLOBAL=\"$repo_root/tests/fixtures/git/test-gitconfig\"",
            "export GIT_CONFIG_SYSTEM=/dev/null",
            "unset WRIX_SIGNING_KEY",
            "cargo nextest run --workspace",
        ],
    },
    FileContract {
        path: "tests/run-tests.sh",
        required: &[
            "LOOM_TEST_PROFILE_CONFIG",
            "LOOM_TEST_PRE_PUSH_CHECKS",
            "unset WRIX_AGENT",
            "bash \"${LOOM_TEST_WAIT_DOLT:?missing Dolt readiness helper}\" \"$LOOM_WRIX_SERVICE_BIN\"",
            "bash \"$LOOM_TEST_LOOP\" \"$LOOM_BIN\" \"$WORKSPACE\" \"$BEAD_ID\" \"$START_TS\"",
            "WRIX_DEPLOY_KEY",
            "WRIX_SIGNING_KEY",
            "WRIX_PI_AUTH_FILE",
            ".pre-commit-config.yaml",
            ".loom/",
            "bin/pre-push-checks --hook-id smoke-noop",
        ],
    },
    FileContract {
        path: "tests/smoke/host-gate-test.sh",
        required: &["extra-experimental-features = nix-command flakes"],
    },
    FileContract {
        path: "tests/smoke/loop.sh",
        required: &["--agent pi loop \"$BEAD_ID\""],
    },
    FileContract {
        path: "tests/mock-pi/pi.sh",
        required: &["git config --get commit.gpgsign", "bd close \"$bead_id\""],
    },
];

fn rule(input: &WalkInput) -> Verdict {
    let root = workspace_root(input);
    let mut violations = Vec::new();
    for contract in CONTRACTS {
        let Some(body) = read_to_string(input, &root.join(contract.path)) else {
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

    let checks_path = CHECKS_PATH;
    match read_to_string(input, &root.join(checks_path)) {
        Some(body) => {
            let archive_runner = body
                .split_once("llmCompatProgram =")
                .and_then(|(_, tail)| tail.split_once("llm-compat-report ="))
                .map(|(runner, _)| runner);
            for required in [
                "mkdir -p \"$HOME/tests\"",
                "export SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt",
                "export NIX_SSL_CERT_FILE=\"$SSL_CERT_FILE\"",
            ] {
                if !archive_runner.is_some_and(|runner| active_line_contains(runner, required)) {
                    violations.push(format!(
                        "{checks_path}: archive runner is missing `{required}`"
                    ));
                }
            }
            for forbidden in ["loom-tests", "fuzz-loom", "loomTests"] {
                if active_wiring_contains(&body, forbidden) {
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

// Assertions inspect package metadata; they do not wire a package into checks.
// Actual check keys are also asserted by the Nix native-stage-integrity check.
fn active_wiring_contains(body: &str, needle: &str) -> bool {
    body.lines().map(str::trim_start).any(|line| {
        if line.starts_with('#') {
            return false;
        }
        let wiring = if line.starts_with("assert ") {
            line.split_once(';').map_or(line, |(_, rest)| rest)
        } else {
            line
        };
        wiring.contains(needle)
    })
}

#[cfg(test)]
mod tests {
    use super::active_wiring_contains;

    #[test]
    fn registered_nix_surface_walk_declares_its_mock_agent_and_runner_reads() {
        let root = std::path::Path::new("/source");
        let walk = crate::walk::lookup("test_nix_surface_contract").unwrap();
        let inputs = (walk.inputs)(root, &crate::walk::WalkInput::default());
        for path in [
            "tests/mock-pi/pi.sh",
            "tests/smoke/host-gate-test.sh",
            "nix/workspace.nix",
            "nix/flake/checks.nix",
            "nix/native-smoke-nix.nix",
        ] {
            assert!(
                inputs.contains(&root.join(path)),
                "missing declared read: {path}"
            );
        }
    }

    #[test]
    fn metadata_assertions_are_not_suite_bindings_but_real_wiring_is_rejected() {
        assert!(!active_wiring_contains(
            "assert config.packages.loom-tests.drvPath == loom.nextest.drvPath;",
            "loom-tests",
        ));
        for wiring in [
            "loom-tests = suite;",
            "renamed-suite = packages.loom-tests;",
            "inherit (packages) loom-tests;",
            "assert true; packages.loom-tests;",
        ] {
            assert!(active_wiring_contains(wiring, "loom-tests"));
        }
        assert!(!active_wiring_contains(
            "# loom-tests is on demand",
            "loom-tests"
        ));
    }
}

fn active_line_contains(body: &str, needle: &str) -> bool {
    body.lines()
        .map(str::trim_start)
        .any(|line| !line.starts_with('#') && line.contains(needle))
}

/// Standalone and checked dispatch execute the identical rule body.
pub fn run(input: &WalkInput) -> Verdict {
    input.finish(rule(input))
}

super::source::registration!("test_nix_surface_contract");

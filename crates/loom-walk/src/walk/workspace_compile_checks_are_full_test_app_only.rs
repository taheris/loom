//! Workspace compile coverage split between fast flake checks and the full test app.
//!
//! `nix flake check` stays on the fast derivation tier. Full workspace
//! clippy and nextest remain shared cargo-artifact derivations, but the
//! required user-facing surface for running them is `nix run .#test`.

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "workspace_compile_checks_are_full_test_app_only — nix flake checks omit workspace clippy/nextest; nix run .#test runs clippy, nextest, and system verifiers";

const APPS_REL: &str = "nix/flake/apps.nix";
const CHECKS_REL: &str = "nix/flake/checks.nix";
const WORKSPACE_REL: &str = "nix/workspace.nix";

const FORBIDDEN_CHECK_TOKENS: &[&str] = &["loom-clippy", "loom-nextest", "clippy", "nextest"];

struct RequiredAppSnippet {
    label: &'static str,
    needle: &'static str,
}

const REQUIRED_APP_SNIPPETS: &[RequiredAppSnippet] = &[
    RequiredAppSnippet {
        label: "test app",
        needle: "name = \"test\";",
    },
    RequiredAppSnippet {
        label: "fast flake tier",
        needle: "nix flake check --no-warn-dirty",
    },
    RequiredAppSnippet {
        label: "workspace clippy",
        needle: "cargo clippy --workspace --all-targets -- -D warnings",
    },
    RequiredAppSnippet {
        label: "full nextest",
        needle: "cargo nextest run --workspace",
    },
    RequiredAppSnippet {
        label: "system verifiers",
        needle: "loom gate system --tree",
    },
];

struct RequiredDerivation {
    attr: &'static str,
    builder: &'static str,
}

const REQUIRED_DERIVATIONS: &[RequiredDerivation] = &[
    RequiredDerivation {
        attr: "clippy",
        builder: "cargoClippy",
    },
    RequiredDerivation {
        attr: "nextest",
        builder: "cargoNextest",
    },
];

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let checks_path = root.join(CHECKS_REL);
    let Some(checks_body) = read_to_string(&checks_path) else {
        return Verdict {
            pass: false,
            evidence: format!("{CHECKS_REL}: file not readable\n{RULE}"),
        };
    };
    let workspace_path = root.join(WORKSPACE_REL);
    let Some(workspace_body) = read_to_string(&workspace_path) else {
        return Verdict {
            pass: false,
            evidence: format!("{WORKSPACE_REL}: file not readable\n{RULE}"),
        };
    };
    let apps_path = root.join(APPS_REL);
    let Some(apps_body) = read_to_string(&apps_path) else {
        return Verdict {
            pass: false,
            evidence: format!("{APPS_REL}: file not readable\n{RULE}"),
        };
    };

    let mut violations = Vec::new();
    check_flake_checks_omit_compile_surfaces(&checks_body, &mut violations);
    check_cargo_artifacts_source(&workspace_body, &mut violations);
    for required in REQUIRED_DERIVATIONS {
        check_workspace_derivation(&workspace_body, required, &mut violations);
    }
    check_full_test_app(&apps_body, &mut violations);
    verdict_from(RULE, violations)
}

fn check_flake_checks_omit_compile_surfaces(body: &str, violations: &mut Vec<String>) {
    for (idx, raw) in body.lines().enumerate() {
        let code = code_before_comment(raw);
        for token in code_tokens(code) {
            if FORBIDDEN_CHECK_TOKENS.contains(&token) {
                violations.push(format!(
                    "{CHECKS_REL}:{} flake checks must not expose workspace compile surface `{token}`; run it via `nix run .#test`",
                    idx + 1,
                ));
            }
        }
    }
}

fn check_full_test_app(body: &str, violations: &mut Vec<String>) {
    for required in REQUIRED_APP_SNIPPETS {
        if body.contains(required.needle) {
            continue;
        }
        violations.push(format!(
            "{APPS_REL}: missing {} command `{}` in the `nix run .#test` full-suite app",
            required.label, required.needle,
        ));
    }
    for (idx, raw) in body.lines().enumerate() {
        let code = code_before_comment(raw);
        if code_tokens(code).any(|token| token == "test-ci") {
            violations.push(format!(
                "{APPS_REL}:{} `test-ci` app surface is obsolete; use `nix run .#test`",
                idx + 1,
            ));
        }
    }
}

fn check_cargo_artifacts_source(body: &str, violations: &mut Vec<String>) {
    let has_shared_artifacts = body.lines().any(|raw| {
        code_before_comment(raw)
            .trim()
            .contains("cargoArtifacts = craneLib.buildDepsOnly commonArgs;")
    });
    if has_shared_artifacts {
        return;
    }

    match first_assignment_line(body, "cargoArtifacts") {
        Some(line) => violations.push(format!(
            "{WORKSPACE_REL}:{line} cargoArtifacts must be built once with craneLib.buildDepsOnly commonArgs",
        )),
        None => violations.push(format!(
            "{WORKSPACE_REL}: cargoArtifacts derivation is missing",
        )),
    }
}

fn check_workspace_derivation(
    body: &str,
    required: &RequiredDerivation,
    violations: &mut Vec<String>,
) {
    let Some((line, block)) = derivation_block(body, required.attr, required.builder) else {
        match first_assignment_line(body, required.attr) {
            Some(line) => violations.push(format!(
                "{WORKSPACE_REL}:{line} {} must be built with craneLib.{}",
                required.attr, required.builder,
            )),
            None => violations.push(format!(
                "{WORKSPACE_REL}: {} derivation is missing",
                required.attr,
            )),
        }
        return;
    };

    let inherits_cargo_artifacts = block.lines().any(|raw| {
        code_before_comment(raw)
            .trim()
            .contains("inherit cargoArtifacts;")
    });
    if !inherits_cargo_artifacts {
        violations.push(format!(
            "{WORKSPACE_REL}:{line} {} must inherit the shared cargoArtifacts cache",
            required.attr,
        ));
    }
}

fn derivation_block(body: &str, attr: &str, builder: &str) -> Option<(usize, String)> {
    let assignment_prefix = format!("{attr} =");
    let builder_needle = format!("craneLib.{builder}");
    let mut start_line = None;
    let mut block = String::new();

    for (idx, raw) in body.lines().enumerate() {
        let code = code_before_comment(raw).trim_start();
        if start_line.is_none() {
            if code.starts_with(&assignment_prefix) && code.contains(&builder_needle) {
                start_line = Some(idx + 1);
            } else {
                continue;
            }
        }
        block.push_str(raw);
        block.push('\n');
        if code.trim() == ");" {
            break;
        }
    }

    start_line.map(|line| (line, block))
}

fn first_assignment_line(body: &str, attr: &str) -> Option<usize> {
    let prefix = format!("{attr} =");
    body.lines().enumerate().find_map(|(idx, raw)| {
        let code = code_before_comment(raw).trim_start();
        code.starts_with(&prefix).then_some(idx + 1)
    })
}

fn code_tokens(code: &str) -> impl Iterator<Item = &str> {
    code.split(|character: char| {
        !(character.is_ascii_alphanumeric() || character == '_' || character == '-')
    })
    .filter(|token| !token.is_empty())
}

fn code_before_comment(raw: &str) -> &str {
    match raw.split_once('#') {
        Some((code, _comment)) => code,
        None => raw,
    }
}

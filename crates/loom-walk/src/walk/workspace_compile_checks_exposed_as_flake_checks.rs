//! Flake-check exposure for workspace Rust compile coverage.
//!
//! The `loom-clippy` and `loom-nextest` checks must be aliases of the
//! workspace derivations built in `nix/workspace.nix`; those derivations
//! share the `cargoArtifacts` dependency build so `nix flake check` does
//! not compile the dependency graph twice.

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "workspace_compile_checks_exposed_as_flake_checks — nix flake checks must expose loom-clippy and loom-nextest from shared cargoArtifacts workspace derivations";

const CHECKS_REL: &str = "nix/flake/checks.nix";
const WORKSPACE_REL: &str = "nix/workspace.nix";

struct RequiredCheck {
    check_attr: &'static str,
    workspace_attr: &'static str,
}

const REQUIRED_CHECKS: &[RequiredCheck] = &[
    RequiredCheck {
        check_attr: "loom-clippy",
        workspace_attr: "clippy",
    },
    RequiredCheck {
        check_attr: "loom-nextest",
        workspace_attr: "nextest",
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

    let mut violations = Vec::new();
    for required in REQUIRED_CHECKS {
        check_flake_binding(&checks_body, required, &mut violations);
    }
    check_cargo_artifacts_source(&workspace_body, &mut violations);
    for required in REQUIRED_DERIVATIONS {
        check_workspace_derivation(&workspace_body, required, &mut violations);
    }
    verdict_from(RULE, violations)
}

fn check_flake_binding(body: &str, required: &RequiredCheck, violations: &mut Vec<String>) {
    let compact = compact_without_comments(body);
    let local_binding = format!("{}={};", required.check_attr, required.workspace_attr,);
    let dotted_binding = format!("{}=loom.{};", required.check_attr, required.workspace_attr,);
    if compact.contains(&local_binding) || compact.contains(&dotted_binding) {
        return;
    }

    match first_non_comment_line_containing(body, required.check_attr) {
        Some(line) => violations.push(format!(
            "{CHECKS_REL}:{line} checks.{} is not wired to the shared loom.{} derivation",
            required.check_attr, required.workspace_attr,
        )),
        None => violations.push(format!(
            "{CHECKS_REL}: checks.{} is missing from flake checks",
            required.check_attr,
        )),
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

fn first_non_comment_line_containing(body: &str, needle: &str) -> Option<usize> {
    body.lines().enumerate().find_map(|(idx, raw)| {
        let code = code_before_comment(raw);
        code.contains(needle).then_some(idx + 1)
    })
}

fn compact_without_comments(body: &str) -> String {
    let mut out = String::new();
    for raw in body.lines() {
        for part in code_before_comment(raw).split_whitespace() {
            out.push_str(part);
        }
    }
    out
}

fn code_before_comment(raw: &str) -> &str {
    match raw.split_once('#') {
        Some((code, _comment)) => code,
        None => raw,
    }
}

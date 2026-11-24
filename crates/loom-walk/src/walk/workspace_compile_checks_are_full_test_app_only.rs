//! Workspace compile coverage split between fast flake checks and the full test app.
//!
//! `nix flake check` stays on the fast derivation tier. Full workspace
//! clippy and nextest remain shared cargo-artifact derivations, but the
//! required user-facing surface for running them is `nix run .#test`.

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "workspace_compile_checks_are_full_test_app_only — nix flake checks omit workspace clippy/nextest; nix run .#test runs clippy and nextest without recursive whole-tree Gate execution";

const APPS_REL: &str = "nix/flake/apps.nix";
const CHECKS_REL: &str = "nix/flake/checks.nix";
const FULL_TEST_SCRIPT_REL: &str = "scripts/full-test.sh";
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
        label: "full test script",
        needle: "text = builtins.readFile ../../scripts/full-test.sh;",
    },
];

const REQUIRED_SCRIPT_SNIPPETS: &[RequiredAppSnippet] = &[
    RequiredAppSnippet {
        label: "hermetic global Git config",
        needle: "export GIT_CONFIG_GLOBAL=\"$repo_root/tests/fixtures/git/test-gitconfig\"",
    },
    RequiredAppSnippet {
        label: "disabled system Git config",
        needle: "export GIT_CONFIG_SYSTEM=/dev/null",
    },
    RequiredAppSnippet {
        label: "disabled host signing-key environment",
        needle: "unset WRIX_SIGNING_KEY",
    },
    RequiredAppSnippet {
        label: "fast flake tier",
        needle: "nix flake check --no-warn-dirty",
    },
    RequiredAppSnippet {
        label: "production clippy",
        needle: "cargo clippy --workspace -- -D warnings",
    },
    RequiredAppSnippet {
        label: "workspace clippy",
        needle: "cargo clippy --workspace --all-targets -- -D warnings",
    },
    RequiredAppSnippet {
        label: "full nextest",
        needle: "cargo nextest run --workspace",
    },
];

struct RequiredDerivation {
    attr: &'static str,
    builder: &'static str,
    operation: &'static str,
}

const REQUIRED_DERIVATIONS: &[RequiredDerivation] = &[
    RequiredDerivation {
        attr: "clippy",
        builder: "cargoClippy",
        operation: "lint",
    },
    RequiredDerivation {
        attr: "nextest",
        builder: "cargoNextest",
        operation: "test",
    },
];

fn rule(input: &WalkInput) -> Verdict {
    let root = workspace_root(input);
    let checks_path = root.join(CHECKS_REL);
    let Some(checks_body) = read_to_string(input, &checks_path) else {
        return Verdict {
            pass: false,
            evidence: format!("{CHECKS_REL}: file not readable\n{RULE}"),
        };
    };
    let workspace_path = root.join(WORKSPACE_REL);
    let Some(workspace_body) = read_to_string(input, &workspace_path) else {
        return Verdict {
            pass: false,
            evidence: format!("{WORKSPACE_REL}: file not readable\n{RULE}"),
        };
    };
    let apps_path = root.join(APPS_REL);
    let Some(apps_body) = read_to_string(input, &apps_path) else {
        return Verdict {
            pass: false,
            evidence: format!("{APPS_REL}: file not readable\n{RULE}"),
        };
    };
    let full_test_script_path = root.join(FULL_TEST_SCRIPT_REL);
    let Some(full_test_script_body) = read_to_string(input, &full_test_script_path) else {
        return Verdict {
            pass: false,
            evidence: format!("{FULL_TEST_SCRIPT_REL}: file not readable\n{RULE}"),
        };
    };

    let mut violations = Vec::new();
    check_flake_checks_omit_compile_surfaces(&checks_body, &mut violations);
    check_cargo_artifacts_source(&workspace_body, &mut violations);
    for required in REQUIRED_DERIVATIONS {
        check_workspace_derivation(&workspace_body, required, &mut violations);
    }
    check_full_test_app(&apps_body, &full_test_script_body, &mut violations);
    verdict_from(RULE, violations)
}

fn check_flake_checks_omit_compile_surfaces(body: &str, violations: &mut Vec<String>) {
    let mut assertion = false;
    let mut command = String::new();
    let mut line = 0;
    for (idx, raw) in body.lines().enumerate() {
        let mut code = code_before_comment(raw).trim();
        if assertion || code == "assert" || code.starts_with("assert ") {
            assertion = true;
            let Some((_, tail)) = code.split_once(';') else {
                continue;
            };
            assertion = false;
            code = tail;
        }
        if command.is_empty() {
            line = idx + 1;
        }
        command.push_str(code.trim_end_matches('\\'));
        if code.ends_with('\\') {
            command.push(' ');
            continue;
        }
        for (offset, token) in code_token_spans(&command) {
            let tail = &command[offset + token.len()..];
            if FORBIDDEN_CHECK_TOKENS.contains(&token)
                && !is_tool_probe(token, tail)
                && !is_scoped_archive_command(token, tail)
            {
                violations.push(format!(
                    "{CHECKS_REL}:{line} flake checks must not expose workspace compile surface `{token}`; run it via `nix run .#test`",
                ));
            }
        }
        command.clear();
    }
}

fn is_scoped_archive_command(token: &str, tail: &str) -> bool {
    if token != "nextest" {
        return false;
    }
    let tail = shell_segment(tail);
    let words: Vec<_> = tail
        .split_whitespace()
        .map(|word| word.trim_matches(['\'', '"']))
        .collect();
    if words
        .iter()
        .any(|word| matches!(*word, "--workspace" | "--all" | "--all-packages"))
    {
        return false;
    }
    match words.first().copied() {
        Some("archive") => words.windows(2).any(|pair| {
            matches!(pair[0], "-p" | "--package")
                && !pair[1].is_empty()
                && pair[1]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        }),
        Some("run") => {
            words.contains(&"--archive-file")
                && words
                    .iter()
                    .any(|word| matches!(*word, "-E" | "--filter-expr"))
                && tail.contains("test(=")
        }
        _ => false,
    }
}

fn shell_segment(tail: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    for (offset, character) in tail.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
        } else if quote == Some(character) {
            quote = None;
        } else if quote.is_none() {
            if matches!(character, '\'' | '"') {
                quote = Some(character);
            } else if matches!(character, ';' | '|' | '&') {
                return &tail[..offset];
            }
        }
    }
    tail
}

fn check_full_test_app(apps_body: &str, script_body: &str, violations: &mut Vec<String>) {
    for required in REQUIRED_APP_SNIPPETS {
        let captured_script = required.label == "full test script"
            && apps_body.contains("fullScript = source \"scripts/full-test.sh\";")
            && apps_body.contains("text = builtins.readFile fullScript.path;")
            && apps_body.contains("evaluation = [ fullScript ];");
        if apps_body.contains(required.needle) || captured_script {
            continue;
        }
        violations.push(format!(
            "{APPS_REL}: missing {} declaration `{}` in the `nix run .#test` full-suite app",
            required.label, required.needle,
        ));
    }
    for required in REQUIRED_SCRIPT_SNIPPETS {
        if script_body.contains(required.needle) {
            continue;
        }
        violations.push(format!(
            "{FULL_TEST_SCRIPT_REL}: missing {} command `{}` in the `nix run .#test` full-suite app",
            required.label, required.needle,
        ));
    }
    for (idx, raw) in script_body.lines().enumerate() {
        if code_before_comment(raw).trim() == "loom gate system --tree" {
            violations.push(format!("{FULL_TEST_SCRIPT_REL}:{} duplicates ordinary verification with recursive whole-tree Gate execution", idx + 1));
        }
    }
    for (idx, raw) in apps_body.lines().enumerate() {
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
    let composed_source = derivation_block(body, "cargoArtifacts", "craneLib.buildDepsOnly")
        .is_some_and(|(_, block)| {
            [
                "commonArgs",
                "src = lib.cleanSourceWith {",
                "filter = srcFilter;",
            ]
            .iter()
            .all(|expected| {
                block
                    .lines()
                    .any(|line| code_before_comment(line).trim() == *expected)
            })
        });
    let profiled_source = derivation_block(
        body,
        "dependencyArtifacts",
        "CARGO_PROFILE: craneLib.buildDepsOnly",
    )
    .is_some_and(|(_, block)| {
        [
            "commonArgs",
            "// optionalAttrs (CARGO_PROFILE != \"release\") { inherit CARGO_PROFILE; }",
            "src = lib.cleanSourceWith {",
            "filter = srcFilter;",
        ]
        .iter()
        .all(|expected| has_code_line(&block, expected))
    });
    let release_seed =
        profiled_source && has_code_line(body, "cargoArtifacts = dependencyArtifacts \"release\";");
    if !(has_shared_artifacts || composed_source || release_seed) {
        match first_assignment_line(body, "cargoArtifacts") {
            Some(line) => violations.push(format!(
                "{WORKSPACE_REL}:{line} cargoArtifacts must be built once with craneLib.buildDepsOnly commonArgs for release",
            )),
            None => violations.push(format!(
                "{WORKSPACE_REL}: cargoArtifacts derivation is missing",
            )),
        }
    }
    if first_assignment_line(body, "testCargoArtifacts").is_some()
        && !(profiled_source
            && has_code_line(body, "testCargoArtifacts = dependencyArtifacts \"dev\";"))
    {
        violations.push(format!(
            "{WORKSPACE_REL}: testCargoArtifacts must use the shared dev dependency constructor",
        ));
    }
}

fn has_code_line(body: &str, expected: &str) -> bool {
    body.lines()
        .any(|line| code_before_comment(line).trim() == expected)
}

fn check_workspace_derivation(
    body: &str,
    required: &RequiredDerivation,
    violations: &mut Vec<String>,
) {
    let legacy_builder = format!("craneLib.{}", required.builder);
    let checked = |attr, builder| {
        derivation_block(body, attr, builder).filter(|(_, block)| {
            has_code_line(block, &format!("operation = \"{}\";", required.operation))
        })
    };
    let definition = derivation_block(body, required.attr, &legacy_builder)
        .or_else(|| checked(required.attr, "checked.cargo"))
        .or_else(|| {
            (required.operation == "test" && has_code_line(body, "nextest = nextestFor null;"))
                .then(|| checked("nextestFor", "package: checked.cargo"))
                .flatten()
        });
    let Some((line, block)) = definition else {
        match first_assignment_line(body, required.attr) {
            Some(line) => violations.push(format!(
                "{WORKSPACE_REL}:{line} {} must be built with craneLib.{} or checked.cargo operation {}",
                required.attr, required.builder, required.operation,
            )),
            None => violations.push(format!(
                "{WORKSPACE_REL}: {} derivation is missing",
                required.attr,
            )),
        }
        return;
    };

    if required.operation == "test"
        && (first_assignment_line(body, "testCargoArtifacts").is_some()
            || has_code_line(&block, "CARGO_PROFILE = \"dev\";"))
    {
        if !(has_code_line(&block, "CARGO_PROFILE = \"dev\";")
            && has_code_line(&block, "cargoArtifacts = testCargoArtifacts;"))
        {
            violations.push(format!(
                "{WORKSPACE_REL}:{line} nextest must use the dev profile and shared testCargoArtifacts cache",
            ));
        }
    } else if required.operation == "build"
        && has_code_line(body, "testCompilation = nextest.compilation;")
    {
        if !(has_code_line(&block, "CARGO_PROFILE = \"dev\";")
            && has_code_line(&block, "cargoArtifacts = testCompilation;")
            && has_code_line(&block, "src = compilerSource.path;"))
        {
            violations.push(format!(
                "{WORKSPACE_REL}:{line} verificationBuild must use the exact dev catalogue compiler source and cache",
            ));
        }
    } else if !has_code_line(&block, "inherit cargoArtifacts;") {
        violations.push(format!(
            "{WORKSPACE_REL}:{line} {} must inherit the shared cargoArtifacts cache",
            required.attr,
        ));
    }
}

fn derivation_block(body: &str, attr: &str, builder: &str) -> Option<(usize, String)> {
    let assignment_prefix = format!("{attr} =");
    let mut start_line = None;
    let mut indentation = 0;
    let mut block = String::new();
    let mut head = String::new();
    let mut builder_found = false;

    for (idx, raw) in body.lines().enumerate() {
        let code = code_before_comment(raw).trim_start();
        if start_line.is_none() {
            let Some(value) = code.strip_prefix(&assignment_prefix) else {
                continue;
            };
            start_line = Some(idx + 1);
            indentation = raw.len() - raw.trim_start().len();
            head.push_str(value);
        } else if !builder_found {
            head.push(' ');
            head.push_str(code);
        }
        if !builder_found {
            let value = head.split_whitespace().collect::<Vec<_>>().join(" ");
            let value = value.trim_start_matches(|c: char| c.is_whitespace() || c == '(');
            if !value.is_empty() && !builder.starts_with(value) {
                let tail = value.strip_prefix(builder)?;
                if !tail.starts_with(|c: char| c.is_whitespace() || matches!(c, '(' | '{')) {
                    return None;
                }
                builder_found = true;
            }
        }
        block.push_str(raw);
        block.push('\n');
        if code.trim() == ");"
            || (code.trim() == "};" && raw.len() - raw.trim_start().len() <= indentation)
        {
            break;
        }
    }

    if builder_found {
        start_line.map(|line| (line, block))
    } else {
        None
    }
}

fn first_assignment_line(body: &str, attr: &str) -> Option<usize> {
    let prefix = format!("{attr} =");
    body.lines().enumerate().find_map(|(idx, raw)| {
        let code = code_before_comment(raw).trim_start();
        code.starts_with(&prefix).then_some(idx + 1)
    })
}

fn is_tool_probe(token: &str, tail: &str) -> bool {
    if !matches!(token, "clippy" | "nextest") {
        return false;
    }
    let tail = tail.trim_start_matches(['\'', '"']);
    if !tail.starts_with(char::is_whitespace) {
        return false;
    }
    let argument = tail.trim_start().trim_start_matches(['\'', '"']);
    ["--version", "-V", "--help", "-h"].iter().any(|probe| {
        argument.strip_prefix(probe).is_some_and(|rest| {
            rest.is_empty()
                || rest.starts_with(|character: char| {
                    character.is_whitespace()
                        || matches!(character, '\'' | '"' | ')' | ';' | '|' | '&' | '<' | '>')
                })
        })
    })
}

fn code_tokens(code: &str) -> impl Iterator<Item = &str> {
    code_token_spans(code).map(|(_offset, token)| token)
}

fn code_token_spans(code: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;
    code.split_inclusive(|character: char| {
        !(character.is_ascii_alphanumeric() || character == '_' || character == '-')
    })
    .filter_map(move |part| {
        let start = offset;
        offset += part.len();
        let token = part.trim_end_matches(|character: char| {
            !(character.is_ascii_alphanumeric() || character == '_' || character == '-')
        });
        (!token.is_empty()).then_some((start, token))
    })
}

fn code_before_comment(raw: &str) -> &str {
    match raw.split_once('#') {
        Some((code, _comment)) => code,
        None => raw,
    }
}

/// Standalone and checked dispatch execute the identical rule body.
pub fn run(input: &WalkInput) -> Verdict {
    input.finish(rule(input))
}

super::source::registration!("workspace_compile_checks_are_full_test_app_only");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_native_suite_rejects_recursive_whole_tree_gate() {
        let apps = REQUIRED_APP_SNIPPETS
            .iter()
            .map(|snippet| snippet.needle)
            .collect::<Vec<_>>()
            .join("\n");
        let mut script = REQUIRED_SCRIPT_SNIPPETS
            .iter()
            .map(|snippet| snippet.needle)
            .collect::<Vec<_>>()
            .join("\n");
        let mut findings = Vec::new();
        check_full_test_app(&apps, &script, &mut findings);
        assert!(findings.is_empty(), "{findings:?}");
        script.push_str("\nloom gate system --tree\n");
        check_full_test_app(&apps, &script, &mut findings);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].contains("recursive whole-tree Gate"));
    }
}

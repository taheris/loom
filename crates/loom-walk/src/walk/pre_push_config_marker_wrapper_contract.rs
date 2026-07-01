use std::fs;
use std::path::Path;

use super::util::{rel, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "pre_push_config_marker_wrapper_contract — first pre-push hook is nix flake check, every pre-push hook uses bin/pre-push-checks, --hook-entry matches the wrapped command, and nix commands use skip-if-missing nix --";

#[derive(Debug, Default, PartialEq, Eq)]
struct Hook {
    id: String,
    entry: Option<String>,
    stages: Vec<String>,
    line: usize,
}

impl Hook {
    fn is_pre_push(&self) -> bool {
        self.stages.iter().any(|stage| stage == "pre-push")
    }
}

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    run_for_path(&root, &root.join(".pre-commit-config.yaml"))
}

fn run_for_path(root: &Path, path: &Path) -> Verdict {
    let rel_path = rel(root, path);
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(source) => {
            return Verdict {
                pass: false,
                evidence: format!(
                    "{rel_path}:0 failed to read pre-commit config: {source}\n{RULE}"
                ),
            };
        }
    };
    verdict_from(RULE, violations(&rel_path, &source))
}

fn violations(path: &str, source: &str) -> Vec<String> {
    let hooks = parse_hooks(source);
    let pre_push_hooks: Vec<&Hook> = hooks.iter().filter(|hook| hook.is_pre_push()).collect();
    let mut violations = Vec::new();
    if pre_push_hooks.is_empty() {
        violations.push(format!("{path}:0 no pre-push hooks declared"));
        return violations;
    }
    if let Some(message) = first_pre_push_hook_violation(pre_push_hooks[0]) {
        violations.push(format!("{path}:{} {message}", pre_push_hooks[0].line));
    }
    for hook in pre_push_hooks {
        let Some(entry) = hook.entry.as_deref() else {
            violations.push(format!(
                "{path}:{} pre-push hook `{}` has no entry",
                hook.line, hook.id
            ));
            continue;
        };
        if let Some(message) = wrapper_violation(hook, entry) {
            violations.push(format!("{path}:{} {message}", hook.line));
        }
        let command = wrapped_command(entry).unwrap_or_else(|| entry.trim().to_owned());
        if command_runs_nix(&command) && !command.starts_with("skip-if-missing nix -- ") {
            violations.push(format!(
                "{path}:{} pre-push hook `{}` runs nix without `skip-if-missing nix --`: {command}",
                hook.line, hook.id
            ));
        }
    }
    violations
}

fn first_pre_push_hook_violation(hook: &Hook) -> Option<String> {
    if hook.id != "nix-flake-check" {
        return Some(format!(
            "first pre-push hook is `{}`, expected `nix-flake-check` fast tier",
            hook.id
        ));
    }
    let entry = hook.entry.as_deref()?.trim();
    let words = shlex::split(entry)?;
    let command = wrapped_command_words(&words).unwrap_or(words.as_slice());
    if command_runs_nix_flake_check(command) {
        None
    } else {
        Some(format!(
            "first pre-push hook `nix-flake-check` does not run `nix flake check`: {}",
            command.join(" ")
        ))
    }
}

fn wrapper_violation(hook: &Hook, entry: &str) -> Option<String> {
    let entry = entry.trim();
    let Some(words) = shlex::split(entry) else {
        return Some(format!(
            "pre-push hook `{}` entry is not parseable as shell words: {entry}",
            hook.id
        ));
    };
    if words.first().map(String::as_str) != Some("bin/pre-push-checks") {
        return Some(format!(
            "pre-push hook `{}` does not start with repo-local `bin/pre-push-checks`: {entry}",
            hook.id
        ));
    }
    let Some(separator) = wrapper_separator_index(&words) else {
        return Some(format!(
            "pre-push hook `{}` does not separate wrapper args from the hook command with ` -- `: {entry}",
            hook.id
        ));
    };
    let wrapper_args = &words[..separator];
    if !has_arg_value(wrapper_args, "--hook-id", &hook.id) {
        return Some(format!(
            "pre-push hook `{}` does not pass its own id to `--hook-id`: {entry}",
            hook.id
        ));
    }
    let Some(hook_entry) = arg_value(wrapper_args, "--hook-entry") else {
        return Some(format!(
            "pre-push hook `{}` does not pass `--hook-entry`: {entry}",
            hook.id
        ));
    };
    let Some(command_words) = command_after_separator(&words, separator) else {
        return Some(format!(
            "pre-push hook `{}` does not separate wrapper args from the hook command with ` -- `: {entry}",
            hook.id
        ));
    };
    let Some(hook_entry_words) = shlex::split(hook_entry) else {
        return Some(format!(
            "pre-push hook `{}` `--hook-entry` is not parseable as shell words: {hook_entry}",
            hook.id
        ));
    };
    if hook_entry_words.as_slice() != command_words {
        return Some(format!(
            "pre-push hook `{}` `--hook-entry` does not match wrapped command after `--`: `{hook_entry}` != `{}`",
            hook.id,
            command_words.join(" ")
        ));
    }
    None
}

fn arg_value<'a>(words: &'a [String], arg: &str) -> Option<&'a str> {
    words
        .windows(2)
        .find_map(|pair| (pair[0] == arg).then_some(pair[1].as_str()))
}

fn has_arg_value(words: &[String], arg: &str, expected: &str) -> bool {
    arg_value(words, arg) == Some(expected)
}

fn wrapped_command(entry: &str) -> Option<String> {
    let words = shlex::split(entry)?;
    let command = wrapped_command_words(&words)?;
    Some(command.join(" "))
}

fn wrapped_command_words(words: &[String]) -> Option<&[String]> {
    if words.first().map(String::as_str) != Some("bin/pre-push-checks") {
        return None;
    }
    let separator = wrapper_separator_index(words)?;
    command_after_separator(words, separator)
}

fn wrapper_separator_index(words: &[String]) -> Option<usize> {
    words.iter().position(|word| word == "--")
}

fn command_after_separator(words: &[String], separator: usize) -> Option<&[String]> {
    let command = words.get((separator + 1)..)?;
    (!command.is_empty()).then_some(command)
}

fn command_runs_nix(command: &str) -> bool {
    let command = command.trim();
    command.starts_with("nix ") || command.starts_with("skip-if-missing nix -- nix ")
}

fn command_runs_nix_flake_check(words: &[String]) -> bool {
    command_starts_with(words, &["nix", "flake", "check"])
        || command_starts_with(
            words,
            &["skip-if-missing", "nix", "--", "nix", "flake", "check"],
        )
}

fn command_starts_with(words: &[String], prefix: &[&str]) -> bool {
    words.len() >= prefix.len()
        && words
            .iter()
            .zip(prefix.iter())
            .all(|(word, expected)| word.as_str() == *expected)
}

fn parse_hooks(source: &str) -> Vec<Hook> {
    let mut hooks = Vec::new();
    let mut current: Option<Hook> = None;
    let mut in_stage_list = false;
    for (index, line) in source.lines().enumerate() {
        let line_no = index + 1;
        let trimmed = line.trim_start();
        if let Some(id) = trimmed.strip_prefix("- id:") {
            if let Some(hook) = current.take() {
                hooks.push(hook);
            }
            current = Some(Hook {
                id: unquote(id.trim()).to_owned(),
                line: line_no,
                ..Hook::default()
            });
            in_stage_list = false;
            continue;
        }
        let Some(hook) = current.as_mut() else {
            continue;
        };
        if let Some(entry) = trimmed.strip_prefix("entry:") {
            hook.entry = Some(entry.trim().to_owned());
            in_stage_list = false;
        } else if let Some(stages) = trimmed.strip_prefix("stages:") {
            hook.stages = parse_stages(stages);
            in_stage_list = hook.stages.is_empty();
        } else if in_stage_list {
            if let Some(stage) = trimmed.strip_prefix("- ") {
                hook.stages.push(unquote(stage.trim()).to_owned());
            } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
                in_stage_list = false;
            }
        }
    }
    if let Some(hook) = current {
        hooks.push(hook);
    }
    hooks
}

fn parse_stages(raw: &str) -> Vec<String> {
    raw.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(str::trim)
        .map(unquote)
        .filter(|stage| !stage.is_empty())
        .map(str::to_owned)
        .collect()
}

fn unquote(value: &str) -> &str {
    value.trim_matches(|c| c == '\'' || c == '"')
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_CONFIG: &str = r#"
repos:
  - repo: local
    hooks:
      - id: nix-flake-check
        entry: bin/pre-push-checks --hook-id nix-flake-check --hook-entry 'skip-if-missing nix -- nix flake check' -- skip-if-missing nix -- nix flake check
        stages: [pre-push]
      - id: cargo-clippy
        entry: bin/pre-push-checks --hook-id cargo-clippy --hook-entry 'cargo clippy --workspace --all-targets -- -D warnings' -- cargo clippy --workspace --all-targets -- -D warnings
        stages: [pre-push]
"#;

    #[test]
    fn accepts_valid_pre_push_wrapper_contract() {
        let got = violations(".pre-commit-config.yaml", VALID_CONFIG);
        assert!(got.is_empty(), "got: {got:?}");
    }

    #[test]
    fn parses_multiline_pre_push_stages() {
        let config = r#"
repos:
  - repo: local
    hooks:
      - id: nix-flake-check
        entry: bin/pre-push-checks --hook-id nix-flake-check --hook-entry 'skip-if-missing nix -- nix flake check' -- skip-if-missing nix -- nix flake check
        stages: [pre-push]
      - id: cargo-clippy
        entry: cargo clippy --workspace --all-targets -- -D warnings
        stages:
          - pre-push
"#;
        let got = violations(".pre-commit-config.yaml", config);
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("cargo-clippy"), "got: {got:?}");
    }

    #[test]
    fn rejects_pre_push_hook_that_skips_repo_local_wrapper() {
        let config = r#"
repos:
  - repo: local
    hooks:
      - id: nix-flake-check
        entry: bin/pre-push-checks --hook-id nix-flake-check --hook-entry 'skip-if-missing nix -- nix flake check' -- skip-if-missing nix -- nix flake check
        stages: [pre-push]
      - id: cargo-clippy
        entry: cargo clippy --workspace --all-targets -- -D warnings
        stages: [pre-push]
"#;
        let got = violations(".pre-commit-config.yaml", config);
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("cargo-clippy"), "got: {got:?}");
        assert!(got[0].contains("bin/pre-push-checks"), "got: {got:?}");
    }

    #[test]
    fn rejects_hook_entry_metadata_that_does_not_match_wrapped_command() {
        let config = r#"
repos:
  - repo: local
    hooks:
      - id: nix-flake-check
        entry: bin/pre-push-checks --hook-id nix-flake-check --hook-entry 'skip-if-missing nix -- nix flake check' -- skip-if-missing nix -- nix flake check
        stages: [pre-push]
      - id: cargo-clippy
        entry: bin/pre-push-checks --hook-id cargo-clippy --hook-entry 'nix flake check' -- cargo clippy --workspace --all-targets -- -D warnings
        stages: [pre-push]
"#;
        let got = violations(".pre-commit-config.yaml", config);
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("cargo-clippy"), "got: {got:?}");
        assert!(
            got[0].contains("does not match wrapped command"),
            "got: {got:?}"
        );
    }

    #[test]
    fn rejects_first_pre_push_hook_that_is_not_nix_flake_check() {
        let config = r#"
repos:
  - repo: local
    hooks:
      - id: cargo-clippy
        entry: bin/pre-push-checks --hook-id cargo-clippy --hook-entry 'cargo clippy --workspace --all-targets -- -D warnings' -- cargo clippy --workspace --all-targets -- -D warnings
        stages: [pre-push]
      - id: nix-flake-check
        entry: bin/pre-push-checks --hook-id nix-flake-check --hook-entry 'skip-if-missing nix -- nix flake check' -- skip-if-missing nix -- nix flake check
        stages: [pre-push]
"#;
        let got = violations(".pre-commit-config.yaml", config);
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("first pre-push hook"), "got: {got:?}");
        assert!(got[0].contains("nix-flake-check"), "got: {got:?}");
    }

    #[test]
    fn rejects_nix_command_without_skip_if_missing_wrapper() {
        let config = r#"
repos:
  - repo: local
    hooks:
      - id: nix-flake-check
        entry: bin/pre-push-checks --hook-id nix-flake-check --hook-entry 'skip-if-missing nix -- nix flake check' -- skip-if-missing nix -- nix flake check
        stages: [pre-push]
      - id: container-smoke
        entry: bin/pre-push-checks --hook-id container-smoke --hook-entry 'nix run .#test' -- nix run .#test
        stages: [pre-push]
"#;
        let got = violations(".pre-commit-config.yaml", config);
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("container-smoke"), "got: {got:?}");
        assert!(got[0].contains("skip-if-missing nix --"), "got: {got:?}");
    }
}

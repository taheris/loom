use std::fs;
use std::path::Path;

use super::util::{rel, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "pre_push_config_marker_wrapper_contract — pre-commit policy bindings must be complete, the first pre-push hook must be nix flake check, every pre-push hook must use bin/pre-push-checks with matching metadata, and nix commands must use skip-if-missing nix --";

#[derive(Debug, Default, PartialEq, Eq)]
struct Hook {
    repo: String,
    id: String,
    entry: Option<String>,
    language: Option<String>,
    stages: Vec<String>,
    types: Vec<String>,
    exclude: Option<String>,
    files: Option<String>,
    always_run: Option<bool>,
    pass_filenames: Option<bool>,
    line: usize,
}

impl Hook {
    fn is_pre_push(&self) -> bool {
        self.stages.iter().any(|stage| stage == "pre-push")
    }
}

#[derive(Debug, Clone, Copy)]
enum ListField {
    Stages,
    Types,
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
    let mut violations = Vec::new();
    validate_unique_ids(path, &hooks, &mut violations);
    validate_pre_commit(path, &hooks, &mut violations);
    validate_pre_push(path, &hooks, &mut violations);
    validate_no_standalone_marker(path, &hooks, &mut violations);
    violations
}

fn validate_unique_ids(path: &str, hooks: &[Hook], violations: &mut Vec<String>) {
    for (index, hook) in hooks.iter().enumerate() {
        if hooks[(index + 1)..].iter().any(|other| other.id == hook.id) {
            violations.push(format!(
                "{path}:{} hook id `{}` is declared more than once",
                hook.line, hook.id
            ));
        }
    }
}

fn validate_pre_commit(path: &str, hooks: &[Hook], violations: &mut Vec<String>) {
    for id in [
        "trailing-whitespace",
        "end-of-file-fixer",
        "check-merge-conflict",
    ] {
        let Some(hook) = required_hook(path, hooks, id, violations) else {
            continue;
        };
        validate_binding(path, hook, "builtin", "pre-commit", None, violations);
    }

    if let Some(hook) = unique_hook(hooks, "end-of-file-fixer")
        && hook.exclude.as_deref() != Some("^.beads/config.yaml$")
    {
        violations.push(format!(
            "{path}:{} end-of-file-fixer must exclude `.beads/config.yaml`",
            hook.line
        ));
    }

    validate_local_pre_commit_hook(
        path,
        hooks,
        "treefmt",
        "treefmt --fail-on-change",
        Some(false),
        violations,
    );
    validate_local_pre_commit_hook(
        path,
        hooks,
        "shell-reexec-explicit-interpreter",
        "scripts/check-shell-reexec",
        None,
        violations,
    );
    if let Some(hook) = unique_hook(hooks, "shell-reexec-explicit-interpreter")
        && hook.types.as_slice() != ["shell"]
    {
        violations.push(format!(
            "{path}:{} shell-reexec-explicit-interpreter must be bound to shell files",
            hook.line
        ));
    }
    validate_local_pre_commit_hook(
        path,
        hooks,
        "loom-gate-verify-files",
        "loom gate verify --files",
        Some(true),
        violations,
    );
}

fn validate_local_pre_commit_hook(
    path: &str,
    hooks: &[Hook],
    id: &str,
    expected_entry: &str,
    pass_filenames: Option<bool>,
    violations: &mut Vec<String>,
) {
    let Some(hook) = required_hook(path, hooks, id, violations) else {
        return;
    };
    validate_binding(
        path,
        hook,
        "local",
        "pre-commit",
        Some("system"),
        violations,
    );
    if !entry_equals(hook, expected_entry) {
        violations.push(format!(
            "{path}:{} hook `{id}` must execute `{expected_entry}`",
            hook.line
        ));
    }
    if let Some(expected) = pass_filenames
        && hook.pass_filenames != Some(expected)
    {
        violations.push(format!(
            "{path}:{} hook `{id}` must set pass_filenames to {expected}",
            hook.line
        ));
    }
}

fn validate_pre_push(path: &str, hooks: &[Hook], violations: &mut Vec<String>) {
    let pre_push_hooks: Vec<&Hook> = hooks.iter().filter(|hook| hook.is_pre_push()).collect();
    if pre_push_hooks.is_empty() {
        violations.push(format!("{path}:0 no pre-push hooks declared"));
        return;
    }
    if let Some(message) = first_pre_push_hook_violation(pre_push_hooks[0]) {
        violations.push(format!("{path}:{} {message}", pre_push_hooks[0].line));
    }

    for id in [
        "nix-flake-check",
        "cargo-clippy",
        "loom-gate-verify-diff",
        "full-test-suite",
    ] {
        let Some(hook) = required_hook(path, hooks, id, violations) else {
            continue;
        };
        validate_binding(path, hook, "local", "pre-push", Some("system"), violations);
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

    validate_pre_push_policy_fields(path, hooks, violations);
}

fn validate_pre_push_policy_fields(path: &str, hooks: &[Hook], violations: &mut Vec<String>) {
    if let Some(hook) = unique_hook(hooks, "nix-flake-check") {
        validate_always_run(path, hook, violations);
        validate_wrapped_command(
            path,
            hook,
            "skip-if-missing nix -- nix flake check",
            violations,
        );
    }
    if let Some(hook) = unique_hook(hooks, "cargo-clippy") {
        validate_wrapped_command(
            path,
            hook,
            "cargo clippy --workspace --all-targets -- -D warnings",
            violations,
        );
        if hook.files.as_deref() != Some(r"\.rs$") {
            violations.push(format!(
                "{path}:{} cargo-clippy must be selected only for Rust files",
                hook.line
            ));
        }
    }
    if let Some(hook) = unique_hook(hooks, "loom-gate-verify-diff") {
        validate_always_run(path, hook, violations);
        validate_wrapped_command(path, hook, "loom gate verify --diff", violations);
        let entry = hook.entry.as_deref().unwrap_or_default();
        if !shlex::split(entry)
            .is_some_and(|words| words.iter().any(|word| word == "--append-push-range"))
        {
            violations.push(format!(
                "{path}:{} loom-gate-verify-diff must append prek's pushed range",
                hook.line
            ));
        }
        if entry.contains("LOOM_VERIFY_TIERS") {
            violations.push(format!(
                "{path}:{} loom-gate-verify-diff must use scope-derived tiers",
                hook.line
            ));
        }
    }
    if let Some(hook) = unique_hook(hooks, "full-test-suite") {
        validate_always_run(path, hook, violations);
        validate_wrapped_command(
            path,
            hook,
            "skip-if-missing nix -- nix run .#test",
            violations,
        );
    }
}

fn validate_no_standalone_marker(path: &str, hooks: &[Hook], violations: &mut Vec<String>) {
    for hook in hooks {
        if hook.id.contains("verify-marker")
            || hook
                .entry
                .as_deref()
                .is_some_and(|entry| entry.contains("gate verify-marker"))
        {
            violations.push(format!(
                "{path}:{} marker validation must remain inside pre-push-checks, not a standalone hook",
                hook.line
            ));
        }
    }
}

fn validate_binding(
    path: &str,
    hook: &Hook,
    expected_repo: &str,
    expected_stage: &str,
    expected_language: Option<&str>,
    violations: &mut Vec<String>,
) {
    if hook.repo != expected_repo {
        violations.push(format!(
            "{path}:{} hook `{}` belongs under repo `{expected_repo}`, found `{}`",
            hook.line, hook.id, hook.repo
        ));
    }
    if hook.stages.as_slice() != [expected_stage] {
        violations.push(format!(
            "{path}:{} hook `{}` must be bound only to stage `{expected_stage}`",
            hook.line, hook.id
        ));
    }
    if let Some(expected) = expected_language
        && hook.language.as_deref() != Some(expected)
    {
        violations.push(format!(
            "{path}:{} hook `{}` must use language `{expected}`",
            hook.line, hook.id
        ));
    }
}

fn validate_always_run(path: &str, hook: &Hook, violations: &mut Vec<String>) {
    if hook.always_run != Some(true) {
        violations.push(format!(
            "{path}:{} pre-push hook `{}` must set always_run to true",
            hook.line, hook.id
        ));
    }
}

fn validate_wrapped_command(path: &str, hook: &Hook, expected: &str, violations: &mut Vec<String>) {
    let actual = hook.entry.as_deref().and_then(wrapped_command);
    if actual.as_deref() != Some(expected) {
        violations.push(format!(
            "{path}:{} pre-push hook `{}` must wrap `{expected}`",
            hook.line, hook.id
        ));
    }
}

fn required_hook<'a>(
    path: &str,
    hooks: &'a [Hook],
    id: &str,
    violations: &mut Vec<String>,
) -> Option<&'a Hook> {
    let matches: Vec<&Hook> = hooks.iter().filter(|hook| hook.id == id).collect();
    match matches.as_slice() {
        [hook] => Some(*hook),
        [] => {
            violations.push(format!("{path}:0 required hook `{id}` is missing"));
            None
        }
        _ => None,
    }
}

fn unique_hook<'a>(hooks: &'a [Hook], id: &str) -> Option<&'a Hook> {
    let mut matches = hooks.iter().filter(|hook| hook.id == id);
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn entry_equals(hook: &Hook, expected: &str) -> bool {
    hook.entry
        .as_deref()
        .and_then(shlex::split)
        .zip(shlex::split(expected))
        .is_some_and(|(actual, expected)| actual == expected)
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
    let mut repo = String::new();
    let mut list_field = None;

    for (index, line) in source.lines().enumerate() {
        let line_no = index + 1;
        let trimmed = line.trim_start();
        if let Some(value) = trimmed.strip_prefix("- repo:") {
            if let Some(hook) = current.take() {
                hooks.push(hook);
            }
            repo = unquote(value.trim()).to_owned();
            list_field = None;
            continue;
        }
        if let Some(id) = trimmed.strip_prefix("- id:") {
            if let Some(hook) = current.take() {
                hooks.push(hook);
            }
            current = Some(Hook {
                repo: repo.clone(),
                id: unquote(id.trim()).to_owned(),
                line: line_no,
                ..Hook::default()
            });
            list_field = None;
            continue;
        }
        let Some(hook) = current.as_mut() else {
            continue;
        };
        if let Some(field) = list_field
            && let Some(value) = trimmed.strip_prefix("- ")
        {
            list_for(hook, field).push(unquote(value.trim()).to_owned());
            continue;
        }
        list_field = None;
        if let Some(value) = trimmed.strip_prefix("entry:") {
            hook.entry = Some(value.trim().to_owned());
        } else if let Some(value) = trimmed.strip_prefix("language:") {
            hook.language = Some(unquote(value.trim()).to_owned());
        } else if let Some(value) = trimmed.strip_prefix("stages:") {
            hook.stages = parse_list(value);
            if hook.stages.is_empty() {
                list_field = Some(ListField::Stages);
            }
        } else if let Some(value) = trimmed.strip_prefix("types:") {
            hook.types = parse_list(value);
            if hook.types.is_empty() {
                list_field = Some(ListField::Types);
            }
        } else if let Some(value) = trimmed.strip_prefix("exclude:") {
            hook.exclude = Some(unquote(value.trim()).to_owned());
        } else if let Some(value) = trimmed.strip_prefix("files:") {
            hook.files = Some(unquote(value.trim()).to_owned());
        } else if let Some(value) = trimmed.strip_prefix("always_run:") {
            hook.always_run = parse_bool(value);
        } else if let Some(value) = trimmed.strip_prefix("pass_filenames:") {
            hook.pass_filenames = parse_bool(value);
        }
    }
    if let Some(hook) = current {
        hooks.push(hook);
    }
    hooks
}

fn list_for(hook: &mut Hook, field: ListField) -> &mut Vec<String> {
    match field {
        ListField::Stages => &mut hook.stages,
        ListField::Types => &mut hook.types,
    }
}

fn parse_list(raw: &str) -> Vec<String> {
    let raw = raw.trim();
    if !raw.starts_with('[') || !raw.ends_with(']') {
        return Vec::new();
    }
    raw.trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(str::trim)
        .map(unquote)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn parse_bool(raw: &str) -> Option<bool> {
    match unquote(raw.trim()) {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn unquote(value: &str) -> &str {
    value.trim_matches(|c| c == '\'' || c == '"')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config() -> String {
        include_str!("../../../../.pre-commit-config.yaml").to_owned()
    }

    #[test]
    fn accepts_complete_hook_policy() {
        let got = violations(".pre-commit-config.yaml", &valid_config());
        assert!(got.is_empty(), "got: {got:?}");
    }

    #[test]
    fn rejects_missing_builtin_hook() {
        let config = valid_config().replace(
            "      - id: check-merge-conflict\n        stages: [pre-commit]\n",
            "",
        );
        let got = violations(".pre-commit-config.yaml", &config);
        assert!(
            got.iter()
                .any(|line| line.contains("required hook `check-merge-conflict` is missing")),
            "got: {got:?}"
        );
    }

    #[test]
    fn rejects_pre_commit_hook_with_wrong_entry() {
        let config = valid_config().replace(
            "entry: scripts/check-shell-reexec",
            "entry: scripts/unrelated-check",
        );
        let got = violations(".pre-commit-config.yaml", &config);
        assert!(
            got.iter().any(|line| line.contains(
                "shell-reexec-explicit-interpreter` must execute `scripts/check-shell-reexec"
            )),
            "got: {got:?}"
        );
    }

    #[test]
    fn rejects_end_of_file_fixer_without_beads_exclusion() {
        let config =
            valid_config().replace("exclude: '^.beads/config.yaml$'", "exclude: '^unrelated$'");
        let got = violations(".pre-commit-config.yaml", &config);
        assert!(
            got.iter()
                .any(|line| line.contains("must exclude `.beads/config.yaml`")),
            "got: {got:?}"
        );
    }

    #[test]
    fn rejects_pre_push_hook_that_skips_repo_local_wrapper() {
        let config = valid_config().replace(
            "entry: bin/pre-push-checks --hook-id cargo-clippy --hook-entry 'cargo clippy --workspace --all-targets -- -D warnings' -- cargo clippy --workspace --all-targets -- -D warnings",
            "entry: cargo clippy --workspace --all-targets -- -D warnings",
        );
        let got = violations(".pre-commit-config.yaml", &config);
        assert!(
            got.iter().any(|line| line
                .contains("cargo-clippy` does not start with repo-local `bin/pre-push-checks")),
            "got: {got:?}"
        );
    }

    #[test]
    fn rejects_hook_entry_metadata_that_does_not_match_wrapped_command() {
        let config = valid_config().replace(
            "--hook-entry 'cargo clippy --workspace --all-targets -- -D warnings' -- cargo clippy",
            "--hook-entry 'nix flake check' -- cargo clippy",
        );
        let got = violations(".pre-commit-config.yaml", &config);
        assert!(
            got.iter()
                .any(|line| line.contains("does not match wrapped command")),
            "got: {got:?}"
        );
    }

    #[test]
    fn rejects_nix_command_without_skip_if_missing_wrapper() {
        let config = valid_config().replace(
            "--hook-entry 'skip-if-missing nix -- nix run .#test' -- skip-if-missing nix -- nix run .#test",
            "--hook-entry 'nix run .#test' -- nix run .#test",
        );
        let got = violations(".pre-commit-config.yaml", &config);
        assert!(
            got.iter()
                .any(|line| line.contains("runs nix without `skip-if-missing nix --`")),
            "got: {got:?}"
        );
    }

    #[test]
    fn rejects_verify_diff_without_pushed_range_append() {
        let config = valid_config().replace(" --append-push-range -- loom gate", " -- loom gate");
        let got = violations(".pre-commit-config.yaml", &config);
        assert!(
            got.iter()
                .any(|line| line.contains("must append prek's pushed range")),
            "got: {got:?}"
        );
    }

    #[test]
    fn rejects_standalone_verify_marker_hook() {
        let config = format!(
            "{}\n      - id: verify-marker\n        entry: loom gate verify-marker\n        language: system\n        stages: [pre-push]\n",
            valid_config()
        );
        let got = violations(".pre-commit-config.yaml", &config);
        assert!(
            got.iter()
                .any(|line| line.contains("not a standalone hook")),
            "got: {got:?}"
        );
    }
}

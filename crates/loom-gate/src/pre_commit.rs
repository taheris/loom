use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use displaydoc::Display;
use thiserror::Error;

use crate::gate_outcome::HookCoverage;

const CONFIG_PATH: &str = ".pre-commit-config.yaml";
const WRAPPER: &str = "bin/pre-push-checks";

#[derive(Debug, Display, Error)]
pub enum ConfigError {
    /// failed to read `{path}`
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// no pre-push hooks declared in `{path}`
    NoPrePushHooks { path: PathBuf },
    /// pre-push hook `{id}` in `{path}` has no entry
    MissingEntry { path: PathBuf, id: String },
    /// pre-push hook `{id}` entry in `{path}` is not parseable as shell words
    InvalidEntry { path: PathBuf, id: String },
    /// pre-push hook `{id}` in `{path}` does not use bin/pre-push-checks
    MissingWrapper { path: PathBuf, id: String },
    /// pre-push hook `{id}` in `{path}` does not separate wrapper args from command
    MissingWrapperSeparator { path: PathBuf, id: String },
    /// pre-push hook `{id}` in `{path}` does not pass --hook-id
    MissingHookId { path: PathBuf, id: String },
    /// pre-push hook `{id}` in `{path}` passes --hook-id `{found}`
    HookIdMismatch {
        path: PathBuf,
        id: String,
        found: String,
    },
    /// pre-push hook `{id}` --hook-entry in `{path}` is not parseable as shell words
    InvalidHookEntry { path: PathBuf, id: String },
    /// pre-push hook `{id}` --hook-entry in `{path}` does not match the wrapped command
    HookEntryMismatch { path: PathBuf, id: String },
    /// pre-push hook `{id}` in `{path}` does not pass --hook-entry
    MissingHookEntry { path: PathBuf, id: String },
}

pub fn pre_push_hook_coverage_from_config(
    workspace: &Path,
) -> Result<Vec<HookCoverage>, ConfigError> {
    pre_push_hook_coverage_from_path(&workspace.join(CONFIG_PATH))
}

pub fn pre_push_hook_coverage_from_path(path: &Path) -> Result<Vec<HookCoverage>, ConfigError> {
    let source = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    pre_push_hook_coverage_from_source(path, &source)
}

fn pre_push_hook_coverage_from_source(
    path: &Path,
    source: &str,
) -> Result<Vec<HookCoverage>, ConfigError> {
    let pre_push_hooks = parse_hooks(source)
        .into_iter()
        .filter(Hook::is_pre_push)
        .collect::<Vec<_>>();
    if pre_push_hooks.is_empty() {
        return Err(ConfigError::NoPrePushHooks {
            path: path.to_path_buf(),
        });
    }
    pre_push_hooks
        .iter()
        .map(|hook| coverage_for_hook(path, hook))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Hook {
    id: String,
    entry: Option<String>,
    stages: Vec<String>,
}

impl Hook {
    fn new(id: String) -> Self {
        Self {
            id,
            entry: None,
            stages: Vec::new(),
        }
    }

    fn is_pre_push(&self) -> bool {
        self.stages.iter().any(|stage| stage == "pre-push")
    }
}

#[derive(Debug)]
struct HookContext {
    hook: Hook,
    hook_indent: usize,
    stage_list_indent: Option<usize>,
}

fn parse_hooks(source: &str) -> Vec<Hook> {
    let mut hooks = Vec::new();
    let mut current: Option<HookContext> = None;
    for line in source.lines() {
        let indent = leading_spaces(line);
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !trimmed.starts_with("- id:")
            && current
                .as_ref()
                .is_some_and(|context| indent <= context.hook_indent)
            && let Some(context) = current.take()
        {
            hooks.push(context.hook);
        }
        if let Some(id) = trimmed.strip_prefix("- id:") {
            if let Some(context) = current.take() {
                hooks.push(context.hook);
            }
            current = Some(HookContext {
                hook: Hook::new(unquote(id.trim()).to_owned()),
                hook_indent: indent,
                stage_list_indent: None,
            });
            continue;
        }
        let Some(context) = current.as_mut() else {
            continue;
        };
        if let Some(stage_indent) = context.stage_list_indent {
            if indent > stage_indent
                && let Some(stage) = trimmed.strip_prefix("- ")
            {
                context.hook.stages.push(unquote(stage.trim()).to_owned());
                continue;
            }
            context.stage_list_indent = None;
        }
        if let Some(entry) = trimmed.strip_prefix("entry:") {
            context.hook.entry = Some(unquote(entry.trim()).to_owned());
            context.stage_list_indent = None;
        } else if let Some(stages) = trimmed.strip_prefix("stages:") {
            context.hook.stages = parse_stages(stages);
            context.stage_list_indent = context.hook.stages.is_empty().then_some(indent);
        }
    }
    if let Some(context) = current {
        hooks.push(context.hook);
    }
    hooks
}

fn coverage_for_hook(path: &Path, hook: &Hook) -> Result<HookCoverage, ConfigError> {
    let entry = hook
        .entry
        .as_deref()
        .ok_or_else(|| ConfigError::MissingEntry {
            path: path.to_path_buf(),
            id: hook.id.clone(),
        })?;
    let words = shlex::split(entry).ok_or_else(|| ConfigError::InvalidEntry {
        path: path.to_path_buf(),
        id: hook.id.clone(),
    })?;
    if words.first().map(String::as_str) != Some(WRAPPER) {
        return Err(ConfigError::MissingWrapper {
            path: path.to_path_buf(),
            id: hook.id.clone(),
        });
    }
    let separator = words.iter().position(|word| word == "--").ok_or_else(|| {
        ConfigError::MissingWrapperSeparator {
            path: path.to_path_buf(),
            id: hook.id.clone(),
        }
    })?;
    let wrapper_args = &words[..separator];
    let hook_id =
        arg_value(wrapper_args, "--hook-id").ok_or_else(|| ConfigError::MissingHookId {
            path: path.to_path_buf(),
            id: hook.id.clone(),
        })?;
    if hook_id != hook.id {
        return Err(ConfigError::HookIdMismatch {
            path: path.to_path_buf(),
            id: hook.id.clone(),
            found: hook_id.to_owned(),
        });
    }
    let hook_entry =
        arg_value(wrapper_args, "--hook-entry").ok_or_else(|| ConfigError::MissingHookEntry {
            path: path.to_path_buf(),
            id: hook.id.clone(),
        })?;
    let hook_entry_words =
        shlex::split(hook_entry).ok_or_else(|| ConfigError::InvalidHookEntry {
            path: path.to_path_buf(),
            id: hook.id.clone(),
        })?;
    let command_words = words
        .get((separator + 1)..)
        .filter(|words| !words.is_empty())
        .ok_or_else(|| ConfigError::MissingWrapperSeparator {
            path: path.to_path_buf(),
            id: hook.id.clone(),
        })?;
    if hook_entry_words.as_slice() != command_words {
        return Err(ConfigError::HookEntryMismatch {
            path: path.to_path_buf(),
            id: hook.id.clone(),
        });
    }
    Ok(HookCoverage {
        id: hook_id.to_owned(),
        entry: hook_entry.to_owned(),
    })
}

fn arg_value<'a>(words: &'a [String], name: &str) -> Option<&'a str> {
    let prefix = format!("{name}=");
    for (index, word) in words.iter().enumerate() {
        if word == name {
            return words.get(index + 1).map(String::as_str);
        }
        if let Some(value) = word.strip_prefix(&prefix) {
            return Some(value);
        }
    }
    None
}

fn parse_stages(raw: &str) -> Vec<String> {
    let value = raw.trim();
    let value = value
        .strip_prefix('[')
        .and_then(|stripped| stripped.strip_suffix(']'))
        .unwrap_or(value);
    value
        .split(',')
        .map(str::trim)
        .map(unquote)
        .filter(|stage| !stage.is_empty())
        .map(str::to_owned)
        .collect()
}

fn unquote(value: &str) -> &str {
    let value = value.trim();
    if let Some(stripped) = value
        .strip_prefix('\'')
        .and_then(|stripped| stripped.strip_suffix('\''))
    {
        return stripped;
    }
    if let Some(stripped) = value
        .strip_prefix('"')
        .and_then(|stripped| stripped.strip_suffix('"'))
    {
        return stripped;
    }
    value
}

fn leading_spaces(line: &str) -> usize {
    line.chars()
        .take_while(|character| *character == ' ')
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
repos:
  - repo: builtin
    hooks:
      - id: trailing-whitespace
        stages: [pre-commit]
  - repo: local
    hooks:
      - id: nix-flake-check
        entry: bin/pre-push-checks --hook-id nix-flake-check --hook-entry 'skip-if-missing nix -- nix flake check' -- skip-if-missing nix -- nix flake check
        stages: [pre-push]
      - id: cargo-clippy
        entry: bin/pre-push-checks --hook-id cargo-clippy --hook-entry 'cargo clippy --workspace --all-targets -- -D warnings' -- cargo clippy --workspace --all-targets -- -D warnings
        stages:
          - pre-push
"#;

    #[test]
    fn derives_pre_push_hook_coverage_from_config_metadata() {
        let coverage = pre_push_hook_coverage_from_source(Path::new("config.yaml"), CONFIG)
            .expect("coverage from config");

        assert_eq!(
            coverage,
            vec![
                HookCoverage {
                    id: "nix-flake-check".to_owned(),
                    entry: "skip-if-missing nix -- nix flake check".to_owned(),
                },
                HookCoverage {
                    id: "cargo-clippy".to_owned(),
                    entry: "cargo clippy --workspace --all-targets -- -D warnings".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn derives_custom_pre_push_hooks_without_a_fixed_table() {
        let source = r#"
repos:
  - repo: local
    hooks:
      - id: custom-slow-check
        entry: bin/pre-push-checks --hook-id=custom-slow-check --hook-entry 'custom check --flag' -- custom check --flag
        stages: [pre-push]
"#;

        let coverage = pre_push_hook_coverage_from_source(Path::new("config.yaml"), source)
            .expect("coverage from config");

        assert_eq!(
            coverage,
            vec![HookCoverage {
                id: "custom-slow-check".to_owned(),
                entry: "custom check --flag".to_owned(),
            }]
        );
    }

    #[test]
    fn reads_workspace_pre_commit_config_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(CONFIG_PATH), CONFIG).expect("write config");

        let coverage = pre_push_hook_coverage_from_config(dir.path()).expect("coverage from file");

        assert_eq!(coverage.len(), 2);
        assert_eq!(coverage[0].id, "nix-flake-check");
    }

    #[test]
    fn rejects_pre_push_hook_without_wrapper_metadata() {
        let source = r#"
repos:
  - repo: local
    hooks:
      - id: cargo-clippy
        entry: cargo clippy --workspace --all-targets -- -D warnings
        stages: [pre-push]
"#;

        let error = pre_push_hook_coverage_from_source(Path::new("config.yaml"), source)
            .expect_err("missing wrapper must fail");

        assert!(matches!(
            error,
            ConfigError::MissingWrapper { id, .. } if id == "cargo-clippy"
        ));
    }

    #[test]
    fn rejects_wrapper_hook_id_mismatch() {
        let source = r#"
repos:
  - repo: local
    hooks:
      - id: cargo-clippy
        entry: bin/pre-push-checks --hook-id wrong-id --hook-entry 'cargo clippy' -- cargo clippy
        stages: [pre-push]
"#;

        let error = pre_push_hook_coverage_from_source(Path::new("config.yaml"), source)
            .expect_err("mismatch must fail");

        assert!(matches!(
            error,
            ConfigError::HookIdMismatch { id, found, .. }
                if id == "cargo-clippy" && found == "wrong-id"
        ));
    }

    #[test]
    fn rejects_hook_entry_that_differs_from_wrapped_command() {
        let source = r#"
repos:
  - repo: local
    hooks:
      - id: cargo-clippy
        entry: bin/pre-push-checks --hook-id cargo-clippy --hook-entry 'nix flake check' -- cargo clippy
        stages: [pre-push]
"#;

        let error = pre_push_hook_coverage_from_source(Path::new("config.yaml"), source)
            .expect_err("mismatch must fail");

        assert!(matches!(
            error,
            ConfigError::HookEntryMismatch { id, .. } if id == "cargo-clippy"
        ));
    }
}

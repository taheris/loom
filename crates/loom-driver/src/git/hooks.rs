//! Git hook-path resolution and local config writes for loom workspaces.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use super::error::GitError;

/// Env var exported by wrix devshell/profile images with the packaged prek hooks.
pub const WRIX_PREK_HOOKS_ENV: &str = "WRIX_PREK_HOOKS";

const REQUIRED_HOOKS: &[&str] = &["pre-commit", "pre-push"];

/// Resolve the canonical wrix `prekHooks` directory.
pub fn resolve_prek_hooks_path() -> Result<PathBuf, GitError> {
    resolve_from(&ResolveInputs {
        env_hooks: std::env::var_os(WRIX_PREK_HOOKS_ENV),
    })
}

/// Validate a resolved hooks directory contains the hook shims loom requires.
pub fn ensure_prek_hooks_dir(path: &Path) -> Result<(), GitError> {
    if !path.is_dir() {
        return Err(GitError::PrekHooksMissing {
            path: path.to_path_buf(),
        });
    }
    for hook in REQUIRED_HOOKS {
        if !path.join(hook).is_file() {
            return Err(GitError::PrekHooksMissing {
                path: path.to_path_buf(),
            });
        }
    }
    Ok(())
}

/// Write `core.hooksPath` in `target_dir` to `hooks_path`.
pub fn write_hooks_config(target_dir: &Path, hooks_path: &Path) -> Result<(), GitError> {
    ensure_prek_hooks_dir(hooks_path)?;
    sync_git_config(target_dir, "core.hooksPath", &hooks_path.to_string_lossy())
}

/// Verify `target_dir` has `core.hooksPath` set to `expected`.
pub fn validate_hooks_config(target_dir: &Path, expected: &Path) -> Result<(), GitError> {
    ensure_prek_hooks_dir(expected)?;
    let expected_value = expected.to_string_lossy().into_owned();
    let actual = sync_git_config_get(target_dir, "core.hooksPath")?;
    if actual.as_deref() == Some(expected_value.as_str()) {
        return Ok(());
    }
    Err(GitError::HooksPathInvalid {
        workdir: target_dir.to_path_buf(),
        expected: expected_value,
        actual: actual.unwrap_or_else(|| "<unset>".to_string()),
    })
}

struct ResolveInputs {
    env_hooks: Option<OsString>,
}

fn resolve_from(inputs: &ResolveInputs) -> Result<PathBuf, GitError> {
    let Some(raw) = &inputs.env_hooks else {
        return Err(GitError::PrekHooksUnresolved);
    };
    let path = PathBuf::from(raw);
    ensure_prek_hooks_dir(&path)?;
    Ok(path)
}

fn sync_git_config(target_dir: &Path, key: &str, value: &str) -> Result<(), GitError> {
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(target_dir)
        .args(["config", key, value])
        .output()
        .map_err(GitError::Spawn)?;
    if output.status.success() {
        return Ok(());
    }
    Err(GitError::GitCli {
        status: output.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn sync_git_config_get(target_dir: &Path, key: &str) -> Result<Option<String>, GitError> {
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(target_dir)
        .args(["config", "--get", key])
        .output()
        .map_err(GitError::Spawn)?;
    if output.status.success() {
        return Ok(Some(String::from_utf8(output.stdout)?.trim().to_string()));
    }
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    Err(GitError::GitCli {
        status: output.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_hooks(root: &Path) -> Result<PathBuf, std::io::Error> {
        let hooks = root.join("prek-hooks");
        std::fs::create_dir_all(&hooks)?;
        for hook in REQUIRED_HOOKS {
            std::fs::write(hooks.join(hook), "#!/bin/sh\n")?;
        }
        Ok(hooks)
    }

    #[test]
    fn resolver_returns_wrix_prek_hooks_env_path() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let hooks = fake_hooks(tmp.path())?;
        let resolved = resolve_from(&ResolveInputs {
            env_hooks: Some(hooks.clone().into_os_string()),
        })?;
        assert_eq!(resolved, hooks);
        Ok(())
    }

    #[test]
    fn resolver_fails_loud_when_wrix_prek_hooks_env_missing()
    -> Result<(), Box<dyn std::error::Error>> {
        let err = match resolve_from(&ResolveInputs { env_hooks: None }) {
            Ok(path) => return Err(format!("missing env resolved unexpectedly: {path:?}").into()),
            Err(err) => err,
        };
        assert!(matches!(err, GitError::PrekHooksUnresolved));
        Ok(())
    }

    #[test]
    fn resolver_fails_loud_when_wrix_prek_hooks_dir_invalid()
    -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let missing = tmp.path().join("missing-hooks");
        let err = match resolve_from(&ResolveInputs {
            env_hooks: Some(missing.clone().into_os_string()),
        }) {
            Ok(path) => return Err(format!("missing path resolved unexpectedly: {path:?}").into()),
            Err(err) => err,
        };
        assert!(matches!(err, GitError::PrekHooksMissing { path } if path == missing));
        Ok(())
    }
}

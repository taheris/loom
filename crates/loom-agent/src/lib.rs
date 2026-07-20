//! Agent backend implementations for Loom.
//!
//! Houses three zero-sized backends — [`PiBackend`] for pi-mono RPC,
//! [`ClaudeBackend`] for Claude Code stream-json, and [`DirectBackend`]
//! for the in-container `loom-direct-runner` — that implement the
//! [`AgentBackend`](loom_driver::agent::AgentBackend) trait declared in
//! `loom-driver`. The trait's job is process lifecycle only; conversation
//! driving (prompt, steer, abort, event streaming) lives on
//! [`AgentSession`](loom_driver::agent::AgentSession).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use loom_driver::agent::SpawnConfig;

pub mod claude;
pub mod direct;
pub mod pi;
mod skill;

pub use claude::ClaudeBackend;
pub use direct::DirectBackend;
pub use pi::PiBackend;

const ENV_WRIX_SPAWN_BIN: &str = "LOOM_WRIX_SPAWN_BIN";
const ENV_WRIX_BIN: &str = "LOOM_WRIX_BIN";

pub(crate) fn resolve_wrix_spawn_bin(config: &SpawnConfig) -> OsString {
    config
        .wrix_launcher
        .as_ref()
        .map(|path| deprofiled_wrix(path.as_os_str().to_os_string()))
        .unwrap_or_else(resolve_wrix_spawn_bin_from_env)
}

fn resolve_wrix_spawn_bin_from_env() -> OsString {
    let candidate = std::env::var_os(ENV_WRIX_SPAWN_BIN)
        .or_else(|| std::env::var_os(ENV_WRIX_BIN))
        .unwrap_or_else(|| OsString::from("wrix"));
    deprofiled_wrix(candidate)
}

fn deprofiled_wrix(candidate: OsString) -> OsString {
    let Some(path) = resolve_candidate_path(&candidate) else {
        return candidate;
    };
    let script = match std::fs::read_to_string(&path) {
        Ok(script) => script,
        // best-effort: raw wrix binaries are not UTF-8 shell scripts, and a
        // missing/unreadable override should preserve the operator-provided
        // command so the eventual spawn error names what they configured.
        Err(_) => return candidate,
    };
    let Some(launcher) = parse_profiled_wrix_launcher(&script) else {
        return candidate;
    };
    if launcher.is_file() {
        launcher.into_os_string()
    } else {
        candidate
    }
}

fn resolve_candidate_path(candidate: &OsStr) -> Option<PathBuf> {
    let candidate_path = Path::new(candidate);
    if candidate_path.is_absolute() || candidate_path.components().count() > 1 {
        return candidate_path
            .is_file()
            .then(|| candidate_path.to_path_buf());
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let full = dir.join(candidate_path);
        if full.is_file() {
            return Some(full);
        }
    }
    None
}

fn parse_profiled_wrix_launcher(script: &str) -> Option<PathBuf> {
    for line in script.lines() {
        let Some(rest) = line.trim_start().strip_prefix("exec ") else {
            continue;
        };
        let mut parts = rest.split_whitespace();
        let Some(program) = parts.next() else {
            continue;
        };
        let Some(flag) = parts.next() else {
            continue;
        };
        if flag == "--profile-config" {
            let program = strip_shell_quotes(program);
            let path = Path::new(program);
            if path.is_absolute()
                && path
                    .file_name()
                    .is_some_and(|name| name == OsStr::new("wrix"))
            {
                return Some(path.to_path_buf());
            }
        }
    }
    None
}

fn strip_shell_quotes(token: &str) -> &str {
    token
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            token
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        })
        .unwrap_or(token)
}

/// Apply [`SpawnConfig::launcher_env`] to the `wrix spawn` child process
/// before it is spawned. These pairs (`WRIX_DEPLOY_KEY` /
/// `WRIX_SIGNING_KEY` → host key paths) are read by the wrix launcher to
/// bind-mount the deploy + signing keys into the bead container; they are
/// deliberately **not** part of [`SpawnConfig::env`] (the in-container
/// allowlist) and never reach the spawn-config JSON. Without this, loop
/// agents boot with no git keys and cannot sign or push (`specs/harness.md`
/// § Repository Git isolation).
///
/// [`SpawnConfig::launcher_env`]: loom_driver::agent::SpawnConfig::launcher_env
/// [`SpawnConfig::env`]: loom_driver::agent::SpawnConfig::env
pub fn apply_launcher_env(cmd: &mut tokio::process::Command, launcher_env: &[(String, String)]) {
    cmd.envs(launcher_env.iter().map(|(k, v)| (k, v)));
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::{apply_launcher_env, deprofiled_wrix, parse_profiled_wrix_launcher};

    #[test]
    fn parse_profiled_wrix_launcher_extracts_raw_exec_target() {
        let script = r#"#!/bin/sh
set -euo pipefail
exec /nix/store/raw-wrix/bin/wrix --profile-config /nix/store/profile.json "$@"
"#;
        assert_eq!(
            parse_profiled_wrix_launcher(script),
            Some(std::path::PathBuf::from("/nix/store/raw-wrix/bin/wrix")),
        );
    }

    #[test]
    fn deprofiled_wrix_uses_raw_launcher_from_configured_wrapper() {
        let dir = tempfile::tempdir().expect("tempdir");
        let raw = dir.path().join("raw/bin/wrix");
        fs::create_dir_all(raw.parent().expect("raw has parent")).expect("mkdir raw");
        fs::write(&raw, "#!/bin/sh\n").expect("write raw");
        let mut raw_perm = fs::metadata(&raw).expect("stat raw").permissions();
        raw_perm.set_mode(0o755);
        fs::set_permissions(&raw, raw_perm).expect("chmod raw");

        let wrapper = dir.path().join("profiled/bin/wrix");
        fs::create_dir_all(wrapper.parent().expect("wrapper has parent")).expect("mkdir wrapper");
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexec {} --profile-config /nix/store/profile.json \"$@\"\n",
                raw.display()
            ),
        )
        .expect("write wrapper");

        assert_eq!(
            deprofiled_wrix(wrapper.as_os_str().to_os_string()),
            raw.into_os_string(),
        );
    }

    /// `apply_launcher_env` places each pair on the child process
    /// environment — the load-bearing half of delivering git keys to a
    /// spawned loop agent. Verified by spawning `printenv` and reading the
    /// value back rather than inspecting `Command` (which exposes no env
    /// getter), so a regression that drops the call surfaces here.
    #[tokio::test]
    async fn apply_launcher_env_sets_child_process_env() {
        let mut cmd = tokio::process::Command::new("printenv");
        cmd.arg("WRIX_SIGNING_KEY");
        apply_launcher_env(
            &mut cmd,
            &[(
                "WRIX_SIGNING_KEY".to_string(),
                "/home/op/.ssh/deploy_keys/k-signing".to_string(),
            )],
        );
        let output = cmd.output().await.expect("spawn printenv");
        assert!(output.status.success(), "printenv must find the var");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "/home/op/.ssh/deploy_keys/k-signing",
        );
    }
}

//! Agent backend implementations for Loom.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;

use loom_driver::agent::{ProtocolError, SpawnConfig};

pub mod claude;
pub mod direct;
pub mod pi;
mod skill;

pub use claude::ClaudeBackend;
pub use direct::DirectBackend;
pub use pi::PiBackend;

pub(crate) fn resolve_wrix_spawn_bin(config: &SpawnConfig) -> Result<OsString, ProtocolError> {
    let launcher = config.wrix_launcher.as_deref().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "missing manifest Wrix launcher",
        )
    })?;
    Ok(loom_driver::wrix::resolve_launcher(launcher, None).into_os_string())
}

pub(crate) fn build_wrix_command(
    wrix_bin: &OsStr,
    profile_config: Option<&Path>,
    spawn_config_path: &Path,
) -> Result<tokio::process::Command, ProtocolError> {
    let profile_config = profile_config.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "missing manifest Wrix profile_config",
        )
    })?;
    let mut cmd = loom_driver::wrix::command(wrix_bin, profile_config);
    cmd.arg("spawn")
        .arg("--spawn-config")
        .arg(spawn_config_path)
        .arg("--stdio");
    Ok(cmd.into())
}

/// Apply host-only launcher inputs, including resolved repository deploy/signing keys.
pub fn apply_launcher_env(cmd: &mut tokio::process::Command, launcher_env: &[(String, String)]) {
    cmd.envs(launcher_env.iter().map(|(k, v)| (k, v)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_rejects_missing_profile_config_instead_of_using_wrapper_defaults() {
        assert!(build_wrix_command(OsStr::new("wrix"), None, Path::new("spawn.json")).is_err());
    }

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

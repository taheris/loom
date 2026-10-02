//! Image-backed Wrix launch contract shared by interactive and RPC sessions.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Explicit raw-launcher override; never a configured profile wrapper.
pub const LAUNCHER_OVERRIDE: &str = "LOOM_WRIX_SPAWN_BIN";

/// Resolve an explicit raw launcher, then the environment override, then the manifest launcher.
/// Overrides change only the executable, not the selected profile configuration.
pub fn resolve_launcher(manifest_launcher: &Path, explicit: Option<&Path>) -> PathBuf {
    explicit.map_or_else(
        || {
            std::env::var_os(LAUNCHER_OVERRIDE)
                .map_or_else(|| manifest_launcher.to_path_buf(), PathBuf::from)
        },
        Path::to_path_buf,
    )
}

/// Start a raw-launcher command with exactly one immutable profile configuration.
/// Callers append the Wrix subcommand before any workspace or agent arguments.
pub fn command(launcher: &OsStr, profile_config: &Path) -> Command {
    let mut command = Command::new(launcher);
    command.arg("--profile-config").arg(profile_config);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_config_precedes_run_and_spawn_arguments() {
        for subcommand in ["run", "spawn"] {
            let mut command = command(OsStr::new("/raw/wrix"), Path::new("/profile config.json"));
            command.arg(subcommand).arg("/workspace");
            assert_eq!(command.get_program(), "/raw/wrix");
            assert_eq!(
                command.get_args().collect::<Vec<_>>(),
                [
                    "--profile-config",
                    "/profile config.json",
                    subcommand,
                    "/workspace"
                ]
            );
        }
    }

    #[test]
    fn explicit_raw_launcher_override_wins_without_changing_profile_config() {
        let launcher = resolve_launcher(
            Path::new("/manifest/wrix"),
            Some(Path::new("/override/wrix")),
        );
        let command = command(launcher.as_os_str(), Path::new("/selected-profile.json"));
        assert_eq!(command.get_program(), "/override/wrix");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["--profile-config", "/selected-profile.json"]
        );
    }
}

//! Image-backed Wrix launch contract shared by interactive and RPC sessions.

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Explicit raw-launcher override; never a configured profile wrapper.
pub const LAUNCHER_OVERRIDE: &str = "LOOM_WRIX_SPAWN_BIN";

/// Resolve a caller override, then consumer pins, then the host's Wrix command.
/// Executable selection never changes the selected immutable profile configuration.
pub fn resolve_launcher(explicit: Option<&Path>) -> PathBuf {
    explicit.map_or_else(
        || {
            std::env::var_os(LAUNCHER_OVERRIDE)
                .or_else(|| std::env::var_os("LOOM_WRIX_BIN"))
                .or_else(|| std::env::var_os("WRIX_BIN"))
                .map_or_else(|| PathBuf::from("wrix"), PathBuf::from)
        },
        Path::to_path_buf,
    )
}

pub(crate) fn executable(launcher: &Path) -> std::io::Result<PathBuf> {
    if launcher.components().count() > 1 || launcher.is_absolute() {
        return launcher.canonicalize();
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&paths) {
            let candidate = directory.join(launcher);
            let metadata = match candidate.metadata() {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
                return candidate.canonicalize();
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "caller Wrix launcher is unavailable",
    ))
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
    fn consumer_pin_precedence_resolves_without_manifest_authority() {
        const EXPECTED: &str = "LOOM_TEST_EXPECTED_WRIX";
        if let Some(expected) = std::env::var_os(EXPECTED) {
            assert_eq!(resolve_launcher(None), PathBuf::from(expected));
            return;
        }
        for (spawn, loom, wrix, expected) in [
            (None, None, None, "wrix"),
            (None, None, Some("/consumer/wrix"), "/consumer/wrix"),
            (
                None,
                Some("/loom/wrix"),
                Some("/consumer/wrix"),
                "/loom/wrix",
            ),
            (
                Some("/override/wrix"),
                Some("/loom/wrix"),
                Some("/consumer/wrix"),
                "/override/wrix",
            ),
        ] {
            let mut child = Command::new(std::env::current_exe().unwrap());
            child.args([
                "--exact",
                "wrix::tests::consumer_pin_precedence_resolves_without_manifest_authority",
            ]);
            child.env(EXPECTED, expected);
            for (key, value) in [
                (LAUNCHER_OVERRIDE, spawn),
                ("LOOM_WRIX_BIN", loom),
                ("WRIX_BIN", wrix),
            ] {
                child.env_remove(key);
                if let Some(value) = value {
                    child.env(key, value);
                }
            }
            let output = child.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        }
    }

    #[test]
    fn explicit_raw_launcher_override_wins_without_changing_profile_config() {
        let launcher = resolve_launcher(Some(Path::new("/override/wrix")));
        let command = command(launcher.as_os_str(), Path::new("/selected-profile.json"));
        assert_eq!(command.get_program(), "/override/wrix");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["--profile-config", "/selected-profile.json"]
        );
    }
}

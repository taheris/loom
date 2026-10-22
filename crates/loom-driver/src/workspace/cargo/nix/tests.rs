#[cfg(test)]
mod cases {
    use super::super::*;
    use crate::workspace::cargo::{command, verifier_command};

    const CHILD: &str = "LOOM_NIX_OUTPUT_CHILD";
    const ROOT: &str = "LOOM_NIX_OUTPUT_ROOT";

    /// Real subprocesses establish the selected executable, environment and exit status.
    fn isolated(name: &str, run: impl FnOnce(&Path)) {
        if std::env::var(CHILD).as_deref() == Ok(name) {
            let source = PathBuf::from(std::env::var_os(ROOT).unwrap()).join("source");
            std::fs::create_dir(&source).unwrap();
            run(&source);
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("workspace::cargo::nix::tests::cases::{name}"),
                "--nocapture",
            ])
            .env(CHILD, name)
            .env(ROOT, root.path())
            .env("XDG_STATE_HOME", root.path().join("state"))
            .env("LOOM_TEST_STORAGE", "1")
            .env_remove(crate::agent::LOOM_INSIDE_ENV)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }

    fn pinned() -> PathBuf {
        pin()
            .unwrap()
            .expect("initialize native Cargo patches before building tests")
    }

    fn app(root: &Path) -> PathBuf {
        let source = root.join("app");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("flake.nix"), format!(
            "{{ outputs = _: let app = {{ type = \"app\"; program = \"{}\"; }}; in {{ apps = builtins.listToAttrs (map (name: {{ inherit name; value.probe = app; }}) [ \"x86_64-linux\" \"aarch64-linux\" \"x86_64-darwin\" \"aarch64-darwin\" ]); }}; }}",
            pinned().display(),
        )).unwrap();
        source
    }

    fn state(command: &Command) -> PathBuf {
        PathBuf::from(
            command
                .get_envs()
                .find(|(key, _)| *key == "XDG_STATE_HOME")
                .unwrap()
                .1
                .unwrap(),
        )
    }

    #[test]
    fn nix_owned_verifiers_repeat_success_and_failure_without_native_reservations() {
        isolated(
            "nix_owned_verifiers_repeat_success_and_failure_without_native_reservations",
            |root| {
                let source = app(root);
                for valid in [true, true, false, false] {
                    let mut cmd = command(pinned(), &source);
                    cmd.args([
                        "run",
                        "--extra-experimental-features",
                        "nix-command flakes",
                        "--no-write-lock-file",
                        "--store",
                    ])
                    .arg(root.join("store"))
                    .arg(format!("path:{}#probe", source.display()))
                    .args([
                        "--", "eval", "--store", "dummy://", "--impure", "--raw", "--expr",
                    ])
                    .arg(if valid {
                        "builtins.getEnv \"CARGO_TARGET_DIR\""
                    } else {
                        "throw \"failure control\""
                    })
                    .env("HOME", root.join("home"))
                    .env("XDG_CACHE_HOME", root.join("cache"));
                    let execution = verifier_command(&mut cmd, &source).unwrap();
                    let state = state(&cmd);
                    let output = cmd.output().unwrap();
                    assert_eq!(output.status.success(), valid, "{output:?}");
                    if valid {
                        assert_eq!(output.stdout, b"/dev/null/loom-cargo-discovery");
                    }
                    assert!(!state.join("loom/trusted-cargo/state.db").exists());
                    assert!(!source.join("target").exists());
                    drop(execution);
                }
                let output = Command::new(pinned())
                    .args([
                        "store",
                        "gc",
                        "--extra-experimental-features",
                        "nix-command",
                        "--store",
                    ])
                    .arg(root.join("store"))
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{output:?}");
            },
        );
    }

    #[test]
    fn nix_owned_commands_preserve_cleared_environment_without_rebuilding_command() {
        isolated(
            "nix_owned_commands_preserve_cleared_environment_without_rebuilding_command",
            |root| {
                let mut cmd = command(pinned(), root);
                cmd.env_clear()
                    .args([
                        "eval",
                        "--extra-experimental-features",
                        "nix-command",
                        "--store",
                        "dummy://",
                        "--impure",
                        "--raw",
                        "--expr",
                        "builtins.getEnv \"LOOM_NIX_OUTPUT_CHILD\"",
                    ])
                    .env("HOME", root.join("home"));
                let _execution = verifier_command(&mut cmd, root).unwrap();
                let output = cmd.output().unwrap();
                assert!(output.status.success(), "{output:?}");
                assert_eq!(output.stdout, b"");
                assert!(!state(&cmd).join("loom/trusted-cargo/state.db").exists());
            },
        );
    }

    #[test]
    fn nix_shadowing_wrappers_and_native_environments_remain_managed() {
        isolated(
            "nix_shadowing_wrappers_and_native_environments_remain_managed",
            |root| {
                let fake = root.join("nix");
                std::fs::write(
                    &fake,
                    loom_test_support::bash_script(
                        "set -euo pipefail\nprintf '%s' \"$CARGO_TARGET_DIR\"\n",
                    ),
                )
                .unwrap();
                std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
                let mut cmd = command(&fake, root);
                cmd.arg("run");
                let _execution = verifier_command(&mut cmd, root).unwrap();
                let output = cmd.output().unwrap();
                assert!(output.status.success(), "{output:?}");
                let target = PathBuf::from(String::from_utf8(output.stdout).unwrap());
                assert!(target.starts_with(state(&cmd).join("loom/trusted-cargo/native")));
                for mode in ["shell", "develop"] {
                    let mut cmd = command(pinned(), root);
                    cmd.args([mode, "--command", "cargo", "build"]);
                    assert!(!owns_output(&cmd));
                    let _execution = verifier_command(&mut cmd, root).unwrap();
                    assert!(cmd.get_envs().any(|(key, value)| key == "CARGO_TARGET_DIR"
                        && Path::new(value.unwrap()).starts_with(state(&cmd))));
                }
                let mut cmd = command("sh", root);
                cmd.args(["-c", "mkdir -p \"$CARGO_TARGET_DIR\"; printf native > \"$CARGO_TARGET_DIR/wrapper-output\""]);
                let _execution = verifier_command(&mut cmd, root).unwrap();
                assert!(cmd.status().unwrap().success());
                assert!(state(&cmd).join("loom/trusted-cargo/state.db").exists());
            },
        );
    }
}

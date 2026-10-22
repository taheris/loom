//! Discovery and fixture isolation require real subprocess environment propagation.

#[cfg(test)]
mod cases {
    use super::super::*;

    #[test]
    fn cargo_discovery_metadata_reserves_no_storage_on_repeated_success_and_failure() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let state = fixture.path().join("state");
        fs::create_dir_all(source.join("src")).unwrap();
        fs::write(source.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
        fs::write(
            source.join("Cargo.toml"),
            "[package]\nname='discovery-fixture'\nversion='0.1.0'\nedition='2024'\n",
        )
        .unwrap();
        for valid in [true, true, false, false] {
            if !valid {
                fs::write(source.join("Cargo.toml"), "not a manifest").unwrap();
            }
            let mut command = Command::new("cargo");
            command
                .args(["metadata", "--offline", "--format-version=1"])
                .current_dir(&source)
                .env("XDG_STATE_HOME", &state)
                .env("CARGO_TARGET_DIR", fixture.path().join("ambient"));
            discovery_command(&mut command);
            let output = command.output().unwrap();
            assert_eq!(output.status.success(), valid, "{output:?}");
            assert!(!source.join("target").exists());
            assert!(!fixture.path().join("ambient").exists());
            assert!(!state.exists());
        }
    }

    #[test]
    fn cargo_discovery_denies_compiling_wrappers_instead_of_exempting_command_names() {
        let fixture = tempfile::tempdir().unwrap();
        for program in ["sh", "bash"] {
            let mut command = Command::new(program);
            command
                .args([
                    "-c",
                    "mkdir -p \"$CARGO_TARGET_DIR\" && touch \"$CARGO_TARGET_DIR/compiled\"",
                ])
                .env("CARGO_TARGET_DIR", fixture.path().join("ambient"));
            discovery_command(&mut command);
            assert!(!command.output().unwrap().status.success());
            assert!(!fixture.path().join("ambient").exists());
        }
    }

    #[test]
    fn unwrapped_fixture_commands_cannot_charge_inherited_operator_state() {
        if std::env::var_os("LOOM_UNWRAPPED_STORAGE_CHILD").is_some() {
            let source = tempfile::tempdir().unwrap();
            for program in ["sh", "missing-loom-fixture-command", "sh"] {
                let mut command = Command::new(program);
                command.args(["-c", "mkdir -p \"$CARGO_TARGET_DIR\"; exit 23"]);
                let execution = trusted_command(&mut command, source.path()).unwrap();
                let root = execution.fixture.as_ref().unwrap().path().to_path_buf();
                let selected = command
                    .get_envs()
                    .find(|(key, _)| *key == "XDG_STATE_HOME")
                    .unwrap()
                    .1
                    .unwrap();
                assert_ne!(selected, std::env::var_os("XDG_STATE_HOME").unwrap());
                let result = command.output();
                if program == "sh" {
                    assert_eq!(result.unwrap().status.code(), Some(23));
                } else {
                    match result {
                        Ok(output) => assert!(!output.status.success()),
                        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
                    }
                }
                assert!(root.exists());
                drop(execution);
                assert!(!root.exists());
            }
            return;
        }
        let fixture = tempfile::tempdir().unwrap();
        let operator = fixture.path().join("operator-state");
        fs::create_dir(&operator).unwrap();
        fs::write(operator.join("sentinel"), b"protected").unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "workspace::cargo::discovery_tests::cases::unwrapped_fixture_commands_cannot_charge_inherited_operator_state", "--nocapture"])
        .env("LOOM_UNWRAPPED_STORAGE_CHILD", "1")
        .env_remove("LOOM_TEST_STORAGE")
        .env_remove(crate::agent::LOOM_INSIDE_ENV)
        .env("XDG_STATE_HOME", &operator)
        .output()
        .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(fs::read_dir(&operator).unwrap().count(), 1);
        assert_eq!(fs::read(operator.join("sentinel")).unwrap(), b"protected");
    }

    #[test]
    fn private_fixture_runner_isolates_descendants_and_cleans_success_and_failure() {
        if std::env::var_os("NEXTEST_RUN_ID").is_some() {
            assert_eq!(std::env::var("LOOM_TEST_STORAGE").as_deref(), Ok("1"));
        }
        if std::env::var_os("LOOM_FIXTURE_STORAGE_CHILD").is_some() {
            let temporary = PathBuf::from(std::env::var_os("TMPDIR").unwrap());
            for key in [
                "CARGO_TARGET_DIR",
                "CARGO_BUILD_TARGET_DIR",
                "CARGO_BUILD_BUILD_DIR",
            ] {
                assert_eq!(
                    PathBuf::from(std::env::var_os(key).unwrap()),
                    temporary.join("cargo-target")
                );
            }
            for key in [
                "RUSTC_WRAPPER",
                "CARGO_BUILD_RUSTC_WRAPPER",
                "RUSTC_WORKSPACE_WRAPPER",
                "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
            ] {
                assert_eq!(std::env::var(key).as_deref(), Ok(""), "inherited {key}");
            }
            for (key, name) in [
                ("SCCACHE_DIR", "sccache"),
                ("SCCACHE_SERVER_UDS", "sccache.sock"),
            ] {
                assert_eq!(
                    PathBuf::from(std::env::var_os(key).unwrap()),
                    temporary.join(name)
                );
            }
            let source = tempfile::tempdir().unwrap();
            let mut command = Command::new("sh");
            command
            .args([
                "-c",
                "test \"$XDG_STATE_HOME\" = \"$EXPECTED_STATE\" && test \"$LOOM_TEST_STORAGE\" = 1 && mkdir -p \"$CARGO_TARGET_DIR\" && printf fixture > \"$CARGO_TARGET_DIR/output\"",
            ])
            .env("EXPECTED_STATE", std::env::var_os("XDG_STATE_HOME").unwrap())
            .env("XDG_STATE_HOME", source.path().join("wrong-state"))
            .env_remove("LOOM_TEST_STORAGE");
            let _execution = verifier_command(&mut command, source.path()).unwrap();
            assert!(command.status().unwrap().success());
            println!("fixture-state={}", std::env::var("XDG_STATE_HOME").unwrap());
            return;
        }
        let fixture = tempfile::tempdir().unwrap();
        let operator = fixture.path().join("operator-state");
        fs::create_dir(&operator).unwrap();
        fs::write(operator.join("sentinel"), b"protected").unwrap();
        let runner =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/run-private-fixture-tests");
        for code in [0, 0, 23, 23] {
            let output = Command::new("bash")
            .arg(&runner)
            .arg("--test-process")
            .args(["bash", "-c", "\"$@\"; result=$?; if (( result != 0 )); then exit \"$result\"; fi; exit \"$FIXTURE_EXIT\"", "bash"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "workspace::cargo::discovery_tests::cases::private_fixture_runner_isolates_descendants_and_cleans_success_and_failure", "--nocapture"])
            .env("LOOM_FIXTURE_STORAGE_CHILD", "1")
            .env("FIXTURE_EXIT", code.to_string())
            .env("XDG_STATE_HOME", &operator)
            .env("CARGO_TARGET_DIR", fixture.path().join("ambient"))
            .env("CARGO_BUILD_TARGET_DIR", fixture.path().join("ambient"))
            .env("CARGO_BUILD_BUILD_DIR", fixture.path().join("ambient"))
            .env("RUSTC_WRAPPER", "operator-wrapper")
            .env("SCCACHE_DIR", fixture.path().join("cache"))
            .env("SCCACHE_SERVER_UDS", fixture.path().join("operator-cache.sock"))
            .env_remove(crate::agent::LOOM_INSIDE_ENV)
            .output()
            .unwrap();
            assert_eq!(output.status.code(), Some(code), "{output:?}");
            let stdout = String::from_utf8(output.stdout).unwrap();
            let state = stdout
                .lines()
                .find_map(|line| line.strip_prefix("fixture-state="))
                .unwrap();
            assert!(!Path::new(state).parent().unwrap().exists());
            assert!(!fixture.path().join("ambient").exists());
            assert!(!fixture.path().join("cache").exists());
            assert_eq!(fs::read_dir(&operator).unwrap().count(), 1);
            assert_eq!(fs::read(operator.join("sentinel")).unwrap(), b"protected");
        }
    }
}

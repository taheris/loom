#[cfg(test)]
mod cases {
    use std::{
        path::{Path, PathBuf},
        process::{Command, Output},
    };

    struct Fixture {
        root: tempfile::TempDir,
        source: PathBuf,
        tools: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("bootstrap fixture");
            let source = root.path().join("source with spaces");
            let tools = root.path().join("tools");
            std::fs::create_dir_all(source.join("bin")).expect("source bin");
            std::fs::create_dir_all(source.join("nested")).expect("nested caller");
            std::fs::create_dir_all(source.join("scripts")).expect("source scripts");
            std::fs::create_dir_all(&tools).expect("external tools");
            std::fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bin/loom"),
                source.join("bin/loom"),
            )
            .expect("source entry point");
            std::fs::write(
                source.join("scripts/configure-cargo-patches.sh"),
                "set -euo pipefail\nprintf initialized > \"$BOOTSTRAP_PATCHES\"\n",
            )
            .expect("patch initializer probe");
            let mut git = Command::new("git");
            loom_test_support::scrub_git_local_env(&mut git);
            let output = git
                .args(["init", "-q"])
                .arg(&source)
                .output()
                .expect("initialize source repository");
            assert_success(&output);
            let fixture = Self {
                root,
                source,
                tools,
            };
            fixture.install(
            "nix",
            r#"printf '%s\0' "$@" > "$BOOTSTRAP_ARGS"
[[ -z "${CARGO_TARGET_DIR+x}${CARGO_BUILD_TARGET_DIR+x}${CARGO_BUILD_BUILD_DIR+x}" ]]
[[ -z "$RUSTC_WRAPPER$CARGO_BUILD_RUSTC_WRAPPER$RUSTC_WORKSPACE_WRAPPER$CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER" ]]
[[ -z "${!SCCACHE_@}" ]]
[[ -z "${GIT_INDEX_FILE+x}" ]]
printf '%s\n' "$PWD" > "$BOOTSTRAP_CALLED"
exit "${BOOTSTRAP_NIX_STATUS:-0}"
"#,
        );
            fixture.install("cargo", "printf cargo > \"$BOOTSTRAP_CALLED\"\nexit 91\n");
            fixture.install("mktemp", "printf mktemp > \"$BOOTSTRAP_CALLED\"\nexit 92\n");
            fixture
        }

        fn install(&self, name: &str, body: &str) {
            loom_test_support::write_executable_bash_script(
                self.tools.join(name),
                &format!("set -euo pipefail\n{body}"),
            )
            .expect("external tool probe");
        }

        fn command(&self) -> Command {
            let mut path = vec![self.tools.clone()];
            path.extend(std::env::split_paths(
                &std::env::var_os("PATH").expect("tool PATH"),
            ));
            let mut command = Command::new("bash");
            loom_test_support::scrub_git_local_env(&mut command);
            command
                .arg(self.source.join("bin/loom"))
                .current_dir(self.source.join("nested"))
                .env("PATH", std::env::join_paths(path).expect("fixture PATH"))
                .env("BOOTSTRAP_ARGS", self.root.path().join("args"))
                .env("BOOTSTRAP_CALLED", self.root.path().join("called"))
                .env("BOOTSTRAP_PATCHES", self.root.path().join("patches"))
                .env("GIT_INDEX_FILE", self.root.path().join("foreign-index"))
                .env_remove("LOOM_INSIDE");
            command
        }

        fn args(&self) -> Vec<String> {
            std::fs::read(self.root.path().join("args"))
                .expect("recorded arguments")
                .strip_suffix(&[0])
                .expect("NUL-terminated arguments")
                .split(|byte| *byte == 0)
                .map(|arg| String::from_utf8(arg.to_vec()).expect("UTF-8 argument"))
                .collect()
        }
    }

    fn assert_success(output: &Output) {
        assert!(
            output.status.success(),
            "status={}\nstdout={}\nstderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn host_bootstrap_uses_locked_nix_without_unmanaged_cargo() {
        let fixture = Fixture::new();
        let legacy = fixture.root.path().join("loom-trusted-bootstrap.legacy");
        std::fs::create_dir(&legacy).expect("old bootstrap output");
        std::fs::write(legacy.join("driver"), "protected").expect("legacy executable");
        let output = fixture
            .command()
            .args(["gate", "verify", "--diff", "HEAD~2..HEAD", "two words"])
            .env("CARGO_TARGET_DIR", &legacy)
            .env("CARGO_BUILD_TARGET_DIR", &legacy)
            .env("CARGO_BUILD_BUILD_DIR", &legacy)
            .env("RUSTC_WRAPPER", "worker-wrapper")
            .env("CARGO_BUILD_RUSTC_WRAPPER", "worker-wrapper")
            .env("RUSTC_WORKSPACE_WRAPPER", "worker-wrapper")
            .env("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER", "worker-wrapper")
            .env("SCCACHE_DIR", &legacy)
            .env("SCCACHE_SERVER_UDS", legacy.join("server"))
            .env("SCCACHE_DISABLE", "1")
            .output()
            .expect("host bootstrap");
        assert_success(&output);
        assert_eq!(
            fixture.args(),
            vec![
                "run".to_owned(),
                "--no-update-lock-file".to_owned(),
                "--no-write-lock-file".to_owned(),
                format!(
                    "{}#loom",
                    fixture.source.canonicalize().expect("source").display()
                ),
                "--".to_owned(),
                "gate".to_owned(),
                "verify".to_owned(),
                "--diff".to_owned(),
                "HEAD~2..HEAD".to_owned(),
                "two words".to_owned(),
            ]
        );
        assert_eq!(
            std::fs::read_to_string(fixture.root.path().join("called")).expect("Nix call"),
            format!(
                "{}\n",
                fixture
                    .source
                    .join("nested")
                    .canonicalize()
                    .expect("caller")
                    .display()
            )
        );
        assert!(!fixture.root.path().join("patches").exists());
        assert_eq!(
            std::fs::read_to_string(legacy.join("driver")).expect("legacy preserved"),
            "protected"
        );
    }

    #[test]
    fn host_bootstrap_nix_failure_never_falls_back_to_unmanaged_cargo() {
        let fixture = Fixture::new();
        let legacy = fixture.root.path().join("loom-trusted-bootstrap.legacy");
        std::fs::create_dir(&legacy).expect("old bootstrap output");
        std::fs::write(legacy.join("driver"), "protected").expect("legacy executable");
        let output = fixture
            .command()
            .arg("--version")
            .env("BOOTSTRAP_NIX_STATUS", "73")
            .output()
            .expect("failed host bootstrap");
        assert_eq!(output.status.code(), Some(73), "{output:?}");
        assert!(!fixture.root.path().join("patches").exists());
        assert_ne!(
            std::fs::read_to_string(fixture.root.path().join("called")).expect("Nix call"),
            "cargo"
        );
        assert_eq!(
            std::fs::read_to_string(legacy.join("driver")).expect("legacy preserved"),
            "protected"
        );
    }

    #[test]
    fn worker_bootstrap_preserves_projected_cargo_target_and_shared_cache() {
        let fixture = Fixture::new();
        fixture.install("nix", "exit 93\n");
        fixture.install("sccache", "exit 94\n");
        fixture.install(
            "cargo",
            r#"[[ "$(cat "$BOOTSTRAP_PATCHES")" == initialized ]]
[[ "$CARGO_TARGET_DIR" == "$BOOTSTRAP_TARGET" ]]
[[ "$CARGO_BUILD_TARGET_DIR" == "$BOOTSTRAP_TARGET" ]]
[[ "$CARGO_BUILD_BUILD_DIR" == "$BOOTSTRAP_TARGET" ]]
[[ "$RUSTC_WRAPPER" == "$BOOTSTRAP_SCCACHE" ]]
[[ "$CARGO_BUILD_RUSTC_WRAPPER" == "$BOOTSTRAP_SCCACHE" ]]
[[ "$SCCACHE_DIR" == "$BOOTSTRAP_CACHE" ]]
[[ -z "${GIT_INDEX_FILE+x}" ]]
printf '%s\0' "$@" > "$BOOTSTRAP_ARGS"
exit 74
"#,
        );
        let target = fixture.root.path().join("projected target");
        let cache = fixture.root.path().join("shared cache");
        let output = fixture
            .command()
            .args(["gate", "verify"])
            .env("LOOM_INSIDE", "1")
            .env("BOOTSTRAP_TARGET", &target)
            .env("CARGO_TARGET_DIR", &target)
            .env("CARGO_BUILD_TARGET_DIR", &target)
            .env("CARGO_BUILD_BUILD_DIR", &target)
            .env("BOOTSTRAP_SCCACHE", fixture.tools.join("sccache"))
            .env("BOOTSTRAP_CACHE", &cache)
            .env("SCCACHE_DIR", &cache)
            .output()
            .expect("worker feedback bootstrap");
        assert_eq!(output.status.code(), Some(74), "{output:?}");
        assert_eq!(
            fixture.args(),
            vec![
                "run".to_owned(),
                "--bin".to_owned(),
                "loom".to_owned(),
                "--manifest-path".to_owned(),
                fixture
                    .source
                    .canonicalize()
                    .expect("source")
                    .join("Cargo.toml")
                    .display()
                    .to_string(),
                "--".to_owned(),
                "gate".to_owned(),
                "verify".to_owned(),
            ]
        );
        assert!(!fixture.root.path().join("called").exists());
    }
}

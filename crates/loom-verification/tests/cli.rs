#[cfg(test)]
mod cli {
    use serde_json::{Value, json};
    use std::{
        env, fs,
        os::unix::fs::{PermissionsExt, symlink},
        path::{Path, PathBuf},
        process::{Command, Output},
    };

    struct Fixture {
        _root: tempfile::TempDir,
        root: PathBuf,
        repo: PathBuf,
        tools: PathBuf,
        calls: PathBuf,
    }
    impl Fixture {
        fn new(script: &str) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().canonicalize().unwrap();
            let repo = root.join("repo");
            let tools = root.join("tools");
            fs::create_dir(&repo).unwrap();
            fs::create_dir(&tools).unwrap();
            let bash = env::split_paths(&env::var_os("PATH").expect("fixture PATH"))
                .map(|directory| directory.join("bash"))
                .find(|candidate| candidate.is_file())
                .expect("provisioned Bash")
                .canonicalize()
                .expect("canonical Bash");
            for name in ["nix", "nix-store"] {
                let path = tools.join(name);
                fs::write(&path,format!("#!{}\nset -euo pipefail\nprintf '%s %s\\n' \"$(basename \"$0\")\" \"$*\" >> \"$CALLS\"\n{script}\n", bash.display())).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
            }
            let calls = root.join("calls");
            Self {
                _root: directory,
                root,
                repo,
                tools,
                calls,
            }
        }
        fn command(&self) -> Command {
            let binary = env::var_os("LOOM_VERIFICATION_BIN")
                .unwrap_or_else(|| env!("CARGO_BIN_EXE_loom-verification").into());
            let mut command = Command::new(binary);
            let mut paths = vec![self.tools.clone()];
            paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
            command
                .current_dir(&self.repo)
                .env("PATH", env::join_paths(paths).unwrap())
                .env("CALLS", &self.calls);
            command
        }
        fn calls(&self) -> Vec<String> {
            fs::read_to_string(&self.calls)
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect()
        }
    }
    fn json(output: &Output) -> Value {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    const LIVE: &str = "/nix/store/00000000000000000000000000000000-live";
    const DEAD: &str = "/nix/store/11111111111111111111111111111111-dead";
    fn storage() -> Fixture {
        let fixture = Fixture::new(
            r#"case "$*" in
  *'config show --json') printf '%s\n' "$CONFIG" ;;
  *'path-info --all --json --json-format 1') printf '%s\n' "$METADATA" ;;
  '--gc --print-dead') printf '%s\n' "$DEAD_PATH" ;;
  '--gc --print-roots') printf 'operator-root -> %s\n' "$LIVE_PATH" ;;
  *) printf 'unexpected or destructive Nix operation\n' >&2; exit 71 ;;
esac"#,
        );
        let build = fixture.root.join("builds/nix-build-example.drv-0");
        fs::create_dir_all(&build).unwrap();
        fs::write(build.join("diagnostic"), "retained").unwrap();
        fixture
    }
    fn report(fixture: &Fixture, dead: bool, metadata: &Value, dead_path: &str) -> Output {
        let mut command = fixture.command();
        command.arg("storage");
        if dead {
            command.arg("--dead");
        }
        command
        .env(
            "CONFIG",
            json!({"keep-failed":{"value":true},"build-dir":{"value":fixture.root.join("builds")}})
                .to_string(),
        )
        .env("METADATA", metadata.to_string())
        .env("DEAD_PATH", dead_path)
        .env("LIVE_PATH", LIVE)
        .output()
        .unwrap()
    }
    fn sizes() -> Value {
        json!({LIVE:{"narSize":2_147_483_648_u64},DEAD:{"narSize":1_073_741_824_u64}})
    }

    #[test]
    fn storage_default_reads_database_without_gc_or_tree_walk() {
        let fixture = storage();
        let report = json(&report(&fixture, false, &sizes(), DEAD));
        assert_eq!(report["store_NAR_GiB"], 3.0);
        assert_eq!(report["largest_store_paths"][0]["path"], LIVE);
        assert_eq!(report["largest_store_paths"][0]["NAR_GiB"], 2.0);
        assert_eq!(report["largest_store_paths"][1]["path"], DEAD);
        assert!(report.get("dead_paths").is_none());
        assert!(report["sizes"].as_str().unwrap().contains("NOT physical"));
        let calls = fixture.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].contains("config show --json"));
        assert!(calls[1].contains("--all") && !calls[1].contains("--recursive"));
        assert_eq!(report["keep_failed_builds"], true);
        assert!(
            report["temporary_build_directories"]
                .as_array()
                .unwrap()
                .contains(&json!(fixture.root.join("builds/nix-build-example.drv-0")))
        );
        assert_eq!(
            fs::read_to_string(
                fixture
                    .root
                    .join("builds/nix-build-example.drv-0/diagnostic")
            )
            .unwrap(),
            "retained"
        );
        assert!(!fixture.repo.join("target").exists());
    }
    #[test]
    fn storage_dead_mode_only_requests_native_read_only_gc_queries() {
        let fixture = storage();
        let report = json(&report(&fixture, true, &sizes(), DEAD));
        assert_eq!(report["dead_NAR_GiB"], 1.0);
        assert_eq!(report["dead_paths_missing_from_metadata"], json!([]));
        assert_eq!(report["largest_dead_paths"][0]["path"], DEAD);
        let roots = Path::new(report["logs"].as_str().unwrap()).join("gc-roots");
        assert_eq!(
            fs::read_to_string(roots).unwrap(),
            format!("operator-root -> {LIVE}\n")
        );
        assert_eq!(
            &fixture.calls()[2..],
            [
                "nix-store --gc --print-dead",
                "nix-store --gc --print-roots"
            ]
        );
    }
    #[test]
    fn storage_malformed_sizes_fail_instead_of_reporting_zero() {
        for value in [json!(true), json!(-1), json!("large"), Value::Null] {
            let fixture = storage();
            let output = report(&fixture, false, &json!({LIVE:{"narSize":value}}), DEAD);
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("no zero-size fallback"));
        }
    }
    #[test]
    fn storage_unknown_dead_paths_have_no_complete_reclaimable_size() {
        let fixture = storage();
        let report = json(&report(
            &fixture,
            true,
            &sizes(),
            "/nix/store/22222222222222222222222222222222-changed-during-audit",
        ));
        assert_eq!(
            report["dead_paths_missing_from_metadata"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(report["dead_NAR_GiB"].is_null());
    }

    #[test]
    fn native_metadata_passes_canonical_roots_and_clears_ambient_nix_policy() {
        let fixture = Fixture::new(
            r#"[[ ! ${NIX_CONFIG+x} && ! ${NIX_PATH+x} ]]
case "$1 $2" in
  'flake archive')
    [[ "$3" == "git+file://$CAPTURE" ]]
    mkdir "$CAPTURE/captured"
    printf '{"path":"%s/captured"}\n' "$CAPTURE" ;;
  'flake metadata')
    [[ "$3" == "path:$CAPTURE/captured" ]]
    printf '{"path":"%s/captured","url":"path:%s/captured"}\n' "$CAPTURE" "$CAPTURE" ;;
  'config show') printf '{"system":{"value":"fixture-system"}}\n' ;;
  'eval --json') printf '[]\n' ;;
  *) exit 71 ;;
esac"#,
        );
        let alias = fixture.root.join("alias");
        symlink(&fixture.repo, &alias).unwrap();
        let output = fixture
            .command()
            .args(["native-resources", alias.to_str().unwrap()])
            .env("CAPTURE", &fixture.repo)
            .env("NIX_CONFIG", "untrusted override")
            .env("NIX_PATH", "untrusted path")
            .output()
            .unwrap();
        assert_eq!(json(&output), json!([]));
        assert!(fixture.calls()[0].contains(&format!(
            "flake archive git+file://{}",
            fixture.repo.display()
        )));
        assert!(fixture.repo.join("captured").is_dir());
    }
    #[test]
    fn native_malformed_metadata_fails_without_claiming_a_partition_pass() {
        let fixture = Fixture::new("printf '{}\\n'");
        let output = fixture
            .command()
            .arg("native-resources")
            .arg(&fixture.repo)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("missing captured source"));
    }

    #[test]
    fn preparation_check_never_acquires_and_parses_invalid_stdout_on_exit_zero() {
        let fixture = Fixture::new(
            r#"[[ "$NIX_REMOTE" == daemon ]]
[[ "$(basename "$0")" == nix-store && "$1 $2" == '--check-validity --print-invalid' ]]
printf '%s\n' "$3"
"#,
        );
        let sdk = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-sdk";
        let output = fixture
            .command()
            .args([
                "prepare",
                "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-sdk.drv",
                sdk,
                "check",
            ])
            .env("NIX_REMOTE", "daemon")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(78));
        assert!(String::from_utf8_lossy(&output.stderr).contains("before dispatch"));
        assert_eq!(
            fixture.calls(),
            vec![format!("nix-store --check-validity --print-invalid {sdk}")]
        );
    }
    #[test]
    fn preparation_registered_but_absent_sdk_blocks_dispatch_without_acquisition() {
        let fixture = Fixture::new("exit 0");
        let sdk = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-absent-sdk";
        assert!(!std::path::Path::new(sdk).exists());
        let output = fixture
            .command()
            .args([
                "prepare",
                "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-sdk.drv",
                sdk,
                "check",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(78));
        assert!(String::from_utf8_lossy(&output.stderr).contains("before dispatch"));
        assert_eq!(
            fixture.calls(),
            vec![format!("nix-store --check-validity --print-invalid {sdk}")]
        );
    }

    #[test]
    fn preparation_denial_blocks_dispatch_and_retains_bounded_store_diagnostic() {
        let fixture = Fixture::new(
            r#"[[ "$NIX_REMOTE" == daemon ]]
printf 'network denied\n' >&2
exit 124"#,
        );
        let sdk = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-sdk";
        let drv = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-sdk.drv";
        let state = fixture.root.join("state");
        let output = fixture
            .command()
            .args(["prepare", drv, sdk, "prepare"])
            .env("XDG_STATE_HOME", &state)
            .env("NIX_REMOTE", "daemon")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(78));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("dispatch remains blocked"),
            "{output:?}"
        );
        let log =
            state.join("loom/verification/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-sdk/provision.log");
        assert!(fs::read_to_string(log).unwrap().contains("network denied"));
        let calls = fixture.calls();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].starts_with(&format!("nix build {drv}^out --out-link ")));
        assert!(
            calls[0].contains("--option connect-timeout 10 --option stalled-download-timeout 30")
        );
    }
}

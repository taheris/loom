#[cfg(test)]
pub(super) mod cases {
    use super::super::*;
    use crate::config::CargoTargetReuse;
    use crate::state::CacheDb;

    pub(in crate::workspace::cargo) fn fixture()
    -> (tempfile::TempDir, Association, PathBuf, PathBuf) {
        let root =
            tempfile::tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
        let operator = root.path().join("operator");
        fs::create_dir_all(operator.join(".git")).unwrap();
        fs::create_dir_all(operator.join(".loom/integration/.git")).unwrap();
        let worker = operator.join(".loom/beads/lm-cargo");
        fs::create_dir_all(worker.join(".git")).unwrap();
        fs::write(
            worker.join("Cargo.toml"),
            "[package]\nname='fixture'\nversion='0.1.0'\n",
        )
        .unwrap();
        fs::write(
            worker.join("Cargo.lock"),
            "version=4\n[[package]]\nname='fixture'\nversion='0.1.0'\n",
        )
        .unwrap();
        let profile = root.path().join("profile.json");
        fs::write(&profile, r#"{"profile":{"mounts":[]}}"#).unwrap();
        let association = Association::resolve(&operator).unwrap();
        (root, association, worker, profile)
    }

    #[test]
    fn worker_cargo_storage_isolates_contexts_and_preserves_retry() {
        let (_root, association, worker, profile) = fixture();
        fs::create_dir(worker.join("target")).unwrap();
        fs::write(worker.join("target/legacy"), "preserved").unwrap();
        let first = prepare(
            &association,
            &worker,
            &profile,
            CargoTargetReuse::CopyOnWrite,
            retention::Policy::default().default_request(),
            &crate::clock::MockClock::new(),
        )
        .unwrap();
        fs::write(first.target().join("retained"), "retry output").unwrap();
        let second = prepare(
            &association,
            &worker,
            &profile,
            CargoTargetReuse::Off,
            retention::Policy::default().default_request(),
            &crate::clock::MockClock::new(),
        )
        .unwrap();
        assert_ne!(first.target(), second.target());
        assert_eq!(
            first.target().parent().unwrap().parent(),
            second.target().parent().unwrap().parent()
        );
        fs::write(
            worker.join("rust-toolchain.toml"),
            "[toolchain]\nchannel='stable'\n",
        )
        .unwrap();
        let changed = prepare(
            &association,
            &worker,
            &profile,
            CargoTargetReuse::Off,
            retention::Policy::default().default_request(),
            &crate::clock::MockClock::new(),
        )
        .unwrap();
        assert_ne!(
            second.target().parent().unwrap().parent(),
            changed.target().parent().unwrap().parent()
        );
        assert_eq!(
            fs::read_to_string(worker.join("target/legacy")).unwrap(),
            "preserved"
        );
        assert_eq!(
            fs::read_to_string(first.target().join("retained")).unwrap(),
            "retry output"
        );
        for namespace in [&first, &second, &changed] {
            let mount = MountSpec {
                host_path: namespace.target(),
                container_path: WORKER_TARGET.into(),
                read_only: false,
            };
            association
                .validate_worker(&worker, &[mount], Some(&profile))
                .unwrap();
        }
        let sibling = association.operator().join(".loom/beads/lm-other");
        fs::create_dir_all(sibling.join(".git")).unwrap();
        let mount = MountSpec {
            host_path: first.target(),
            container_path: WORKER_TARGET.into(),
            read_only: false,
        };
        assert!(
            association
                .validate_worker(&sibling, &[mount], Some(&profile))
                .is_err()
        );
        for source in [
            first.target(),
            first.target().parent().unwrap().to_path_buf(),
        ] {
            fs::write(
                &profile,
                serde_json::to_vec(&serde_json::json!({
                    "profile": {"mounts": [{"source": source, "dest": "/foreign"}]}
                }))
                .unwrap(),
            )
            .unwrap();
            assert!(
                association
                    .validate_worker(&worker, &[], Some(&profile))
                    .is_err()
            );
        }
        for target in [
            association.operator().join("target"),
            association.operator().join(".loom/trusted-cargo"),
            trusted_root().unwrap(),
            association.integration().join("target"),
            std::env::current_exe().unwrap(),
        ] {
            assert!(
                association
                    .admit_mount(&MountSpec {
                        host_path: target,
                        container_path: "/foreign".into(),
                        read_only: false
                    })
                    .is_err()
            );
        }
    }

    #[test]
    fn worker_cargo_projection_serializes_admitted_output() {
        let (_root, association, worker, profile) = fixture();
        let namespace = prepare(
            &association,
            &worker,
            &profile,
            CargoTargetReuse::Off,
            retention::Policy::default().default_request(),
            &crate::clock::MockClock::new(),
        )
        .unwrap();
        let mut spawn: SpawnConfig = serde_json::from_value(serde_json::json!({
            "image_ref": "fixture",
            "workspace": worker,
            "env": [["CARGO_TARGET_DIR", "/unadmitted-target"]],
            "initial_prompt": "fixture",
            "agent_args": [],
            "repin": {"orientation": "fixture", "pinned_context": "fixture", "partial_bodies": []}
        }))
        .unwrap();
        spawn.profile_config = Some(profile.clone());
        namespace.configure(&mut spawn).unwrap();
        let wire = spawn.to_wrix_json().unwrap();
        assert_eq!(
            wire["mounts"],
            serde_json::json!([{
                "host_path": namespace.target(),
                "container_path": WORKER_TARGET,
                "read_only": false
            }])
        );
        let decoded = SpawnConfig::from_wrix_slice(&serde_json::to_vec(&wire).unwrap()).unwrap();
        association
            .validate_worker(&worker, &decoded.mounts, Some(&profile))
            .unwrap();
        for key in [
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET_DIR",
            "CARGO_BUILD_BUILD_DIR",
        ] {
            let values: Vec<_> = decoded.env.iter().filter(|(name, _)| name == key).collect();
            assert_eq!(values.len(), 1);
            assert_eq!(values[0].1, WORKER_TARGET);
        }
        assert!(!wire.to_string().contains("/unadmitted-target"));
    }

    #[test]
    fn cargo_storage_preserves_unknown_generations_and_refuses_symlinks() {
        use std::os::unix::fs::symlink;
        let (root, association, worker, profile) = fixture();
        let first = prepare(
            &association,
            &worker,
            &profile,
            CargoTargetReuse::Off,
            retention::Policy::default().default_request(),
            &crate::clock::MockClock::new(),
        )
        .unwrap();
        fs::write(first.target().join("retained"), "unknown owner").unwrap();
        let db = CacheDb::open(crate::state::path(association.operator())).unwrap();
        let key = db
            .records::<Record>(RecordKind::CargoTarget)
            .unwrap()
            .pop()
            .unwrap()
            .0;
        db.remove_record(RecordKind::CargoTarget, &key).unwrap();
        drop(first);
        let next = prepare(
            &association,
            &worker,
            &profile,
            CargoTargetReuse::Off,
            retention::Policy::default().default_request(),
            &crate::clock::MockClock::new(),
        )
        .unwrap();
        assert!(next.target().exists());
        fs::create_dir(worker.join(".cargo")).unwrap();
        symlink(
            root.path().join("outside"),
            worker.join(".cargo/config.toml"),
        )
        .unwrap();
        assert!(
            prepare(
                &association,
                &worker,
                &profile,
                CargoTargetReuse::Off,
                retention::Policy::default().default_request(),
                &crate::clock::MockClock::new()
            )
            .is_err()
        );
        let indirect = root.path().join("indirect-trusted-cargo");
        symlink(root.path(), &indirect).unwrap();
        assert!(
            trusted_command_at(&mut Command::new("true"), association.operator(), &indirect)
                .is_err()
        );
        assert!(
            trusted_command_at(
                &mut Command::new("true"),
                association.operator(),
                &worker.join("target")
            )
            .is_err()
        );
    }

    #[test]
    fn trusted_cargo_workspace_alias_cannot_hide_source_output_overlap() {
        let (root, association, _worker, _profile) = fixture();
        let alias = root.path().join("operator-alias");
        std::os::unix::fs::symlink(association.operator(), &alias).unwrap();
        let output = association.operator().join("hidden-trusted-output");
        assert!(trusted_command_at(&mut Command::new("true"), &alias, &output).is_err());
        assert!(!output.exists());
    }

    #[test]
    fn trusted_cargo_execution_rejects_worker_cache_inputs() {
        let (root, association, worker, profile) = fixture();
        let namespace = prepare(
            &association,
            &worker,
            &profile,
            CargoTargetReuse::Off,
            retention::Policy::default().default_request(),
            &crate::clock::MockClock::new(),
        )
        .unwrap();
        let output_root = root.path().join("trusted-cargo");
        let mut command = Command::new("env");
        command
            .env("SCCACHE_DIR", namespace.target())
            .env("SCCACHE_SERVER_UDS", namespace.target().join("worker.sock"))
            .env("SCCACHE_REDIS", "redis://untrusted")
            .env("RUSTC_WRAPPER", "/worker/wrapper")
            .env("CARGO_TARGET_DIR", namespace.target());
        let _execution =
            trusted_command_at(&mut command, association.operator(), &output_root).unwrap();
        let environment: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        for key in ["SCCACHE_DIR", "SCCACHE_SERVER_UDS", "SCCACHE_REDIS"] {
            assert_eq!(environment.get(std::ffi::OsStr::new(key)), Some(&None));
        }
        for key in [
            "RUSTC_WRAPPER",
            "CARGO_BUILD_RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
        ] {
            assert_eq!(
                environment.get(std::ffi::OsStr::new(key)),
                Some(&Some(std::ffi::OsStr::new("")))
            );
        }
        let target = Path::new(environment[std::ffi::OsStr::new("CARGO_TARGET_DIR")].unwrap());
        assert!(target.starts_with(output_root));
        assert!(!target.starts_with(association.operator()));
        assert!(!target.starts_with(namespace.target()));
    }

    #[tokio::test]
    async fn trusted_native_cargo_context_isolates_driver_generations() -> anyhow::Result<()> {
        use crate::clock::SystemClock;
        use std::time::Duration;
        use tokio::process::Command as AsyncCommand;

        if let Some(workspace) = std::env::var_os("LOOM_CARGO_CONTEXT_CHILD") {
            println!("context={}", context(Path::new(&workspace), None)?.name());
            return Ok(());
        }
        let root = tempfile::tempdir()?;
        let workspace = root.path().join("source");
        fs::create_dir(&workspace)?;
        let parent = context(&workspace, None)?.name();
        let executable = root.path().join("different-driver-generation");
        fs::copy(std::env::current_exe()?, &executable)?;
        let output = crate::process::output(
            AsyncCommand::new(executable)
                .args([
                    "trusted_native_cargo_context_isolates_driver_generations",
                    "--nocapture",
                ])
                .env("LOOM_CARGO_CONTEXT_CHILD", &workspace),
            &SystemClock,
            Duration::from_secs(60),
        )
        .await?;
        anyhow::ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout)?;
        let child = stdout
            .lines()
            .find_map(|line| line.strip_prefix("context="))
            .ok_or_else(|| anyhow::anyhow!("copied native driver did not report its context"))?;
        assert_ne!(parent, child);
        Ok(())
    }

    #[tokio::test]
    async fn trusted_and_worker_cargo_builds_cannot_overwrite_canonical_outputs()
    -> anyhow::Result<()> {
        use crate::clock::SystemClock;
        use std::time::Duration;
        use tokio::process::Command as AsyncCommand;

        let root = tempfile::tempdir()?;
        let operator = root.path().join("operator");
        crate::git::init_test_repo_with_integration(&operator)?;
        let integration = operator.join(".loom/integration");
        let worker = operator.join(".loom/beads/lm-worker");
        crate::git::init_test_repo(&worker)?;
        for source in [&operator, &integration, &worker] {
            fs::create_dir_all(source.join("src"))?;
            fs::write(
                source.join("Cargo.toml"),
                "[workspace]\n[package]\nname='canonical-fixture'\nversion='0.1.0'\nedition='2024'\n",
            )?;
            fs::write(
                source.join("src/main.rs"),
                "fn main(){println!(\"trusted\");}\n",
            )?;
        }
        fs::write(
            worker.join("src/main.rs"),
            "fn main(){println!(\"worker\");}\n",
        )?;
        let profile = root.path().join("profile.json");
        fs::write(&profile, r#"{"profile":{"mounts":[]}}"#)?;
        let association = Association::resolve(&operator)?;
        let namespace = prepare(
            &association,
            &worker,
            &profile,
            CargoTargetReuse::Off,
            retention::Policy::default().default_request(),
            &crate::clock::MockClock::new(),
        )?;
        let canonical = operator.join("target/debug/loom");
        fs::create_dir_all(canonical.parent().unwrap())?;
        fs::write(&canonical, "canonical executable sentinel")?;
        for (source, owned) in [(&worker, Some(namespace.target())), (&operator, None)] {
            let mut command = Command::new("cargo");
            command
                .current_dir(source)
                .args(["build", "--offline"])
                .env("CARGO_HOME", root.path().join("cargo-home"))
                .env("CARGO_BUILD_JOBS", "1")
                .env("CARGO_TARGET_DIR", namespace.target());
            let _execution =
                trusted_command_at(&mut command, source, &root.path().join("trusted-cargo"))?;
            if let Some(target) = owned {
                for key in [
                    "CARGO_TARGET_DIR",
                    "CARGO_BUILD_TARGET_DIR",
                    "CARGO_BUILD_BUILD_DIR",
                ] {
                    command.env(key, &target);
                }
            }
            let target = command
                .get_envs()
                .find(|(key, _)| *key == "CARGO_TARGET_DIR")
                .unwrap()
                .1
                .unwrap()
                .to_os_string();
            let output = crate::process::output(
                &mut AsyncCommand::from(command),
                &SystemClock,
                Duration::from_secs(60),
            )
            .await?;
            anyhow::ensure!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let output = crate::process::output(
                &mut AsyncCommand::new(Path::new(&target).join("debug/canonical-fixture")),
                &SystemClock,
                Duration::from_secs(10),
            )
            .await?;
            assert_eq!(
                String::from_utf8(output.stdout)?.trim(),
                if source == &worker {
                    "worker"
                } else {
                    "trusted"
                }
            );
        }
        assert_eq!(
            fs::read_to_string(canonical)?,
            "canonical executable sentinel"
        );
        Ok(())
    }
}

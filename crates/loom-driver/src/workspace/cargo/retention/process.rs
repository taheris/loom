#[cfg(test)]
mod cases {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::Duration;

    use super::super::tests::cases::{allocate, policy, spawn};
    use super::super::*;
    use crate::clock::{MockClock, SystemClock};
    use crate::workspace::cargo::tests::cases::fixture;

    async fn wait_for(path: &Path) {
        let clock = SystemClock::new();
        clock
            .timeout(Duration::from_secs(120), async {
                while !path.try_exists().unwrap() {
                    clock.sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap_or_else(|error| panic!("{} did not appear: {error}", path.display()));
    }

    struct Release(PathBuf);

    impl Drop for Release {
        fn drop(&mut self) {
            if let Err(error) = fs::write(&self.0, "release") {
                tracing::warn!(?error, path = ?self.0, "could not release Cargo fixture build script");
            }
        }
    }

    #[test]
    fn cargo_retention_launcher_child() {
        let Some(operator) = std::env::var_os("LOOM_RETENTION_TEST_LAUNCHER") else {
            return;
        };
        let signals = PathBuf::from(std::env::var_os("LOOM_RETENTION_TEST_SIGNALS").unwrap());
        let association = Association::resolve(Path::new(&operator)).unwrap();
        let worker = association.operator().join(".loom/beads/lm-cargo");
        let profile = PathBuf::from(std::env::var_os("LOOM_RETENTION_TEST_PROFILE").unwrap());
        let managed = std::env::var_os("LOOM_RETENTION_TEST_MANAGED").is_some();
        let (initial_target, _namespace) =
            if let Some(output) = std::env::var_os("LOOM_RETENTION_TEST_NATIVE") {
                (
                    crate::workspace::cargo::trusted::prepare(
                        Path::new(&output),
                        &worker,
                        &MockClock::new(),
                    )
                    .unwrap(),
                    None,
                )
            } else {
                let namespace = allocate(&association, &worker, &profile, &policy());
                namespace.configure(&mut spawn(&worker)).unwrap();
                (namespace.target(), Some(namespace))
            };
        let mut command = Command::new("sh");
        command
            .args(["-c", "cargo build --offline; code=$?; printf '%s\\n' \"$code\" > \"$LOOM_RETENTION_TEST_SIGNALS/cargo-done\"; exit \"$code\""])
            .current_dir(&worker)
            .env("CARGO_TARGET_DIR", &initial_target)
            .env("CARGO_BUILD_TARGET_DIR", &initial_target)
            .env("CARGO_BUILD_BUILD_DIR", &initial_target)
            .env("CARGO_BUILD_JOBS", "1")
            .env("RUSTC_WRAPPER", "")
            .env("CARGO_BUILD_RUSTC_WRAPPER", "")
            .env("RUSTC_WORKSPACE_WRAPPER", "")
            .env("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER", "")
            .env("SCCACHE_DISABLE", "1");
        let _execution = if managed {
            Some(
                crate::workspace::cargo::trusted_command_at(
                    &mut command,
                    &worker,
                    Path::new(&std::env::var_os("LOOM_RETENTION_TEST_NATIVE").unwrap()),
                )
                .unwrap(),
            )
        } else {
            None
        };
        let target = command
            .get_envs()
            .find(|(name, _)| *name == "CARGO_TARGET_DIR")
            .unwrap()
            .1
            .unwrap();
        fs::write(signals.join("target"), target.to_str().unwrap()).unwrap();
        assert!(command.status().unwrap().success());
    }

    #[tokio::test]
    async fn cargo_retention_restart_preserves_surviving_processes() {
        surviving_processes(false, false).await;
    }

    #[tokio::test]
    async fn trusted_cargo_retention_restart_preserves_surviving_processes() {
        surviving_processes(true, false).await;
    }

    #[tokio::test]
    #[ignore = "requires actual Linux delegated cgroup2 and pinned native entry helpers"]
    async fn native_cargo_retention_restart_reconciles_owned_process_generations() {
        if std::env::var_os("LOOM_RETENTION_TEST_MANAGED_PARENT").is_none() {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", "workspace::cargo::retention::process::cases::native_cargo_retention_restart_reconciles_owned_process_generations", "--ignored", "--nocapture"])
                .env("LOOM_RETENTION_TEST_MANAGED_PARENT", "1");
            for (key, _) in
                std::env::vars_os().filter(|(key, _)| key.as_encoded_bytes().starts_with(b"LD_"))
            {
                command.env_remove(key);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        assert!(
            crate::workspace::cargo::lifetime::native_root()
                .unwrap()
                .is_some()
        );
        surviving_processes(true, true).await;
    }

    async fn surviving_processes(native: bool, managed: bool) {
        use rustix::process::{Pid, test_kill_process};
        let (mut root, association, worker, profile) = fixture();
        if managed {
            root.disable_cleanup(true);
            eprintln!(
                "retained native lifecycle fixture: {}",
                root.path().display()
            );
        }
        let native_output = root.path().join("trusted-cargo");
        if native {
            let fence = Fence::trusted(&native_output).unwrap();
            let file = fence
                .root
                .create_file(
                    Path::new("policy.json"),
                    rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
                )
                .unwrap();
            serde_json::to_writer(file, &policy()).unwrap();
        }
        let signals = root.path().join("signals");
        fs::create_dir(&signals).unwrap();
        let release = Release(signals.join("release"));
        fs::create_dir(worker.join("src")).unwrap();
        fs::write(
            worker.join("src/main.rs"),
            "fn main() { println!(\"preserved\"); }\n",
        )
        .unwrap();
        fs::write(
            worker.join("build.rs"),
            r#"
use std::{env, fs, path::PathBuf, thread, time::Duration};
fn main() {
    let signals = PathBuf::from(env::var_os("LOOM_RETENTION_TEST_SIGNALS").unwrap());
    let output = PathBuf::from(env::var_os("CARGO_TARGET_DIR").unwrap()).join("live-build-output");
    let _open = fs::File::create(output).unwrap();
    fs::write(signals.join("ready"), std::process::id().to_string()).unwrap();
    while !signals.join("release").exists() { thread::sleep(Duration::from_millis(10)); }
}
"#,
        )
        .unwrap();
        let log = fs::File::create(root.path().join("launcher.log")).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        if native {
            command.env("LOOM_RETENTION_TEST_NATIVE", &native_output);
        }
        if managed {
            command.env("LOOM_RETENTION_TEST_MANAGED", "1");
        }
        let mut launcher = command
            .args([
                "--exact",
                "workspace::cargo::retention::process::cases::cargo_retention_launcher_child",
                "--nocapture",
            ])
            .env("LOOM_RETENTION_TEST_LAUNCHER", association.operator())
            .env("LOOM_RETENTION_TEST_SIGNALS", &signals)
            .env("LOOM_RETENTION_TEST_PROFILE", &profile)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        wait_for(&signals.join("ready")).await;
        let pid: i32 = fs::read_to_string(signals.join("ready"))
            .unwrap()
            .parse()
            .unwrap();
        let target = PathBuf::from(fs::read_to_string(signals.join("target")).unwrap());
        launcher.kill().unwrap();
        assert!(!launcher.wait().unwrap().success());
        assert!(test_kill_process(Pid::from_raw(pid).unwrap()).is_ok());
        let lease = Directory::open(target.parent().unwrap())
            .unwrap()
            .read_private_file(Path::new("lease"))
            .unwrap();
        if managed {
            assert!(matches!(
                lease.try_lock(),
                Err(std::fs::TryLockError::WouldBlock)
            ));
        } else {
            lease.try_lock().unwrap();
        }
        drop(lease);
        let restarted = Association::resolve(association.operator()).unwrap();
        let pressure = Policy {
            max_bytes: Bytes::default(),
            max_age_secs: 0,
            ..policy()
        };
        let sweep_pool = || {
            if native {
                Fence::trusted(&native_output)
                    .unwrap()
                    .collect(&pressure, Bytes::default(), &MockClock::new())
                    .unwrap()
            } else {
                sweep(&restarted, &pressure, &MockClock::new()).unwrap()
            }
        };
        let report = sweep_pool();
        assert!(report.charged.get() > 0);
        assert!(target.join("live-build-output").exists());
        assert!(test_kill_process(Pid::from_raw(pid).unwrap()).is_ok());
        let mut independent_policy = policy();
        independent_policy.max_bytes = report.charged.add(Bytes::new(256 * 1024)).unwrap();
        if native {
            let source = root.path().join("independent-source");
            fs::create_dir(&source).unwrap();
            independent_policy.max_bytes = independent_policy
                .max_bytes
                .add(Bytes::new(1024 * 1024))
                .unwrap();
            {
                let fence = Fence::trusted(&native_output).unwrap();
                let file = fence
                    .root
                    .read_private_file(Path::new("policy.json"))
                    .unwrap();
                file.set_len(0).unwrap();
                serde_json::to_writer(file, &independent_policy).unwrap();
            }
            assert!(
                crate::workspace::cargo::trusted::prepare(
                    &native_output,
                    &source,
                    &MockClock::new()
                )
                .unwrap()
                .is_dir()
            );
        } else {
            let independent = allocate(&restarted, &worker, &profile, &independent_policy);
            assert!(independent.target().is_dir());
        }
        fs::write(&release.0, "release").unwrap();
        wait_for(&signals.join("cargo-done")).await;
        assert_eq!(
            fs::read_to_string(signals.join("cargo-done"))
                .unwrap()
                .trim(),
            "0"
        );
        if managed {
            SystemClock::new()
                .timeout(Duration::from_secs(120), async {
                    while target.exists() {
                        sweep_pool();
                        SystemClock::new().sleep(Duration::from_millis(20)).await;
                    }
                })
                .await
                .unwrap();
            assert!(!target.exists());
            assert!(worker.join("Cargo.toml").exists());
        } else {
            sweep_pool();
            assert!(
                target.join("live-build-output").exists(),
                "completion without runtime reconciliation is still unknown"
            );
            assert!(target.join("debug/fixture").is_file());
        }
    }
}

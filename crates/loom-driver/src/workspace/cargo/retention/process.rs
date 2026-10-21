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
        let namespace = allocate(&association, &worker, &profile, &policy());
        namespace.configure(&mut spawn(&worker)).unwrap();
        fs::write(signals.join("target"), namespace.target().to_str().unwrap()).unwrap();
        let status = Command::new("sh")
            .args(["-c", "cargo build --offline; code=$?; printf '%s\\n' \"$code\" > \"$LOOM_RETENTION_TEST_SIGNALS/cargo-done\"; exit \"$code\""])
            .current_dir(&worker)
            .env("CARGO_TARGET_DIR", namespace.target())
            .env("CARGO_BUILD_TARGET_DIR", namespace.target())
            .env("CARGO_BUILD_BUILD_DIR", namespace.target())
            .env("CARGO_BUILD_JOBS", "1")
            .env("RUSTC_WRAPPER", "")
            .env("CARGO_BUILD_RUSTC_WRAPPER", "")
            .env("RUSTC_WORKSPACE_WRAPPER", "")
            .env("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER", "")
            .env("SCCACHE_DISABLE", "1")
            .status().unwrap();
        assert!(status.success());
    }

    #[tokio::test]
    async fn cargo_retention_restart_preserves_surviving_processes() {
        use rustix::process::{Pid, test_kill_process};
        let (root, association, worker, profile) = fixture();
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
        let mut launcher = Command::new(std::env::current_exe().unwrap())
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
        lease.try_lock().unwrap();
        drop(lease);
        let restarted = Association::resolve(association.operator()).unwrap();
        let pressure = Policy {
            max_bytes: Bytes::default(),
            max_age_secs: 0,
            ..policy()
        };
        let report = sweep(&restarted, &pressure, &MockClock::new()).unwrap();
        assert!(report.charged.get() > 0);
        assert!(target.join("live-build-output").exists());
        assert!(test_kill_process(Pid::from_raw(pid).unwrap()).is_ok());
        let mut independent_policy = policy();
        independent_policy.max_bytes = report.charged.add(Bytes::new(256 * 1024)).unwrap();
        let independent = allocate(&restarted, &worker, &profile, &independent_policy);
        assert!(independent.target().is_dir());
        fs::write(&release.0, "release").unwrap();
        wait_for(&signals.join("cargo-done")).await;
        assert_eq!(
            fs::read_to_string(signals.join("cargo-done"))
                .unwrap()
                .trim(),
            "0"
        );
        sweep(&restarted, &pressure, &MockClock::new()).unwrap();
        assert!(
            target.join("live-build-output").exists(),
            "completion without runtime reconciliation is still unknown"
        );
        assert!(target.join("debug/fixture").is_file());
    }
}

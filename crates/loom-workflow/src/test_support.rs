//! Shared test-only isolated native Beads fixture.

#[cfg(test)]
pub mod acceptance;

#[cfg(test)]
pub use native::{IsolatedBeads, client};

#[cfg(test)]
mod native {
    use loom_driver::bd::{BdClient, BdError, CommandRunner, RunOutput};
    use loom_driver::clock::SystemClock;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use tokio::process::Command;

    pub struct IsolatedBeads {
        root: PathBuf,
    }

    impl IsolatedBeads {
        pub fn attach(root: &Path) -> Self {
            Self {
                root: root.to_path_buf(),
            }
        }
    }

    impl CommandRunner for IsolatedBeads {
        async fn run(&self, args: Vec<OsString>, timeout: Duration) -> Result<RunOutput, BdError> {
            let mut command = Command::new("bd");
            command
                .args(["--sandbox", "--dolt-auto-commit", "off"])
                .args(args)
                .current_dir(&self.root)
                .env("HOME", self.root.join("home"))
                .env("GOMAXPROCS", "2");
            for (name, _) in std::env::vars_os() {
                if name.to_string_lossy().starts_with("BEADS_") {
                    command.env_remove(name);
                }
            }
            let output = loom_driver::process::output(&mut command, &SystemClock::new(), timeout)
                .await
                .expect("isolated Beads process completed");
            Ok(RunOutput {
                status: output.status.code().expect("Beads exit code"),
                stdout: output.stdout,
                stderr: output.stderr,
            })
        }
    }

    pub async fn client(root: &Path) -> BdClient<IsolatedBeads> {
        std::fs::create_dir(root.join("home")).unwrap();
        loom_driver::git::initialize_snapshot(root, &SystemClock::new())
            .await
            .unwrap();
        let runner = IsolatedBeads {
            root: root.to_path_buf(),
        };
        let init = runner
            .run(
                [
                    "init",
                    "--prefix",
                    "mint",
                    "--skip-agents",
                    "--skip-hooks",
                    "--non-interactive",
                ]
                .into_iter()
                .map(OsString::from)
                .collect(),
                Duration::from_secs(60),
            )
            .await
            .unwrap();
        assert!(init.success(), "{}", String::from_utf8_lossy(&init.stderr));
        BdClient::with_runner(runner)
    }
}

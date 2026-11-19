//! Canonical-store integration executes locally, never as hermetic test evidence.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use loom_driver::clock::{Clock, SystemClock};
use loom_driver::process::OwnedChild;

use super::{Error, Graph, Name, Observation, Observed, Outcome, report};

#[derive(serde::Deserialize)]
struct Store {
    url: String,
}

impl Graph {
    pub(super) fn run_store(
        &self,
        target: Name,
        contract: &report::Contract,
    ) -> Result<Observed, Error> {
        let store: Store =
            serde_json::from_slice(&super::query(&self.tool, &["store", "info", "--json"])?)?;
        if !matches!(store.url.as_str(), "daemon" | "local") {
            return Ok(Observed::execution(
                target,
                Outcome::Unavailable {
                    capability: super::Capability::CanonicalNixStore,
                    detail: "native-store requires the local canonical Nix store, not a diverted or remote store".into(),
                },
                vec![Observation::ExecutionGraph { digest: self.identity.clone() }],
            ));
        }
        let reporter = self.reporter.as_ref().ok_or(Error::Result)?;
        let node = self.nodes.get(reporter).ok_or(Error::Root)?;
        let _inputs = self.prepare_reporter(reporter, node)?;
        let directory = tempfile::Builder::new()
            .prefix("ls-")
            .tempdir_in(std::fs::canonicalize("/tmp")?)?;
        let unit = Name::try_from(
            directory
                .path()
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or(Error::Result)?
                .to_owned(),
        )
        .map_err(|_| Error::Result)?;
        let output = directory.path().join("report");
        let work = directory.path().join("w");
        std::fs::create_dir(&output)?;
        std::fs::create_dir(&work)?;
        let mut command = tokio::process::Command::new(&node.builder);
        command
            .args(&node.args)
            .env_clear()
            .envs(&node.env)
            .env("out", &output)
            .env("HOME", work.join("home"))
            .env("TMPDIR", &work)
            .env("TMP", &work)
            .env("TEMP", &work)
            .env("NIX_BUILD_TOP", &work)
            .env("NIX_BUILD_CORES", "4")
            .env("NIX_STORE", "/nix/store")
            .env("NIX_REMOTE", &store.url)
            .current_dir(&work)
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(
                directory.path().join("launcher-stdout"),
            )?)
            .stderr(std::fs::File::create(
                directory.path().join("launcher-stderr"),
            )?);
        for key in [
            "NIX_CONFIG",
            "NIX_CONF_DIR",
            "NIX_USER_CONF_FILES",
            "NIX_STATE_DIR",
            "NIX_LOG_DIR",
            "NIX_STORE_DIR",
        ] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let mut configuration = std::env::var_os("NIX_CONFIG").unwrap_or_default();
        configuration.push("\nextra-experimental-features = nix-command flakes\nsandbox = true\nsandbox-fallback = false\nsandbox-paths =\nextra-sandbox-paths =\nrequire-sigs = true\nmax-jobs = 1\ncores = 4\nbuilders =\nsubstitute = false\n");
        command.env("NIX_CONFIG", configuration);
        std::fs::create_dir(work.join("home"))?;
        self.require_current()?;
        let executed = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(run(&mut command, &work, &SystemClock::new()))
                })
                .join()
                .map_err(|_| std::io::Error::other("native-store executor panicked"))?
        });
        let retained = directory.keep();
        self.require_current()?;
        let status = executed.map_err(|error| Error::ReportInfrastructure {
            code: 78,
            detail: format!(
                "{error}; native-store diagnostics retained at {}",
                retained.display()
            ),
        })?;
        if !status.success() {
            return Err(Error::ReportInfrastructure {
                code: status.code().unwrap_or(78),
                detail: format!(
                    "native-store producer failed; diagnostics retained at {}",
                    retained.display()
                ),
            });
        }
        let report = report::Report::read_directory(&output, contract)?;
        self.require_current()?;
        Ok(Observed::reported(
            target,
            report,
            vec![
                Observation::ExecutionGraph {
                    digest: self.identity.clone(),
                },
                Observation::NativeExecution { unit },
                Observation::NativeReport { directory: output },
            ],
        ))
    }
}

fn require_space(path: &Path) -> std::io::Result<()> {
    let space = rustix::fs::statvfs(path)?;
    if u128::from(space.f_bavail) * u128::from(space.f_frsize) < 12 * 1024_u128.pow(3) {
        return Err(std::io::Error::other(
            "native-store requires 12 GiB free space",
        ));
    }
    Ok(())
}

async fn run(
    command: &mut tokio::process::Command,
    work: &Path,
    clock: &dyn Clock,
) -> std::io::Result<std::process::ExitStatus> {
    require_space(work)?;
    require_space(Path::new("/nix/store"))?;
    let deadline = clock.now() + Duration::from_mins(30);
    let mut child = OwnedChild::spawn(command)?;
    loop {
        if let Some(status) = child.wait_bounded(clock, Duration::from_secs(1)).await? {
            return Ok(status);
        }
        let space = require_space(work).and_then(|()| require_space(Path::new("/nix/store")));
        if clock.now() >= deadline || space.is_err() {
            if let Some(id) = child.id() {
                let pid = rustix::process::Pid::from_raw(
                    i32::try_from(id).map_err(std::io::Error::other)?,
                )
                .ok_or_else(|| std::io::Error::other("invalid native-store process ID"))?;
                match rustix::process::kill_process_group(pid, rustix::process::Signal::TERM) {
                    Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                    Err(error) => return Err(error.into()),
                }
                clock.sleep(Duration::from_secs(15)).await;
            }
            child.terminate().await?;
            space?;
            return Err(std::io::Error::other(
                "native-store execution exceeded 1800 seconds",
            ));
        }
    }
}

#![cfg(test)]

use std::ffi::OsString;
use std::io::Read as _;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as NativeCommand, Stdio};
use std::sync::Arc;
use std::time::Duration;

use loom_driver::bd::{BdClient, BdError, CommandRunner, RunOutput};
use loom_driver::clock::{Clock, SystemClock};
use tokio::process::Command;

struct Server {
    child: Child,
    directory: tempfile::TempDir,
}

impl Server {
    async fn start(home: &Path) -> Self {
        let directory = tempfile::Builder::new()
            .prefix("loom-bd-")
            .tempdir()
            .unwrap();
        let data = directory.path().join("data");
        std::fs::create_dir(&data).unwrap();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let socket = directory.path().join("server.sock");
        let log = std::fs::File::create(directory.path().join("server.log")).unwrap();
        let mut command = NativeCommand::new("dolt");
        isolate(&mut command, home);
        command
            .args([
                "sql-server",
                "--loglevel",
                "warning",
                "--host",
                "127.0.0.1",
                "--port",
            ])
            .arg(port.to_string())
            .arg("--socket")
            .arg(&socket)
            .arg("--data-dir")
            .arg(&data)
            .current_dir(directory.path())
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        // Dolt rejects port zero; startup must fail visibly if another listener wins this port.
        drop(listener);
        let mut server = Self {
            child: command.spawn().expect("start fixture Dolt"),
            directory,
        };
        let clock = SystemClock::new();
        let deadline = clock.now() + Duration::from_secs(15);
        while let Err(error) = server.ready() {
            assert!(
                server.child.try_wait().unwrap().is_none() && clock.now() < deadline,
                "fixture Dolt did not become ready ({error}): {}",
                std::fs::read_to_string(server.directory.path().join("server.log")).unwrap()
            );
            clock.sleep(Duration::from_millis(10)).await;
        }
        server
    }

    fn ready(&self) -> std::io::Result<()> {
        // Socket creation precedes the server accepting and greeting clients.
        let mut stream = std::os::unix::net::UnixStream::connect(self.socket())?;
        stream.set_read_timeout(Some(Duration::from_millis(100)))?;
        stream.read_exact(&mut [0])
    }

    fn socket(&self) -> PathBuf {
        self.directory.path().join("server.sock")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.directory.disable_cleanup(true);
            eprintln!(
                "failed native Beads fixture retained: {}",
                self.directory.path().display()
            );
        }
        if self
            .child
            .try_wait()
            .expect("inspect owned fixture Dolt")
            .is_none()
        {
            self.child.kill().expect("stop owned fixture Dolt");
        }
        self.child.wait().expect("reap owned fixture Dolt");
    }
}

fn isolate(command: &mut NativeCommand, home: &Path) {
    for (name, _) in std::env::vars_os() {
        let name_text = name.to_string_lossy();
        if ["BEADS_", "DOLT_", "GIT_"]
            .iter()
            .any(|prefix| name_text.starts_with(prefix))
        {
            command.env_remove(name);
        }
    }
    command
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GOMAXPROCS", "2")
        .env("BD_DISABLE_METRICS", "1");
}

#[derive(Clone)]
pub struct IsolatedBeads {
    root: PathBuf,
    server: Option<Arc<Server>>,
}

impl IsolatedBeads {
    pub fn attach(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            server: None,
        }
    }
}

impl CommandRunner for IsolatedBeads {
    async fn run(&self, args: Vec<OsString>, timeout: Duration) -> Result<RunOutput, BdError> {
        let binary = std::env::var_os("LOOM_ISSUES_BD_BIN").unwrap_or_else(|| "bd".into());
        let mut command = Command::new(binary);
        isolate(command.as_std_mut(), &self.root.join("home"));
        command
            .args(["--sandbox", "--dolt-auto-commit", "off"])
            .args(args)
            .current_dir(&self.root);
        if let Some(server) = &self.server {
            command.env("BEADS_DOLT_SERVER_SOCKET", server.socket());
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
    let home = root.join("home");
    std::fs::create_dir(&home).unwrap();
    loom_driver::git::initialize_snapshot(root, &SystemClock::new())
        .await
        .unwrap();
    let server = Arc::new(Server::start(&home).await);
    let mut args = ["init", "--server", "--external", "--server-socket"]
        .map(OsString::from)
        .to_vec();
    args.push(server.socket().into_os_string());
    args.extend(
        [
            "--prefix",
            "mint",
            "--skip-agents",
            "--skip-hooks",
            "--non-interactive",
        ]
        .map(OsString::from),
    );
    let runner = IsolatedBeads {
        root: root.to_path_buf(),
        server: Some(server),
    };
    let init = runner.run(args, Duration::from_secs(60)).await.unwrap();
    assert!(init.success(), "{}", String::from_utf8_lossy(&init.stderr));
    BdClient::with_runner(runner)
}

#[tokio::test]
async fn native_tracker_server_lives_until_last_owner_and_is_reaped() {
    let home = tempfile::tempdir().unwrap();
    let server = Arc::new(Server::start(home.path()).await);
    let directory = server.directory.path().to_path_buf();
    let socket = server.socket();
    let clone = server.clone();
    drop(server);
    assert!(std::os::unix::net::UnixStream::connect(&socket).is_ok());
    drop(clone);
    assert!(!directory.exists());
}

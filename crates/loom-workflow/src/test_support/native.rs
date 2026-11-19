#![cfg(test)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as NativeCommand, Stdio};
use std::sync::Arc;
use std::time::Duration;

use loom_driver::bd::{BdClient, BdError, CommandRunner, RunOutput};
use loom_driver::clock::{Clock, SystemClock};
use tokio::process::Command;

const DATABASE: &str = "mint";

struct Server {
    child: Child,
    directory: tempfile::TempDir,
    home: PathBuf,
}

impl Server {
    async fn start(home: &Path) -> Self {
        Self::start_on_port(home, 0).await
    }

    async fn start_on_port(home: &Path, port: u16) -> Self {
        let directory = tempfile::Builder::new()
            .prefix("loom-bd-")
            .tempdir()
            .unwrap();
        let data = directory.path().join("data");
        let database = data.join(DATABASE);
        std::fs::create_dir_all(&database).unwrap();
        let mut initialize = Command::new(env!("LOOM_TEST_DOLT_BIN"));
        isolate(initialize.as_std_mut(), home);
        initialize
            .args([
                "init",
                "--name",
                "Loom native fixture",
                "--email",
                "native@example.invalid",
            ])
            .current_dir(&database);
        let initialized = loom_driver::process::output(
            &mut initialize,
            &SystemClock::new(),
            Duration::from_secs(60),
        )
        .await
        .expect("initialize fixture database once before serving it");
        assert!(
            initialized.status.success(),
            "{}",
            String::from_utf8_lossy(&initialized.stderr)
        );
        let socket = directory.path().join("server.sock");
        let log = std::fs::File::create(directory.path().join("server.log")).unwrap();
        let mut command = NativeCommand::new(env!("LOOM_TEST_DOLT_BIN"));
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
        let mut server = Self {
            child: command.spawn().expect("start fixture Dolt"),
            directory,
            home: home.to_path_buf(),
        };
        let clock = SystemClock::new();
        let deadline = clock.now() + Duration::from_secs(60);
        while let Err(error) = server.ready().await {
            assert!(
                server.child.try_wait().unwrap().is_none() && clock.now() < deadline,
                "fixture Dolt did not become ready ({error}): {}",
                std::fs::read_to_string(server.directory.path().join("server.log")).unwrap()
            );
            clock.sleep(Duration::from_millis(10)).await;
        }
        server
    }

    async fn ready(&self) -> std::io::Result<()> {
        drop(std::os::unix::net::UnixStream::connect(self.socket())?);
        self.sql_ready_at(&self.socket()).await
    }

    async fn sql_ready_at(&self, socket: &Path) -> std::io::Result<()> {
        self.sql_ready_on(&format!("unix:{}", socket.display()), None)
            .await
    }

    async fn sql_ready_on(&self, host: &str, port: Option<u16>) -> std::io::Result<()> {
        // A greeting precedes usable SQL sessions; probe without retrying an uncertain mutation.
        let mut command = Command::new(env!("LOOM_TEST_DOLT_BIN"));
        isolate(command.as_std_mut(), &self.home);
        command.arg("--host").arg(host);
        if let Some(port) = port {
            command.arg("--port").arg(port.to_string());
        }
        command.args([
            "--user",
            "root",
            "--password",
            "",
            "--no-tls",
            "sql",
            "--query",
            "USE mint; SELECT 1",
        ]);
        let output =
            loom_driver::process::output(&mut command, &SystemClock::new(), Duration::from_secs(2))
                .await
                .map_err(std::io::Error::other)?;
        if output.status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ))
        }
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
        // Concurrent fixtures keep all sessions and assertions, but not unbounded Go heaps.
        .env(
            "GOMEMLIMIT",
            std::env::var("GOMEMLIMIT").unwrap_or_else(|_| "256MiB".into()),
        )
        .env(
            "GOMAXPROCS",
            std::env::var("GOMAXPROCS").unwrap_or_else(|_| "1".into()),
        )
        .env("BD_DISABLE_METRICS", "1");
}

#[test]
fn native_tracker_processes_keep_explicit_or_bounded_go_memory() {
    let mut command = NativeCommand::new("dolt");
    command.args(["sql-server", "--socket", "fixture.sock"]);
    isolate(&mut command, Path::new("fixture-home"));
    let expected = std::env::var("GOMEMLIMIT").unwrap_or_else(|_| "256MiB".into());
    let actual = command
        .get_envs()
        .find_map(|(name, value)| (name == "GOMEMLIMIT").then_some(value))
        .flatten();
    assert_eq!(actual, Some(std::ffi::OsStr::new(&expected)));
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        ["sql-server", "--socket", "fixture.sock"].map(std::ffi::OsStr::new)
    );
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
        let binary = env!("LOOM_ISSUES_BD_BIN");
        let mut command = Command::new(binary);
        isolate(command.as_std_mut(), &self.root.join("home"));
        // This one-shot adapter client exits after each call. Keep normal GC on
        // the long-lived SQL server; do not repeatedly collect short-lived CLI heaps.
        command.env(
            "GOGC",
            std::env::var("GOGC").unwrap_or_else(|_| "off".into()),
        );
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
            DATABASE,
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
async fn native_tracker_readiness_requires_the_beads_socket_even_with_healthy_sql_elsewhere() {
    let home = tempfile::tempdir().unwrap();
    let server = Server::start(home.path()).await;
    let socket = server.socket();
    let withheld = server.directory.path().join("withheld.sock");
    std::fs::rename(&socket, &withheld).unwrap();
    server.sql_ready_at(&withheld).await.unwrap();
    let error = server.ready().await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    std::fs::rename(&withheld, &socket).unwrap();
    server.ready().await.unwrap();
}

// Mandatory pre-push native TCP control, separate from confined UDS
// fixtures. No runtime permission failure is translated into a passing result.
#[cfg(feature = "native-tcp-tests")]
#[tokio::test]
async fn native_tracker_readiness_requires_the_beads_socket_even_with_healthy_tcp_sql() {
    let home = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let server = Server::start_on_port(home.path(), port).await;
    server.sql_ready_on("127.0.0.1", Some(port)).await.unwrap();
    let withheld = server.directory.path().join("withheld.sock");
    std::fs::rename(server.socket(), &withheld).unwrap();
    server.sql_ready_on("127.0.0.1", Some(port)).await.unwrap();
    assert_eq!(
        server.ready().await.unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    std::fs::rename(withheld, server.socket()).unwrap();
    server.ready().await.unwrap();
}

#[tokio::test]
async fn native_tracker_unix_only_mode_requires_an_explicit_socket() {
    let home = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("LOOM_TEST_DOLT_BIN"));
    isolate(command.as_std_mut(), home.path());
    command
        .args(["sql-server", "--port", "0"])
        .current_dir(home.path());
    let output =
        loom_driver::process::output(&mut command, &SystemClock::new(), Duration::from_secs(10))
            .await
            .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("port is not in the range"),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[tokio::test]
async fn native_tracker_sql_readiness_rejects_a_non_sql_socket() {
    let home = tempfile::tempdir().unwrap();
    let server = Server::start(home.path()).await;
    let socket = server.directory.path().join("not-sql.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let peer = async {
        let (stream, _) = listener.accept().await.unwrap();
        drop(stream);
    };
    let (result, ()) = SystemClock::new()
        .timeout(Duration::from_secs(10), async {
            tokio::join!(server.sql_ready_at(&socket), peer)
        })
        .await
        .unwrap();
    assert!(result.is_err());
    server.ready().await.unwrap();
}

#[tokio::test]
async fn native_tracker_server_lives_until_last_owner_and_is_reaped() {
    let home = tempfile::tempdir().unwrap();
    let server = Arc::new(Server::start(home.path()).await);
    let directory = server.directory.path().to_path_buf();
    let socket = server.socket();
    assert!(
        server
            .directory
            .path()
            .join("data")
            .join(DATABASE)
            .join(".dolt")
            .is_dir()
    );
    server.ready().await.unwrap();
    let clone = server.clone();
    drop(server);
    assert!(std::os::unix::net::UnixStream::connect(&socket).is_ok());
    drop(clone);
    assert!(!directory.exists());
}

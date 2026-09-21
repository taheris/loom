//! Owned, bounded subprocess capture for driver and workflow command boundaries.

use std::io;
use std::process::{Output, Stdio};
use std::time::Duration;

#[cfg(unix)]
use rustix::process::{Pid, Signal, kill_process_group};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

use crate::clock::Clock;

#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum Error {
    /// subprocess I/O failed
    Io(#[from] io::Error),
    /// subprocess timed out after termination and reaping
    Timeout,
    /// failed to terminate or reap subprocess
    Cleanup(#[source] io::Error),
}

/// Capture both pipes without deadlocking, terminating and reaping before returning a timeout.
///
/// Cancellation kills the Unix process group; Tokio reaps the dropped direct child in the background.
/// Descendants that deliberately leave the group are outside this ownership boundary.
/// Termination does not roll back effects already performed by the command.
///
/// # Errors
/// Returns an error on spawning, pipe capture, timeout, or process cleanup failure.
pub async fn output(
    command: &mut Command,
    clock: &dyn Clock,
    timeout: Duration,
) -> Result<Output, Error> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut process = OwnedChild::spawn(command)?;
    let result = tokio::select! {
        result = process.output() => result.map_err(Error::Io),
        () = clock.sleep(timeout) => Err(Error::Timeout),
    };
    match result {
        Ok(output) => {
            process.disarm();
            Ok(output)
        }
        Err(error) => {
            process.terminate().await.map_err(Error::Cleanup)?;
            Err(error)
        }
    }
}

/// A launcher and its Unix process group, killed together on cancellation.
///
/// The direct child is reaped asynchronously by Tokio when this guard is dropped.
/// Call `terminate` when cleanup must wait for reaping. Detached process groups
/// and container daemons remain the launcher's responsibility.
pub struct OwnedChild {
    child: Child,
    #[cfg(unix)]
    group: Option<Pid>,
}

impl OwnedChild {
    /// Spawn with the caller's I/O configuration and an owned process group.
    ///
    /// # Errors
    /// Returns a process-spawn or process-ID error.
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        command.kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let child = command.spawn()?;
        #[cfg(unix)]
        let group = {
            let raw = child
                .id()
                .ok_or_else(|| io::Error::other("spawned child has no process ID"))?;
            let raw = i32::try_from(raw).map_err(io::Error::other)?;
            Some(Pid::from_raw(raw).ok_or_else(|| io::Error::other("invalid child process ID"))?)
        };
        Ok(Self {
            child,
            #[cfg(unix)]
            group,
        })
    }

    /// Wait for the launcher to exit and release its process-group identity.
    ///
    /// # Errors
    /// Returns a process-wait error.
    pub async fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        let status = self.child.wait().await?;
        self.disarm();
        Ok(status)
    }

    /// Check for launcher exit without retaining a reaped process-group identity.
    ///
    /// # Errors
    /// Returns a process-wait error.
    pub fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.disarm();
        }
        Ok(status)
    }

    async fn output(&mut self) -> io::Result<Output> {
        let stdout = read_pipe(self.child.stdout.take());
        let stderr = read_pipe(self.child.stderr.take());
        // Keep the leader unreaped until both pipes close so its group ID cannot be reused.
        let (stdout, stderr) = tokio::try_join!(stdout, stderr)?;
        let status = self.child.wait().await?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }

    /// Kill the process group and reap the direct child.
    ///
    /// # Errors
    /// Returns a signal or process-wait error.
    pub async fn terminate(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        let group_result = self.kill_group();
        self.child.kill().await?;
        #[cfg(unix)]
        group_result?;
        self.disarm();
        Ok(())
    }

    const fn disarm(&mut self) {
        #[cfg(unix)]
        {
            self.group = None;
        }
    }

    #[cfg(unix)]
    fn kill_group(&self) -> io::Result<()> {
        if let Some(group) = self.group {
            match kill_process_group(group, Signal::KILL) {
                Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

impl std::ops::Deref for OwnedChild {
    type Target = Child;

    fn deref(&self) -> &Self::Target {
        &self.child
    }
}

impl std::ops::DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.child
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Err(error) = self.kill_group() {
            tracing::error!(?error, group = ?self.group, "failed to kill cancelled subprocess group");
        }
    }
}

async fn read_pipe(pipe: Option<impl AsyncRead + Unpin>) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe {
        pipe.read_to_end(&mut bytes).await?;
    }
    Ok(bytes)
}

#[cfg(test)]
#[cfg(unix)]
pub(crate) mod tests {
    //! Real processes are required to observe pipe closure, group termination and OS reaping.

    use std::future::Future;
    use std::io;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant, SystemTime};

    use rustix::process::{Pid, Signal, WaitOptions, kill_process, test_kill_process, waitpid};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::process::Command;
    use tokio::sync::Notify;

    use crate::clock::{BoxFuture, Clock, MockClock};

    use super::{Error, output};

    /// Manually expire only after the OS fixture has connected; no startup-time race or real sleep.
    #[derive(Default)]
    pub struct TriggerClock(Notify);

    impl TriggerClock {
        pub(crate) fn expire(&self) {
            self.0.notify_one();
        }
    }

    impl Clock for TriggerClock {
        fn now(&self) -> Instant {
            MockClock::new().now()
        }

        fn wall_now(&self) -> SystemTime {
            MockClock::new().wall_now()
        }

        fn sleep(&self, _duration: Duration) -> BoxFuture<'_, ()> {
            Box::pin(self.0.notified())
        }
    }

    pub async fn bounded<T>(future: impl Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(5), future)
            .await
            .expect("subprocess lifecycle fixture exceeded its cleanup deadline")
    }

    pub struct Fixture {
        directory: tempfile::TempDir,
        listener: TcpListener,
        pub(crate) clock: Arc<TriggerClock>,
    }

    impl Fixture {
        pub(crate) async fn new() -> Self {
            let directory = tempfile::tempdir().expect("fixture directory");
            std::fs::write(
                directory.path().join("hold.sh"),
                r#"set -euo pipefail
hold() {
    local port="$1" marker="$2"
    exec 3<>"/dev/tcp/127.0.0.1/${port}"
    printf '%s %s\n' "${BASHPID}" "$$" >&3
    if IFS= read -r token <&3; then
        printf 'late write\n' >> "$marker"
    fi
}
hold "$@" &
if [[ "${3-}" != orphan ]]; then
    hold "$@"
    wait
fi
"#,
            )
            .expect("fixture script");
            Self {
                directory,
                listener: TcpListener::bind("127.0.0.1:0").await.expect("listen"),
                clock: Arc::default(),
            }
        }

        pub(crate) fn command(&self) -> Command {
            let mut command = Command::new("bash");
            command.args(self.args());
            command
        }

        pub(crate) fn args(&self) -> [String; 3] {
            [
                self.directory.path().join("hold.sh").display().to_string(),
                self.listener
                    .local_addr()
                    .expect("listener address")
                    .port()
                    .to_string(),
                self.marker().display().to_string(),
            ]
        }

        fn marker(&self) -> PathBuf {
            self.directory.path().join("late-write")
        }

        pub(crate) async fn ready(&self, count: usize) -> Vec<Peer> {
            let mut peers = Vec::new();
            for _ in 0..count {
                let (stream, _) = self.listener.accept().await.expect("fixture connection");
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader
                    .read_line(&mut line)
                    .await
                    .expect("fixture handshake");
                let mut pids = line.split_whitespace().map(|pid| {
                    Pid::from_raw(pid.parse().expect("numeric pid")).expect("nonzero pid")
                });
                peers.push(Peer {
                    stream: reader.into_inner(),
                    pid: pids.next().expect("peer pid"),
                    leader: pids.next().expect("leader pid"),
                    live: true,
                });
            }
            peers
        }

        pub(crate) async fn assert_stopped(&self, peers: Vec<Peer>) {
            for peer in peers {
                peer.release().await;
            }
            assert!(
                !self.marker().exists(),
                "subprocess wrote after timeout/cancellation"
            );
        }
    }

    fn closed_connection(error: &io::Error) -> bool {
        matches!(
            error.kind(),
            io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
        )
    }

    pub struct Peer {
        stream: TcpStream,
        pid: Pid,
        leader: Pid,
        live: bool,
    }

    impl Peer {
        async fn release(mut self) {
            match self.stream.write_all(b"go\n").await {
                Ok(()) => {}
                Err(error) if closed_connection(&error) => {}
                Err(error) => panic!("release fixture: {error}"),
            }
            let mut bytes = Vec::new();
            match self.stream.read_to_end(&mut bytes).await {
                Ok(_) => {}
                Err(error) if closed_connection(&error) => {}
                Err(error) => panic!("observe fixture exit: {error}"),
            }
            self.live = false;
            assert!(bytes.is_empty(), "unexpected fixture output");
        }
    }

    impl Drop for Peer {
        fn drop(&mut self) {
            if self.live {
                match kill_process(self.pid, Signal::KILL) {
                    Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                    Err(error) => eprintln!("fixture cleanup failed for {:?}: {error}", self.pid),
                }
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn subprocess_timeout_uses_mock_clock() {
        let clock = MockClock::new();
        let start = clock.now();
        let mut command = Command::new("bash");
        command.args(["-c", "while :; do :; done"]);
        let timeout = Duration::from_hours(1);
        assert!(matches!(
            output(&mut command, &clock, timeout).await,
            Err(Error::Timeout)
        ));
        assert!(clock.now().duration_since(start) >= timeout);
    }

    #[tokio::test]
    async fn subprocess_timeout_kills_group_and_reaps_child() {
        bounded(async {
            let fixture = Fixture::new().await;
            let mut command = fixture.command();
            let (result, peers) = tokio::join!(
                output(
                    &mut command,
                    fixture.clock.as_ref(),
                    Duration::from_secs(60)
                ),
                async {
                    let peers = fixture.ready(2).await;
                    fixture.clock.expire();
                    peers
                },
            );
            assert!(matches!(result, Err(Error::Timeout)), "{result:?}");
            let leader = peers[0].leader;
            assert!(matches!(
                waitpid(Some(leader), WaitOptions::NOHANG),
                Err(rustix::io::Errno::CHILD)
            ));
            fixture.assert_stopped(peers).await;
        })
        .await;
    }

    #[tokio::test]
    async fn subprocess_timeout_kills_descendant_after_leader_exits() {
        bounded(async {
            let fixture = Fixture::new().await;
            let mut command = fixture.command();
            command.arg("orphan");
            let (result, peers) = tokio::join!(
                output(
                    &mut command,
                    fixture.clock.as_ref(),
                    Duration::from_secs(60)
                ),
                async {
                    let peers = fixture.ready(1).await;
                    fixture.clock.expire();
                    peers
                },
            );
            assert!(matches!(result, Err(Error::Timeout)), "{result:?}");
            fixture.assert_stopped(peers).await;
        })
        .await;
    }

    #[tokio::test]
    async fn subprocess_cancellation_kills_group_and_eventually_reaps_child() {
        bounded(async {
            let fixture = Fixture::new().await;
            let mut command = fixture.command();
            let mut operation = Box::pin(output(
                &mut command,
                fixture.clock.as_ref(),
                Duration::from_secs(60),
            ));
            let peers = tokio::select! {
                result = &mut operation => panic!("unexpected completion: {result:?}"),
                peers = fixture.ready(2) => peers,
            };
            drop(operation);
            let leader = peers[0].leader;
            fixture.assert_stopped(peers).await;
            loop {
                match test_kill_process(leader) {
                    Err(rustix::io::Errno::SRCH) => break,
                    Ok(()) => tokio::task::yield_now().await,
                    Err(error) => panic!("check child reaping: {error}"),
                }
            }
        })
        .await;
    }

    #[tokio::test]
    async fn subprocess_fixture_writes_only_after_release() {
        bounded(async {
            let fixture = Fixture::new().await;
            let mut command = fixture.command();
            let (result, ()) = tokio::join!(
                output(
                    &mut command,
                    fixture.clock.as_ref(),
                    Duration::from_secs(60)
                ),
                async {
                    let mut peers = fixture.ready(2).await;
                    assert!(!fixture.marker().exists());
                    let first = peers.pop().expect("first peer");
                    let second = peers.pop().expect("second peer");
                    tokio::join!(first.release(), second.release());
                },
            );
            assert!(result.expect("fixture output").status.success());
            assert_eq!(
                std::fs::read(fixture.marker()).expect("fixture writes"),
                b"late write\n".repeat(2)
            );
        })
        .await;
    }

    #[tokio::test]
    async fn subprocess_captures_large_pipes_and_nonzero_exit() {
        bounded(async {
            let mut command = Command::new("bash");
            command.args(["-c", "for ((i=0; i<10000; i++)); do printf 'stdout data\n'; printf 'stderr data\n' >&2; done; exit 23"]);
            let result = output(&mut command, &TriggerClock::default(), Duration::from_secs(60)).await.expect("captured output");
            assert_eq!(result.status.code(), Some(23));
            assert_eq!(result.stdout, b"stdout data\n".repeat(10000));
            assert_eq!(result.stderr, b"stderr data\n".repeat(10000));
        }).await;
    }

    #[tokio::test]
    async fn subprocess_spawn_failure_is_typed_io_error() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut command = Command::new(directory.path().join("missing-command"));
        let result = output(
            &mut command,
            &TriggerClock::default(),
            Duration::from_secs(60),
        )
        .await;
        assert!(matches!(result, Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound));
    }
}

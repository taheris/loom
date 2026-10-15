use std::collections::VecDeque;
use std::marker::PhantomData;

use crate::clock::{Clock, SystemClock};
use crate::process::OwnedChild;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::process::ChildStdin;

use super::error::ProtocolError;
use super::jsonl::JsonlReader;
use super::parse::LineParse;
use loom_events::ParsedAgentEvent;

/// Typestate marker — session has been spawned but no prompt has been sent.
pub struct Idle;

/// Typestate marker — session has been prompted and is streaming events.
pub struct Active;

/// Live agent session with a typestate-enforced protocol.
///
/// The state parameter `S` enforces protocol order at compile time: `prompt`
/// only exists on [`Idle`], while event reads and
/// in-flight commands only exist on [`Active`]. Backend implementations
/// construct the session with [`Self::new`] and hand it back through
/// `AgentBackend::spawn`.
pub struct AgentSession<S> {
    child: OwnedChild,
    clock: Arc<dyn Clock>,
    stdin: BufWriter<ChildStdin>,
    reader: JsonlReader,
    parser: Box<dyn LineParse + Send>,
    pending: VecDeque<ParsedAgentEvent>,
    open_turns: usize,
    completion_sent: bool,
    _state: PhantomData<S>,
}

fn process_exit_code(status: std::process::ExitStatus) -> Option<i32> {
    if let Some(code) = status.code() {
        return Some(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal().map(|signal| 128 + signal)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

impl AgentSession<Idle> {
    /// Build an idle session from the parts a backend's `spawn` already owns.
    /// Backends in `agent` call this immediately after launching the
    /// agent process and wiring its stdio.
    pub fn new(
        child: OwnedChild,
        stdin: BufWriter<ChildStdin>,
        reader: JsonlReader,
        parser: Box<dyn LineParse + Send>,
    ) -> Self {
        Self {
            child,
            clock: Arc::new(SystemClock::new()),
            stdin,
            reader,
            parser,
            pending: VecDeque::new(),
            open_turns: 0,
            completion_sent: false,
            _state: PhantomData,
        }
    }

    /// Use the backend's clock for startup-exit and stream-EOF deadlines.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Send the initial prompt and transition the session to [`Active`].
    /// The parser owns wire framing — this method only writes the encoded
    /// bytes and flushes.
    ///
    /// # Errors
    ///
    /// Returns an error when agent I/O, protocol parsing, or session validation fails.
    pub async fn prompt(mut self, msg: &str) -> Result<AgentSession<Active>, ProtocolError> {
        let line = self.parser.encode_prompt(msg)?;
        let write = async {
            self.stdin.write_all(line.as_bytes()).await?;
            self.stdin.flush().await
        }
        .await;
        if let Err(error) = write {
            if error.kind() == std::io::ErrorKind::BrokenPipe
                && let Some(status) = self
                    .child
                    .wait_bounded(self.clock.as_ref(), Duration::from_secs(5))
                    .await?
                && !status.success()
                && let Some(code) = process_exit_code(status)
            {
                return Err(ProtocolError::ProcessExit(code));
            }
            return Err(error.into());
        }
        Ok(AgentSession {
            child: self.child,
            clock: self.clock,
            stdin: self.stdin,
            reader: self.reader,
            parser: self.parser,
            pending: self.pending,
            open_turns: 1,
            completion_sent: false,
            _state: PhantomData,
        })
    }
}

impl AgentSession<Active> {
    /// Read the next [`ParsedAgentEvent`] from the stream.
    ///
    /// Drains buffered events from prior multi-event lines first. Otherwise
    /// reads JSONL lines until one yields at least one event, writing any
    /// `ParsedLine::response` payload back to stdin in between (the canonical
    /// case is claude's `control_request` auto-approve). Returns `Ok(None)`
    /// on clean EOF.
    ///
    /// Per RS-12 the session yields the unstamped parser payload; the
    /// workflow layer joins each event with the per-spawn `EventEnvelope`
    /// via `AgentEvent::from_parsed` before any consumer sees it.
    ///
    /// # Errors
    ///
    /// Returns an error when agent I/O, protocol parsing, or session validation fails.
    pub async fn next_event(&mut self) -> Result<Option<ParsedAgentEvent>, ProtocolError> {
        if let Some(evt) = self.pending.pop_front() {
            return Ok(Some(evt));
        }
        loop {
            let line_owned = if let Some(line) = self.reader.next_line().await? {
                line.to_owned()
            } else {
                if let Some(status) = self
                    .child
                    .wait_bounded(self.clock.as_ref(), Duration::from_secs(5))
                    .await?
                    && !status.success()
                {
                    if let Some(code) = process_exit_code(status) {
                        return Err(ProtocolError::ProcessExit(code));
                    }
                    return Err(ProtocolError::Io(std::io::Error::other(
                        "agent process exited without status code",
                    )));
                }
                return Ok(None);
            };
            let parsed = self.parser.parse_line(&line_owned)?;
            if let Some(response) = parsed.response {
                self.stdin.write_all(response.as_bytes()).await?;
                if !response.ends_with('\n') {
                    self.stdin.write_all(b"\n").await?;
                }
                self.stdin.flush().await?;
            }
            let mut iter = parsed.events.into_iter();
            if let Some(first) = iter.next() {
                self.pending.extend(iter);
                return Ok(Some(first));
            }
        }
    }

    /// Send a mid-session steering message. The parser encodes the wire
    /// payload (pi: JSONL `steer` command, claude: stream-json user message).
    ///
    /// # Errors
    ///
    /// Returns an error when agent I/O, protocol parsing, or session validation fails.
    pub async fn steer(&mut self, msg: &str) -> Result<(), ProtocolError> {
        let line = self.parser.encode_steer(msg)?;
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        self.open_turns = self.open_turns.saturating_add(1);
        Ok(())
    }

    /// Send a post-turn follow-up message while keeping the process alive.
    ///
    /// # Errors
    ///
    /// Returns an error when agent I/O, protocol parsing, or session validation fails.
    pub async fn follow_up(&mut self, msg: &str) -> Result<(), ProtocolError> {
        let line = self.parser.encode_follow_up(msg)?;
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        self.open_turns = self.open_turns.saturating_add(1);
        Ok(())
    }

    /// Close the steering window for one observed turn.
    ///
    /// A completion frame is written only after every prompt or steer sent so
    /// far has reached its own `TurnEnd`. Backends with native completion do
    /// not encode a frame, so this remains a bookkeeping-only operation for
    /// them.
    ///
    /// # Errors
    ///
    /// Returns an error when agent I/O, protocol parsing, or session validation fails.
    pub async fn finish_turn(&mut self) -> Result<(), ProtocolError> {
        self.open_turns = self.open_turns.saturating_sub(1);
        if self.open_turns != 0 || self.completion_sent {
            return Ok(());
        }
        if let Some(line) = self.parser.encode_complete()? {
            self.stdin.write_all(line.as_bytes()).await?;
            self.stdin.flush().await?;
            self.completion_sent = true;
        }
        Ok(())
    }

    /// Abort the in-flight operation and return the session to [`Idle`].
    ///
    /// If the parser provides an abort wire command (pi), it is written to
    /// stdin first; otherwise the caller is responsible for any process-level
    /// cleanup (claude is killed via signals — see the shutdown watchdog in
    /// the claude backend). The pending event queue is drained.
    ///
    /// # Errors
    ///
    /// Returns an error when agent I/O, protocol parsing, or session validation fails.
    pub async fn abort(mut self) -> Result<AgentSession<Idle>, ProtocolError> {
        if let Some(line) = self.parser.encode_abort()? {
            self.stdin.write_all(line.as_bytes()).await?;
            self.stdin.flush().await?;
        }
        Ok(AgentSession {
            child: self.child,
            clock: self.clock,
            stdin: self.stdin,
            reader: self.reader,
            parser: self.parser,
            pending: VecDeque::new(),
            open_turns: 0,
            completion_sent: false,
            _state: PhantomData,
        })
    }
}

impl<S> AgentSession<S> {
    /// Borrow the underlying child process — backends use this to wire up
    /// shutdown watchdogs without giving up ownership of the session.
    pub const fn child_mut(&mut self) -> &mut OwnedChild {
        &mut self.child
    }

    /// Decompose the session into the underlying child process and stdin
    /// writer. Used by the claude backend's shutdown watchdog after a
    /// `result` event: it must drop the writer (closing the pipe so claude
    /// observes EOF) then wait/signal the child.
    pub fn into_parts(self) -> (OwnedChild, BufWriter<ChildStdin>) {
        (self.child, self.stdin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ParsedLine;
    use crate::clock::{BoxFuture, MockClock};
    use std::process::Stdio;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Instant, SystemTime};
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::TcpListener;
    use tokio::process::Command;

    #[derive(Default)]
    struct TrackedMockClock {
        clock: MockClock,
        sleeps: AtomicUsize,
    }

    impl Clock for TrackedMockClock {
        fn now(&self) -> Instant {
            self.clock.now()
        }
        fn wall_now(&self) -> SystemTime {
            self.clock.wall_now()
        }
        fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()> {
            self.sleeps.fetch_add(1, Ordering::SeqCst);
            self.clock.sleep(duration)
        }
    }

    struct SilentParser;

    impl LineParse for SilentParser {
        fn parse_line(&self, _line: &str) -> Result<ParsedLine, ProtocolError> {
            Ok(ParsedLine {
                events: Vec::new(),
                response: None,
            })
        }
        fn encode_prompt(&self, msg: &str) -> Result<String, ProtocolError> {
            Ok(format!("{msg}\n"))
        }
        fn encode_steer(&self, msg: &str) -> Result<String, ProtocolError> {
            self.encode_prompt(msg)
        }
        fn encode_abort(&self) -> Result<Option<String>, ProtocolError> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn session_eof_deadline_uses_injected_mock_clock_when_process_has_no_status() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("private fixture listener");
        let mut command = Command::new("bash");
        command.args(["-c", "set -euo pipefail\nexec 3<>\"/dev/tcp/127.0.0.1/$1\"\nexec 1>&-\nprintf 'ready\\n' >&2\nIFS= read -r release <&3\n", "session-eof-fixture", &listener.local_addr().expect("address").port().to_string()]);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = OwnedChild::spawn(&mut command).expect("owned child");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let mut stderr = BufReader::new(child.stderr.take().expect("stderr"));
        let (mut peer, ready) = SystemClock::new()
            .timeout(Duration::from_secs(30), async {
                let (peer, _) = listener.accept().await.expect("child connected");
                let mut ready = String::new();
                stderr.read_line(&mut ready).await.expect("child readiness");
                (peer, ready)
            })
            .await
            .expect("OS fixture startup deadline");
        assert_eq!(ready, "ready\n");
        let clock = Arc::new(TrackedMockClock::default());
        let idle = AgentSession::new(
            child,
            BufWriter::new(stdin),
            JsonlReader::new(stdout),
            Box::new(SilentParser),
        )
        .with_clock(clock.clone());
        let mut active = idle.prompt("prompt").await.expect("open stdin");
        tokio::time::pause();
        let started = clock.now();
        assert!(active.next_event().await.expect("bounded EOF").is_none());
        assert!(clock.now().duration_since(started) >= Duration::from_secs(5));
        assert_eq!(clock.sleeps.load(Ordering::SeqCst), 1);
        assert!(
            active
                .child_mut()
                .try_wait()
                .expect("status query")
                .is_none()
        );
        tokio::time::resume();
        peer.write_all(b"release\n")
            .await
            .expect("owned fixture release");
        assert!(
            active
                .child_mut()
                .wait()
                .await
                .expect("owned child cleanup")
                .success()
        );
    }
}

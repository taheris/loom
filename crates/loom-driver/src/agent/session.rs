use std::collections::VecDeque;
use std::marker::PhantomData;

use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::process::{Child, ChildStdin};

use super::error::ProtocolError;
use super::jsonl::JsonlReader;
use super::parse::LineParse;
use loom_events::ParsedAgentEvent;

/// Typestate marker — session has been spawned but no prompt has been sent.
pub struct Idle;

/// Typestate marker — session has been prompted and is streaming events.
pub struct Active;

/// Live agent session. The state parameter `S` enforces protocol order at
/// compile time: `prompt` only exists on [`Idle`], `next_event`/`steer` only
/// on [`Active`]. Backend implementations construct the session with
/// [`Self::new`] and hand it back through `AgentBackend::spawn`.
pub struct AgentSession<S> {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    reader: JsonlReader,
    parser: Box<dyn LineParse + Send>,
    pending: VecDeque<ParsedAgentEvent>,
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
        child: Child,
        stdin: BufWriter<ChildStdin>,
        reader: JsonlReader,
        parser: Box<dyn LineParse + Send>,
    ) -> Self {
        Self {
            child,
            stdin,
            reader,
            parser,
            pending: VecDeque::new(),
            _state: PhantomData,
        }
    }

    /// Send the initial prompt and transition the session to [`Active`].
    /// The parser owns wire framing — this method only writes the encoded
    /// bytes and flushes.
    pub async fn prompt(mut self, msg: &str) -> Result<AgentSession<Active>, ProtocolError> {
        let line = self.parser.encode_prompt(msg)?;
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(AgentSession {
            child: self.child,
            stdin: self.stdin,
            reader: self.reader,
            parser: self.parser,
            pending: self.pending,
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
    pub async fn next_event(&mut self) -> Result<Option<ParsedAgentEvent>, ProtocolError> {
        if let Some(evt) = self.pending.pop_front() {
            return Ok(Some(evt));
        }
        loop {
            let line_owned = match self.reader.next_line().await? {
                Some(line) => line.to_owned(),
                None => {
                    if let Some(status) = self.child.try_wait()?
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
                }
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
    pub async fn steer(&mut self, msg: &str) -> Result<(), ProtocolError> {
        let line = self.parser.encode_steer(msg)?;
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    /// Send a post-turn follow-up message while keeping the process alive.
    pub async fn follow_up(&mut self, msg: &str) -> Result<(), ProtocolError> {
        let line = self.parser.encode_follow_up(msg)?;
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    /// Abort the in-flight operation and return the session to [`Idle`].
    ///
    /// If the parser provides an abort wire command (pi), it is written to
    /// stdin first; otherwise the caller is responsible for any process-level
    /// cleanup (claude is killed via signals — see the shutdown watchdog in
    /// the claude backend). The pending event queue is drained.
    pub async fn abort(mut self) -> Result<AgentSession<Idle>, ProtocolError> {
        if let Some(line) = self.parser.encode_abort()? {
            self.stdin.write_all(line.as_bytes()).await?;
            self.stdin.flush().await?;
        }
        Ok(AgentSession {
            child: self.child,
            stdin: self.stdin,
            reader: self.reader,
            parser: self.parser,
            pending: VecDeque::new(),
            _state: PhantomData,
        })
    }
}

impl<S> AgentSession<S> {
    /// Borrow the underlying child process — backends use this to wire up
    /// shutdown watchdogs without giving up ownership of the session.
    pub fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    /// Decompose the session into the underlying child process and stdin
    /// writer. Used by the claude backend's shutdown watchdog after a
    /// `result` event: it must drop the writer (closing the pipe so claude
    /// observes EOF) then wait/signal the child.
    pub fn into_parts(self) -> (Child, BufWriter<ChildStdin>) {
        (self.child, self.stdin)
    }
}

use super::error::ProtocolError;
use loom_events::ParsedAgentEvent;

/// Result of parsing a single JSONL line received from an agent.
///
/// `events` carries zero-or-more [`ParsedAgentEvent`]s — the parser-visible
/// payload prior to envelope stamping. The session layer joins each one
/// with the per-spawn `EventEnvelope` (bead id / iteration / source /
/// ts_ms / seq) via `AgentEvent::from_parsed` (RS-12). A `Vec` is used
/// because some protocol messages map to multiple events (claude's
/// `result/success` produces both `TurnEnd` and `SessionComplete`); other
/// lines (e.g. claude's `system/init`) produce zero.
///
/// `response` carries an optional JSONL line that the session must write back
/// on the agent's stdin in response to the parsed line. The canonical case is
/// claude's `control_request` auto-approve flow: the parser produces the
/// `control_response` payload and the session is responsible for the IO.
pub struct ParsedLine {
    pub events: Vec<ParsedAgentEvent>,
    pub response: Option<String>,
}

/// Backend-specific protocol bridge.
///
/// Implementors live in `agent` (one for pi-mono RPC, one for claude
/// stream-json) and translate between backend wire formats and loom-driver's
/// neutral surface. The session only knows about `LineParse` — once the
/// parser converts a line into a [`ParsedLine`], no downstream code can tell
/// which backend produced it.
///
/// `encode_*` methods serialize driver-side commands (initial prompt, mid-
/// session steering, abort) into JSONL lines for the session to write to the
/// agent's stdin. They live alongside `parse_line` because both directions of
/// the wire are backend-specific and a single trait keeps the session a
/// concrete generic-free type.
pub trait LineParse: Send {
    /// Parse one JSONL line received from the agent's stdout.
    fn parse_line(&self, line: &str) -> Result<ParsedLine, ProtocolError>;

    /// Encode the initial prompt that opens the session. Returned string is
    /// written to stdin verbatim — implementors include the trailing `\n`.
    fn encode_prompt(&self, msg: &str) -> Result<String, ProtocolError>;

    /// Encode a mid-session steering message. Same framing rules as
    /// [`Self::encode_prompt`].
    fn encode_steer(&self, msg: &str) -> Result<String, ProtocolError>;

    /// Encode an abort command, or `None` if the backend has no abort wire
    /// command (claude is killed via signals instead).
    fn encode_abort(&self) -> Result<Option<String>, ProtocolError>;
}

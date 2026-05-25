use std::io;
use std::time::Duration;

use displaydoc::Display;
use thiserror::Error;

/// Errors raised at the JSONL / agent-protocol boundary.
///
/// The variants cover the layers where loom-driver is the only code that knows
/// about the wire (line framing, JSON parse, subprocess IO) plus the small set
/// of semantic outcomes a backend `LineParse` reports back upward.
#[derive(Debug, Display, Error)]
pub enum ProtocolError {
    /// invalid JSON on protocol line
    InvalidJson(#[from] serde_json::Error),

    /// unknown message type: {0}
    UnknownMessageType(String),

    /// io failure on agent stdio
    Io(#[from] io::Error),

    /// agent process exited with code {0}
    ProcessExit(i32),

    /// unexpected end of agent event stream
    UnexpectedEof,

    /// JSONL line too long: {len} bytes (max {max})
    LineTooLong { len: usize, max: usize },

    /// operation not supported by this backend
    Unsupported,

    /// handshake stage `{stage}` did not complete within {after:?}
    HandshakeTimeout {
        stage: &'static str,
        after: Duration,
    },

    /// parser-internal mutex was poisoned by a panicking thread
    LockPoisoned,
}

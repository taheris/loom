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

    /// invalid JSON on protocol line: {preview} ({source})
    InvalidProtocolLine {
        preview: String,
        #[source]
        source: serde_json::Error,
    },

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

impl ProtocolError {
    pub fn invalid_protocol_line(line: &str, source: serde_json::Error) -> Self {
        Self::InvalidProtocolLine {
            preview: preview_protocol_line(line),
            source,
        }
    }
}

fn preview_protocol_line(line: &str) -> String {
    const MAX_PREVIEW_BYTES: usize = 240;
    let mut preview = String::new();
    for ch in line.chars() {
        let escaped = ch.escape_debug().to_string();
        if preview.len() + escaped.len() > MAX_PREVIEW_BYTES {
            preview.push('…');
            break;
        }
        preview.push_str(&escaped);
    }
    preview
}

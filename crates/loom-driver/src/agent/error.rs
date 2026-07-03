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

impl ProtocolError {
    pub fn invalid_protocol_line(_line: &str, source: serde_json::Error) -> Self {
        Self::InvalidJson(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_error() -> serde_json::Error {
        serde_json::from_str::<serde_json::Value>("{").expect_err("invalid JSON fixture")
    }

    fn variant_name(err: ProtocolError) -> &'static str {
        match err {
            ProtocolError::InvalidJson(_) => "InvalidJson",
            ProtocolError::UnknownMessageType(_) => "UnknownMessageType",
            ProtocolError::Io(_) => "Io",
            ProtocolError::ProcessExit(_) => "ProcessExit",
            ProtocolError::UnexpectedEof => "UnexpectedEof",
            ProtocolError::LineTooLong { .. } => "LineTooLong",
            ProtocolError::Unsupported => "Unsupported",
            ProtocolError::HandshakeTimeout { .. } => "HandshakeTimeout",
            ProtocolError::LockPoisoned => "LockPoisoned",
        }
    }

    #[test]
    fn protocol_error_variant_set_matches_agent_spec() {
        let variants = vec![
            ProtocolError::InvalidJson(json_error()),
            ProtocolError::UnknownMessageType("mystery".to_string()),
            ProtocolError::Io(io::Error::other("io")),
            ProtocolError::ProcessExit(7),
            ProtocolError::UnexpectedEof,
            ProtocolError::LineTooLong { len: 11, max: 10 },
            ProtocolError::Unsupported,
            ProtocolError::HandshakeTimeout {
                stage: "probe",
                after: Duration::from_secs(1),
            },
            ProtocolError::LockPoisoned,
        ];
        let names = variants.into_iter().map(variant_name).collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "InvalidJson",
                "UnknownMessageType",
                "Io",
                "ProcessExit",
                "UnexpectedEof",
                "LineTooLong",
                "Unsupported",
                "HandshakeTimeout",
                "LockPoisoned",
            ],
        );
    }
}

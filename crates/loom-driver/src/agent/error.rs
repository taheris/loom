use std::{fmt, io, time::Duration};

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
    InvalidJson {
        #[source]
        source: serde_json::Error,
        line: Option<RejectedLine>,
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

/// Untrusted protocol input, omitted from diagnostic formatting until redacted.
pub struct RejectedLine(String);

impl fmt::Debug for RejectedLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl From<serde_json::Error> for ProtocolError {
    fn from(source: serde_json::Error) -> Self {
        Self::InvalidJson { source, line: None }
    }
}

impl ProtocolError {
    pub fn invalid_protocol_line(line: &str, source: serde_json::Error) -> Self {
        Self::InvalidJson {
            source,
            line: Some(RejectedLine(line.to_owned())),
        }
    }

    /// Untrusted input for diagnostics; redact secrets before escaping or truncating.
    pub fn protocol_line(&self) -> Option<&str> {
        match self {
            Self::InvalidJson {
                line: Some(line), ..
            } => Some(&line.0),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_error() -> serde_json::Error {
        serde_json::from_str::<serde_json::Value>("{").expect_err("invalid JSON fixture")
    }

    #[test]
    fn rejected_line_is_retained_without_exposing_it_in_error_formatting() {
        let line = "\u{1b}[33m WARN token=private-fixture-value";
        let source = serde_json::from_str::<serde_json::Value>(line).expect_err("not JSON");
        let error = ProtocolError::invalid_protocol_line(line, source);
        assert_eq!(error.protocol_line(), Some(line));
        assert!(!format!("{error:?}").contains("private-fixture-value"));
        assert!(!error.to_string().contains("private-fixture-value"));
        assert_eq!(
            std::error::Error::source(&error)
                .expect("JSON cause")
                .to_string(),
            "expected value at line 1 column 1",
        );
        assert!(ProtocolError::from(json_error()).protocol_line().is_none());
    }

    fn variant_name(err: &ProtocolError) -> &'static str {
        match err {
            ProtocolError::InvalidJson { .. } => "InvalidJson",
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
            ProtocolError::from(json_error()),
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
        let names = variants.iter().map(variant_name).collect::<Vec<_>>();
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

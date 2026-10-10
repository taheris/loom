//! Inbox projection of the canonical agent-output contract.

use std::collections::HashSet;

use displaydoc::Display;
use loom_events::identifier::BeadId;
use thiserror::Error;

use crate::output::{self, Context, Failure, Message, Phase};

pub use crate::output::Message as TerminalMarker;

#[derive(Debug, Display, Error)]
pub enum TerminalMarkerError {
    /// inbox output violates the agent-output contract
    Contract(#[from] Failure),
    /// inbox apply repeats proposal `{id}`
    DuplicateProposal { id: BeadId, context: Context },
}

impl TerminalMarkerError {
    /// An ordinary interactive turn may continue without a terminal.
    pub fn is_conversation(&self) -> bool {
        matches!(self, Self::Contract(failure) if failure.is_conversation())
    }
}

/// Decode and admit an inbox session; proposal application remains driver-owned.
///
/// # Errors
/// Retains shared diagnostics and context, or the context of a repeated proposal.
pub fn parse(output: &str) -> Result<Message, TerminalMarkerError> {
    let session = output::decode(output, Phase::Inbox)?;
    if let Message::Apply(payload) = &session.terminal().message {
        let mut seen = HashSet::new();
        for id in payload.proposals.as_slice() {
            if !seen.insert(id) {
                return Err(TerminalMarkerError::DuplicateProposal {
                    id: id.clone(),
                    context: session.into_context(),
                });
            }
        }
    }
    Ok(session.terminal().message.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbox_adapter_admits_multiline_apply() {
        let marker = parse("LOOM_APPLY: {\n  \"proposals\": [\"lm-proposal.1\"]\n}").unwrap();
        let Message::Apply(payload) = marker else {
            panic!("expected apply");
        };
        assert_eq!(payload.proposals[0].as_str(), "lm-proposal.1");
    }

    #[test]
    fn inbox_adapter_preserves_malformed_context() {
        let raw = "LOOM_APPLY: {}\nLOOM_COMPLETE";
        let TerminalMarkerError::Contract(failure) = parse(raw).unwrap_err() else {
            panic!("expected contract failure");
        };
        assert_eq!(failure.context().raw(), raw);
        assert!(failure.to_string().contains("missing field `proposals`"));
        assert_eq!(
            failure.context().terminal().unwrap().message,
            Message::Complete
        );
        assert!(!failure.is_conversation());
    }

    #[test]
    fn inbox_adapter_only_allows_markerless_conversation_to_continue() {
        assert!(
            parse("How should we proceed?")
                .unwrap_err()
                .is_conversation()
        );
        for raw in [
            "LOOM_COMPLETE\nmore",
            "LOOM_COMPLETE\nLOOM_COMPLETE",
            "LOOM_WAITING",
            "LOOM_APPLY: {\"proposals\":[\"bad bead\"]}",
            "LOOM_BOGUS\nLOOM_COMPLETE",
        ] {
            assert!(!parse(raw).unwrap_err().is_conversation(), "{raw}");
        }
    }

    #[test]
    fn inbox_adapter_rejects_duplicate_proposals() {
        assert!(matches!(
            parse("LOOM_APPLY: {\"proposals\":[\"lm-proposal.1\",\"lm-proposal.1\"]}"),
            Err(TerminalMarkerError::DuplicateProposal { .. })
        ));
    }
}

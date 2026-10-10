//! Compiled protocol facts, independent of workflow explanations.

use std::fmt::Write as _;

use loom_protocol::output::{Arity, Phase};

pub fn contract(phase: Phase) -> String {
    let rows = phase.syntax().fold(String::new(), |mut rows, entry| {
        let wire = match entry.arity {
            Arity::Unit => entry.marker.to_owned(),
            Arity::Data => format!("{}: <JSON object>", entry.marker),
        };
        let _ = writeln!(rows, "| `{wire}` | {:?} |", entry.role); // String writes are infallible.
        rows
    });
    format!(
        "## Canonical Agent-Output Syntax\n\nOnly these messages are admitted in {phase:?}. This compiled table is authoritative for spelling, role, and payload arity. Decoding proposes outcomes; it does not authorize Beads changes or publication.\n\n| Wire spelling | Role |\n| --- | --- |\n{rows}\nLive messages start in column one. Data messages require strict JSON (compact or pretty-printed); escape newlines inside strings. Do not use preceding prose as a payload. Records precede exactly one final logical terminal, with no trailing commentary. Prompt examples, tool output, and driver events are not agent messages.\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_syntax_uses_canonical_phase_role_and_arity() {
        let todo = contract(Phase::Todo);
        assert!(todo.contains("`LOOM_CLARIFY: <JSON object>` | Record"));
        assert!(todo.contains("`LOOM_TODO: <JSON object>` | Terminal"));
        assert!(!todo.contains("LOOM_COMPLETE"));
        assert!(!todo.contains("LOOM_APPLY"));
        let review = contract(Phase::Review);
        assert!(review.contains("`LOOM_FINDING: <JSON object>` | Record"));
        assert!(review.contains("`LOOM_COMPLETE` | Terminal"));
        assert!(!review.contains("LOOM_CLARIFY"));
        let inbox = contract(Phase::Inbox);
        assert!(inbox.contains("`LOOM_APPLY: <JSON object>` | Terminal"));
        assert!(!inbox.contains("LOOM_RETRY"));
    }
}

use loom_driver::identifier::BeadId;

use super::error::MsgError;
use super::list::MsgKind;
use super::options::{OptionsParse, parse_options_in};

/// What `loom msg -a <choice>` should write to the bead. `Option` is the
/// composed `Chose option N — title: body` note from a successful integer
/// lookup; `Verbatim` carries any non-integer choice unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FastReply {
    Option { n: u32, note: String },
    Verbatim { note: String },
}

/// Compose the bead note for a `-a <choice>` fast-reply.
///
/// - `MsgKind::Clarify`:
///     - Pure-integer `choice` → look up `### Option <choice>` in the parsed
///       options. Match → `FastReply::Option`; miss → [`MsgError::OptionMissing`]
///       carrying the available indices for the user-facing error message.
///     - Anything else → `FastReply::Verbatim`.
/// - `MsgKind::Blocked` → always `FastReply::Verbatim` (free-form per
///   `specs/harness.md` lines 1204-1205).
pub fn build_fast_reply(
    bead: &BeadId,
    choice: &str,
    notes: Option<&str>,
    description: &str,
    kind: MsgKind,
) -> Result<FastReply, MsgError> {
    if matches!(kind, MsgKind::Clarify)
        && let Ok(n) = choice.parse::<u32>()
    {
        let parsed = parse_options_in(notes, description);
        return resolve_option(bead, n, &parsed);
    }
    Ok(FastReply::Verbatim {
        note: choice.to_string(),
    })
}

/// Compose the bead note for a `loom msg -o <int> -b <id>` invocation.
///
/// Parses notes ∪ description for the `## Options` block, looks up
/// `### Option <n>`, and returns the composed note text — independent
/// of bead kind (Clarify vs Blocked). A missing subsection produces
/// [`MsgError::OptionMissing`] carrying the available indices for the
/// user-facing error message. Used by the I1 flag-split surface where
/// `-o` does strict option-lookup; the legacy `-a <choice>` path that
/// kind-discriminates lives in [`build_fast_reply`].
///
/// `notes` carries the reviewer's promoted-blocked options when the
/// bead was promoted via `bd update --notes` (see
/// `specs/gate.md` § Options Format Contract).
pub fn compose_option_note(
    bead: &BeadId,
    n: u32,
    notes: Option<&str>,
    description: &str,
) -> Result<String, MsgError> {
    let parsed = parse_options_in(notes, description);
    match resolve_option(bead, n, &parsed)? {
        FastReply::Option { note, .. } | FastReply::Verbatim { note } => Ok(note),
    }
}

fn resolve_option(bead: &BeadId, n: u32, parsed: &OptionsParse) -> Result<FastReply, MsgError> {
    if let Some(opt) = parsed.options.iter().find(|o| o.n == n) {
        let note = if opt.title.is_empty() {
            format!("Chose option {n}: {}", opt.body)
        } else if opt.body.is_empty() {
            format!("Chose option {n} — {}", opt.title)
        } else {
            format!("Chose option {n} — {}: {}", opt.title, opt.body)
        };
        Ok(FastReply::Option { n, note })
    } else {
        let available = parsed
            .options
            .iter()
            .map(|o| o.n.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        Err(MsgError::OptionMissing {
            bead: bead.to_string(),
            option: n,
            available,
        })
    }
}

/// Note written by `-d` (dismiss) so the bead carries a recognisable marker
/// after the label is removed.
pub const DISMISS_NOTE: &str =
    "Dismissed via loom msg -d. Agent should work around the open question.";

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "tests use panicking helpers"
)]
mod tests {
    use super::*;

    fn desc() -> &'static str {
        "## Options — pick a path

### Option 1 — Preserve invariant
Revert. Cost: churn.

### Option 2 — Keep on top
Accept. Cost: debt.
"
    }

    #[test]
    fn integer_choice_resolves_to_option_note() -> Result<(), MsgError> {
        let bead = BeadId::new("wx-x").expect("valid bead id");
        let reply = build_fast_reply(&bead, "1", None, desc(), MsgKind::Clarify)?;
        match reply {
            FastReply::Option { n, note } => {
                assert_eq!(n, 1);
                assert!(note.contains("Chose option 1"));
                assert!(note.contains("Preserve invariant"));
                assert!(note.contains("Revert"));
            }
            other => panic!("expected Option, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn missing_option_index_errors_with_available_list() {
        let bead = BeadId::new("wx-x").expect("valid bead id");
        let err = build_fast_reply(&bead, "9", None, desc(), MsgKind::Clarify)
            .expect_err("expected error");
        match err {
            MsgError::OptionMissing {
                bead,
                option,
                available,
            } => {
                assert_eq!(bead, "wx-x");
                assert_eq!(option, 9);
                assert_eq!(available, "1, 2");
            }
            other => panic!("expected OptionMissing, got {other:?}"),
        }
    }

    #[test]
    fn verbatim_string_passes_through_unchanged() -> Result<(), MsgError> {
        let bead = BeadId::new("wx-x").expect("valid bead id");
        let reply = build_fast_reply(&bead, "free-form answer", None, desc(), MsgKind::Clarify)?;
        match reply {
            FastReply::Verbatim { note } => assert_eq!(note, "free-form answer"),
            other => panic!("expected Verbatim, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn integer_with_no_options_section_errors() {
        let bead = BeadId::new("wx-x").expect("valid bead id");
        let err = build_fast_reply(&bead, "1", None, "no options at all", MsgKind::Clarify)
            .expect_err("expected missing option");
        assert!(matches!(err, MsgError::OptionMissing { .. }));
    }

    #[test]
    fn empty_title_or_body_renders_partial_note() -> Result<(), MsgError> {
        let bead = BeadId::new("wx-x").expect("valid bead id");
        let no_title = "## Options\n\n### Option 1\nonly body\n";
        let reply = build_fast_reply(&bead, "1", None, no_title, MsgKind::Clarify)?;
        match reply {
            FastReply::Option { note, .. } => {
                assert!(note.contains("Chose option 1"));
                assert!(note.contains("only body"));
            }
            other => panic!("expected Option, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn blocked_integer_choice_is_always_verbatim() -> Result<(), MsgError> {
        let bead = BeadId::new("wx-x").expect("valid bead id");
        let reply = build_fast_reply(&bead, "1", None, desc(), MsgKind::Blocked)?;
        match reply {
            FastReply::Verbatim { note } => assert_eq!(note, "1"),
            other => panic!("expected Verbatim, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn compose_option_note_reads_options_from_notes_when_present() -> Result<(), MsgError> {
        let bead = BeadId::new("wx-x").expect("valid bead id");
        let notes = "## Options — promoted\n\n### Option 1 — fix\nbody.\n";
        let description = "agent-blocked: no options here";
        let note = compose_option_note(&bead, 1, Some(notes), description)?;
        assert!(note.contains("Chose option 1"));
        assert!(note.contains("fix"));
        Ok(())
    }

    #[test]
    fn build_fast_reply_reads_options_from_notes_when_present() -> Result<(), MsgError> {
        let bead = BeadId::new("wx-x").expect("valid bead id");
        let notes = "## Options — promoted\n\n### Option 1 — promoted-fix\nbody.\n";
        let reply = build_fast_reply(
            &bead,
            "1",
            Some(notes),
            "no options in description",
            MsgKind::Clarify,
        )?;
        match reply {
            FastReply::Option { note, .. } => {
                assert!(note.contains("promoted-fix"));
            }
            other => panic!("expected Option, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn blocked_free_form_passes_through_unchanged() -> Result<(), MsgError> {
        let bead = BeadId::new("wx-x").expect("valid bead id");
        let reply = build_fast_reply(
            &bead,
            "use the staging endpoint",
            None,
            "no options here",
            MsgKind::Blocked,
        )?;
        match reply {
            FastReply::Verbatim { note } => assert_eq!(note, "use the staging endpoint"),
            other => panic!("expected Verbatim, got {other:?}"),
        }
        Ok(())
    }
}

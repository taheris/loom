//! Canonical agent-origin messages, strict framing, and phase admission.

mod framing;

use std::collections::HashSet;
use std::ops::Range;

use displaydoc::Display;
use loom_events::identifier::BeadId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::gate::RawFinding;
use crate::todo::{NonEmptyString, NonEmptyVec, TodoSuccess};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decisions {
    pub decisions: NonEmptyVec<BeadId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proposals {
    pub proposals: NonEmptyVec<BeadId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    pub summary: NonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reason {
    pub reason: NonEmptyString,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Record,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arity {
    Unit,
    Data,
}

/// Prompt-visible syntax derived from the same declaration as decoding and admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Syntax {
    pub marker: &'static str,
    pub role: Role,
    pub arity: Arity,
    phases: &'static [Phase],
}

macro_rules! messages {
    ($($variant:ident $(($payload:ty))? => ($marker:literal, $role:ident, $($phase:ident)|+)),+ $(,)?) => {
        /// Typed proposals only; decoding grants no workflow authority.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum Message {
            $($variant $(($payload))?),+
        }

        const SYNTAX: &[Syntax] = &[
            $(Syntax { marker: $marker, role: Role::$role, arity: messages!(@arity $($payload)?), phases: &[$(Phase::$phase),+] }),+
        ];

        impl Phase {
            pub const fn admits(self, message: &Message) -> bool {
                match message {
                    $(Message::$variant $( (messages!(@pattern $payload)) )? => matches!(self, $(Self::$phase)|+)),+
                }
            }
        }

        impl Message {
            pub const fn marker(&self) -> &'static str {
                match self {
                    $(Self::$variant $( (messages!(@pattern $payload)) )? => $marker),+
                }
            }

            /// Compatibility identity delegates to the canonical marker metadata.
            pub const fn identity(&self) -> &'static str {
                self.marker()
            }

            pub const fn role(&self) -> Role {
                match self {
                    $(Self::$variant $( (messages!(@pattern $payload)) )? => Role::$role),+
                }
            }

            /// Encode the compact canonical wire spelling, without a line terminator.
            ///
            /// # Errors
            /// Returns a JSON serialization error if the domain payload cannot serialize.
            pub fn to_wire(&self) -> Result<String, serde_json::Error> {
                match self {
                    $(Self::$variant $( (messages!(@binding $payload, value)) )? =>
                        messages!(@encode $marker, value $(, $payload)?)),+
                }
            }

            fn recognizes(marker: &str) -> bool {
                matches!(marker, $($marker)|+)
            }

            fn parse(marker: &str, payload: Option<&str>) -> Result<Self, Error> {
                match marker {
                    $($marker => messages!(@decode $variant, payload $(, $payload)?)),+,
                    _ => Err(Error::UnknownMarker { marker: marker.to_owned() }),
                }
            }
        }
    };
    (@arity $payload:ty) => { Arity::Data };
    (@arity) => { Arity::Unit };
    (@pattern $payload:ty) => { _ };
    (@binding $payload:ty, $value:ident) => { $value };
    (@encode $marker:literal, $value:ident, $payload:ty) => {
        serde_json::to_string($value).map(|json| format!("{}: {json}", $marker))
    };
    (@encode $marker:literal, $value:ident) => { Ok($marker.to_owned()) };
    (@decode $variant:ident, $payload:ident, $ty:ty) => {
        $payload.filter(|json| json.starts_with('{')).ok_or(Error::ExpectedObject).and_then(|json| {
            serde_json::from_str::<$ty>(json)
                .map(Self::$variant)
                .map_err(|source| Error::Json { source })
        })
    };
    (@decode $variant:ident, $payload:ident) => {
        if $payload.is_some() {
            Err(Error::UnexpectedPayload)
        } else {
            Ok(Self::$variant)
        }
    };
}

messages! {
    Finding(RawFinding) => ("LOOM_FINDING", Record, Review),
    Clarify(Decisions) => ("LOOM_CLARIFY", Record, Todo|Loop),
    Complete => ("LOOM_COMPLETE", Terminal, Plan|Loop|Review|Inbox),
    Noop => ("LOOM_NOOP", Terminal, Loop),
    Waiting => ("LOOM_WAITING", Terminal, Todo|Loop),
    Concern(Summary) => ("LOOM_CONCERN", Terminal, Review),
    Retry(Reason) => ("LOOM_RETRY", Terminal, Todo|Loop|Review),
    Blocked(Reason) => ("LOOM_BLOCKED", Terminal, Todo|Loop|Review),
    Todo(TodoSuccess) => ("LOOM_TODO", Terminal, Todo),
    Apply(Proposals) => ("LOOM_APPLY", Terminal, Inbox),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Plan,
    Todo,
    Loop,
    Review,
    Inbox,
}

impl Phase {
    pub fn syntax(self) -> impl Iterator<Item = &'static Syntax> {
        SYNTAX
            .iter()
            .filter(move |entry| entry.phases.contains(&self))
    }
}

/// UTF-8 byte range and one-based starting physical line in the original text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub bytes: Range<usize>,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub message: Message,
    pub span: Span,
}

/// Independently decoded messages, including wrong-phase messages, for diagnosis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    raw: String,
    phase: Phase,
    messages: Vec<Located>,
}

impl Context {
    pub fn raw(&self) -> &str {
        &self.raw
    }

    pub const fn phase(&self) -> Phase {
        self.phase
    }

    pub fn messages(&self) -> &[Located] {
        &self.messages
    }

    /// The unique independently decoded terminal, even on admission failure.
    pub fn terminal(&self) -> Option<&Located> {
        let mut terminals = self
            .messages
            .iter()
            .filter(|located| located.message.role() == Role::Terminal);
        let terminal = terminals.next()?;
        if terminals.next().is_some() {
            return None;
        }
        Some(terminal)
    }

    /// Exact checked union; existence and decision scope still require resolution.
    pub fn decisions(&self) -> HashSet<BeadId> {
        self.messages
            .iter()
            .filter_map(|located| {
                if let Message::Clarify(payload) = &located.message
                    && self.phase.admits(&located.message)
                {
                    Some(payload.decisions.as_slice())
                } else {
                    None
                }
            })
            .flatten()
            .cloned()
            .collect()
    }
}

/// A syntactically and phase-admitted session, not authorization to act.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    context: Context,
    terminal_index: usize,
}

impl Session {
    pub const fn context(&self) -> &Context {
        &self.context
    }

    pub fn terminal(&self) -> &Located {
        &self.context.messages[self.terminal_index]
    }

    pub fn into_context(self) -> Context {
        self.context
    }
}

#[derive(Debug, Display, Error)]
/// agent output violates the message contract: {diagnostics:?}
pub struct Failure {
    context: Context,
    diagnostics: Vec<Diagnostic>,
}

impl Failure {
    pub const fn context(&self) -> &Context {
        &self.context
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// True only for an otherwise valid markerless interactive turn.
    pub fn is_conversation(&self) -> bool {
        self.diagnostics.len() == 1 && matches!(self.diagnostics[0].error, Error::MissingTerminal)
    }

    pub fn into_context(self) -> Context {
        self.context
    }
}

#[derive(Debug)]
pub struct Diagnostic {
    pub span: Span,
    pub error: Error,
}

#[derive(Debug, Display, Error)]
pub enum Error {
    /// unknown agent-output marker `{marker}`
    UnknownMarker { marker: String },
    /// data message requires a colon followed by one JSON object
    ExpectedObject,
    /// unit message cannot carry a payload
    UnexpectedPayload,
    /// marker line contains an invalid suffix
    MarkerSuffix,
    /// JSON object closing line contains a non-whitespace suffix
    ObjectSuffix,
    /// payload has no unambiguous complete object boundary
    UnterminatedObject,
    /// agent-output payload is not valid typed JSON
    Json {
        #[source]
        source: serde_json::Error,
    },
    /// message `{marker}` is not admitted in phase {phase:?}
    WrongPhase { marker: &'static str, phase: Phase },
    /// session has no independently decoded terminal
    MissingTerminal,
    /// session has multiple terminals
    DuplicateTerminal,
    /// record appears after a terminal
    RecordAfterTerminal,
    /// terminal is followed by non-whitespace text
    TrailingText,
}

/// Decode only agent-origin text; callers must not mix in tool, prompt, or driver text.
///
/// # Errors
/// Returns original text, independent context, and located diagnostics on any violation.
pub fn decode(text: &str, phase: Phase) -> Result<Session, Failure> {
    let (messages, mut diagnostics) = framing::decode(text);
    let context = Context {
        raw: text.to_owned(),
        phase,
        messages,
    };
    let mut terminal_index = None;
    for (index, located) in context.messages.iter().enumerate() {
        if !phase.admits(&located.message) {
            diagnostics.push(Diagnostic {
                span: located.span.clone(),
                error: Error::WrongPhase {
                    marker: located.message.marker(),
                    phase,
                },
            });
        }
        match located.message.role() {
            Role::Terminal => {
                if terminal_index.is_some() {
                    diagnostics.push(Diagnostic {
                        span: located.span.clone(),
                        error: Error::DuplicateTerminal,
                    });
                } else {
                    terminal_index = Some(index);
                }
                let suffix = located.span.bytes.end..text.len();
                if !text[suffix.clone()].trim().is_empty() {
                    diagnostics.push(Diagnostic {
                        span: Span {
                            bytes: suffix,
                            line: located.span.line
                                + text[located.span.bytes.clone()]
                                    .bytes()
                                    .filter(|byte| *byte == b'\n')
                                    .count(),
                        },
                        error: Error::TrailingText,
                    });
                }
            }
            Role::Record if terminal_index.is_some() => diagnostics.push(Diagnostic {
                span: located.span.clone(),
                error: Error::RecordAfterTerminal,
            }),
            Role::Record => {}
        }
    }
    let Some(terminal_index) = terminal_index else {
        diagnostics.push(Diagnostic {
            span: Span {
                bytes: text.len()..text.len(),
                line: text.bytes().filter(|byte| *byte == b'\n').count() + 1,
            },
            error: Error::MissingTerminal,
        });
        return Err(Failure {
            context,
            diagnostics,
        });
    };
    if diagnostics.is_empty() {
        Ok(Session {
            context,
            terminal_index,
        })
    } else {
        Err(Failure {
            context,
            diagnostics,
        })
    }
}

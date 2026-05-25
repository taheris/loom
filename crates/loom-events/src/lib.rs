//! `loom-events` — the public contract leaf crate.
//!
//! Frontends, SSE bridges, and external log analyzers depend on this
//! crate to consume `AgentEvent`, the domain identifier newtypes, and
//! the agent-driver contract surface (`Session`, `EventSink`,
//! `SessionCommand`) without pulling in the `loom-driver` runtime
//! (rusqlite, gix, tokio, …). The Cargo dependency surface is
//! intentionally tiny — `futures-core`, `serde`, `serde_json`,
//! `thiserror`. Adding anything else changes the dependency surface of
//! every downstream consumer and requires a spec change.
//!
//! The `loom-driver` crate re-exports the contents of this crate so
//! existing call sites (`use loom_driver::identifier::BeadId`,
//! `use loom_driver::agent::event::AgentEvent`) keep working without
//! churn. New code that doesn't need the runtime should depend on
//! `loom-events` directly.

use std::pin::Pin;

use futures_core::Stream;

pub mod event;
pub mod identifier;

pub use event::{AgentEvent, DriverKind, EnvelopeBuilder, EventEnvelope, ParsedAgentEvent, Source};

/// Boxed event stream returned by [`Session::prompt`].
///
/// Implementations pin a heap-allocated `Stream` so `dyn Session` is
/// usable behind a `Box`. The workflow layer holds backends as
/// `Box<dyn Session<Events = EventStream>>` to make per-phase backend
/// selection a runtime choice.
pub type EventStream = Pin<Box<dyn Stream<Item = AgentEvent> + Send>>;

/// Coarse-grained session mode the workflow can switch between
/// mid-session. Backends translate this into backend-specific protocol
/// calls; the variant set is `#[non_exhaustive]` so future modes can
/// land additively without breaking consumers that exhaustively match.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionMode {
    /// Whatever mode the backend was launched in — no override.
    Default,
}

/// The public agent-driver contract. Workflow code holds backends as
/// `Box<dyn Session<Events = EventStream>>` so per-phase backend
/// selection is a runtime choice rather than a compile-time one.
///
/// Subprocess-driving backends (Pi, Claude) keep an internal typestate
/// (`AgentSession<Idle|Active>` in `loom-driver`) as a private mechanic
/// — that typestate does not leak through this trait. Backends without
/// a subprocess (Direct, future ACP-exposed sessions) carry no
/// typestate at all; the asymmetry is why the trait sits on top.
pub trait Session: Send {
    /// Stream type each backend yields from [`Session::prompt`]. By
    /// convention this resolves to [`EventStream`] so `Box<dyn Session>`
    /// is usable; the bound here lets backends pick a stream impl
    /// internally before pinning it at the trait boundary.
    type Events: Stream<Item = AgentEvent> + Send;

    /// Send the initial prompt and return the event stream the backend
    /// produces in response.
    fn prompt(&mut self, msg: String) -> Self::Events;

    /// Inject a mid-session steering message into the agent's next turn.
    fn steer(&mut self, msg: String);

    /// Terminate the in-flight operation.
    fn cancel(&mut self);

    /// Switch the session into a new mode. Backends translate the
    /// variant into backend-specific protocol calls.
    fn set_mode(&mut self, mode: SessionMode);
}

/// Universal `AgentEvent` consumer interface. Renderers, log writers,
/// and observers all implement this trait; the driver fans the live
/// event stream into a chain of `EventSink`s composed via
/// [`EventSinkExt::tee`].
///
/// `emit` is sync — sinks push to channels, write to disk, or mutate
/// counters without awaiting. Sinks that need async work own a channel
/// internally. The `Send` bound supports multi-runtime deployments.
///
/// `react()` is pull-based with a default empty implementation. The
/// driver invokes it after every **non-streaming** event (lifecycle,
/// tool, driver, operational) and applies the returned commands to the
/// live [`Session`]. Streaming variants (`text_delta`,
/// `thinking_delta`, `toolcall_delta`) do not trigger `react()` because
/// observer state doesn't change on text bytes.
pub trait EventSink: Send {
    /// Consume one event. The caller still owns the event so multiple
    /// sinks read it without cloning.
    fn emit(&mut self, event: &AgentEvent);

    /// Commands the sink wants the driver to apply before the next
    /// event is read. Default empty so sinks that only observe don't
    /// need to override.
    fn react(&mut self) -> Vec<SessionCommand> {
        Vec::new()
    }
}

/// Commands an [`EventSink`] returns from `react()`. Variants are
/// deliberately narrower than `Session`'s full surface — observers
/// only have two levers, both safety-relevant. Direct callers of
/// `Session` retain the full surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCommand {
    /// Inject a system message into the next turn.
    Steer(String),
    /// Terminate the session with this reason. The driver treats
    /// `Abort` as terminal — subsequent commands in the same `react()`
    /// batch are not applied, the session is cancelled, and the
    /// outcome classifies as `observer-abort` in the verdict gate.
    Abort(String),
}

/// Composition combinator returned by [`EventSinkExt::tee`]. Forwards
/// `emit`/`react` to the left sink first, then the right, preserving
/// registration order so observers fire in the order the workflow
/// chained them.
pub struct TeeSink<S, O> {
    left: S,
    right: O,
}

impl<S: EventSink, O: EventSink> EventSink for TeeSink<S, O> {
    fn emit(&mut self, event: &AgentEvent) {
        self.left.emit(event);
        self.right.emit(event);
    }

    fn react(&mut self) -> Vec<SessionCommand> {
        let mut commands = self.left.react();
        commands.extend(self.right.react());
        commands
    }
}

/// Chainable builder for sink composition. The driver constructs the
/// per-session sink chain with `LogSink::new(path).tee(observer_a)
/// .tee(observer_b)`; registration order equals the `react()`
/// invocation order.
pub trait EventSinkExt: EventSink + Sized {
    fn tee<O: EventSink>(self, other: O) -> TeeSink<Self, O> {
        TeeSink {
            left: self,
            right: other,
        }
    }
}

impl<T: EventSink + Sized> EventSinkExt for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Source;
    use crate::identifier::BeadId;

    /// Sink that counts events seen and emits a fixed command list on
    /// `react()`. Drives the composition tests without dragging in a
    /// real backend.
    struct RecordingSink {
        seen: u32,
        commands: Vec<SessionCommand>,
    }

    impl RecordingSink {
        fn new(commands: Vec<SessionCommand>) -> Self {
            Self { seen: 0, commands }
        }
    }

    impl EventSink for RecordingSink {
        fn emit(&mut self, _event: &AgentEvent) {
            self.seen += 1;
        }

        fn react(&mut self) -> Vec<SessionCommand> {
            std::mem::take(&mut self.commands)
        }
    }

    fn sample_event() -> AgentEvent {
        AgentEvent::TextDelta {
            envelope: EventEnvelope {
                bead_id: BeadId::new("sample-1").expect("valid bead id"),
                molecule_id: None,
                iteration: 1,
                source: Source::Agent,
                ts_ms: 0,
                seq: 0,
            },
            text: "hi".into(),
        }
    }

    /// Registration order = react invocation order. The first sink in
    /// the chain produces its commands first; the second sink's
    /// commands follow. Verifies `TeeSink::react` concatenates rather
    /// than interleaves or reverses.
    #[test]
    fn tee_chain_preserves_registration_order_for_react() {
        let first = RecordingSink::new(vec![SessionCommand::Steer("first-steer".into())]);
        let second = RecordingSink::new(vec![SessionCommand::Steer("second-steer".into())]);
        let third = RecordingSink::new(vec![SessionCommand::Abort("third-abort".into())]);

        let mut chain = first.tee(second).tee(third);
        let commands = chain.react();

        assert_eq!(
            commands,
            vec![
                SessionCommand::Steer("first-steer".into()),
                SessionCommand::Steer("second-steer".into()),
                SessionCommand::Abort("third-abort".into()),
            ],
        );
    }

    /// `emit` fans the same event reference into every sink in
    /// registration order. Verifies every sink sees the event before
    /// `react()` is consulted.
    #[test]
    fn tee_chain_emits_to_every_sink_in_order() {
        let first = RecordingSink::new(Vec::new());
        let second = RecordingSink::new(Vec::new());

        let mut chain = first.tee(second);
        let event = sample_event();
        chain.emit(&event);
        chain.emit(&event);

        assert_eq!(chain.left.seen, 2);
        assert_eq!(chain.right.seen, 2);
    }
}

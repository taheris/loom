use std::collections::VecDeque;

use loom_driver::agent::SpawnConfig;
use loom_events::{AgentEvent, EventSink, SessionCommand};
use loom_render::{BeadOutcome, LogError, LogSink};

use super::Policy;

struct Pending {
    event: AgentEvent,
    remaining: usize,
}

/// Redacts session transcripts before the shared persistence/rendering fan-out.
/// Consecutive deltas may be re-chunked, retaining envelopes and event order.
/// Only a possible secret prefix waits; interruptions replace it with a marker.
pub struct Sink {
    inner: LogSink,
    policy: Policy,
    pending: VecDeque<Pending>,
    fragments: String,
}

impl Sink {
    pub(crate) fn new(inner: LogSink, config: &SpawnConfig) -> Self {
        Self {
            inner,
            policy: Policy::new(config),
            pending: VecDeque::new(),
            fragments: String::new(),
        }
    }

    pub(crate) fn emit(&mut self, event: &AgentEvent) -> Result<(), LogError> {
        if let Some(first) = self.pending.front()
            && !same_stream(&first.event, event)
        {
            self.flush(!closes_stream(&first.event, event))?;
        }
        let mut event = event.clone();
        if let Some(text) = fragment_mut(&mut event) {
            self.fragments.push_str(text);
            let remaining = text.len();
            text.clear();
            self.pending.push_back(Pending { event, remaining });
            let (redacted, consumed) = self.policy.prefix(&self.fragments, false);
            self.release(consumed, &redacted.text)?;
            if self.pending.len() > self.fragments.len() + 1 {
                self.flush(true)?;
            }
        } else {
            self.policy.event(&mut event);
            self.inner.emit(&event)?;
        }
        Ok(())
    }

    fn release(&mut self, mut consumed: usize, text: &str) -> Result<(), LogError> {
        if let Some(first) = self.pending.front_mut()
            && let Some(fragment) = fragment_mut(&mut first.event)
        {
            fragment.push_str(text);
        }
        self.fragments.drain(..consumed);
        while let Some(first) = self.pending.front_mut() {
            if first.remaining > consumed {
                first.remaining -= consumed;
                break;
            }
            consumed -= first.remaining;
            if let Some(first) = self.pending.pop_front() {
                self.inner.emit(&first.event)?;
            }
        }
        Ok(())
    }

    fn flush(&mut self, interrupted: bool) -> Result<(), LogError> {
        let text = if interrupted && !self.fragments.is_empty() {
            self.policy
                .secrets
                .iter()
                .find(|secret| secret.value.starts_with(&self.fragments))
                .map_or_else(
                    || self.policy.text(&self.fragments).text,
                    |secret| secret.redaction.marker.clone(),
                )
        } else {
            self.policy.text(&self.fragments).text
        };
        self.release(self.fragments.len(), &text)
    }

    pub(crate) fn finish(&mut self, outcome: BeadOutcome) -> Result<(), LogError> {
        self.flush(true)?;
        self.inner.finish(outcome)
    }
}

impl EventSink for Sink {
    fn emit(&mut self, event: &AgentEvent) {
        if let Err(error) = Self::emit(self, event) {
            tracing::warn!(error = ?error, "redacted event sink emit failed");
        }
    }

    fn react(&mut self) -> Vec<SessionCommand> {
        self.inner.react()
    }
}

const fn fragment_mut(event: &mut AgentEvent) -> Option<&mut String> {
    match event {
        AgentEvent::TextDelta { text, .. }
        | AgentEvent::ThinkingDelta { text, .. }
        | AgentEvent::ToolcallDelta { delta: text, .. } => Some(text),
        _ => None,
    }
}

fn same_stream(left: &AgentEvent, right: &AgentEvent) -> bool {
    match (left, right) {
        (AgentEvent::TextDelta { .. }, AgentEvent::TextDelta { .. })
        | (AgentEvent::ThinkingDelta { .. }, AgentEvent::ThinkingDelta { .. }) => true,
        (
            AgentEvent::ToolcallDelta { id: left, .. },
            AgentEvent::ToolcallDelta { id: right, .. },
        ) => left == right,
        _ => false,
    }
}

const fn closes_stream(left: &AgentEvent, right: &AgentEvent) -> bool {
    matches!(
        (left, right),
        (AgentEvent::TextDelta { .. }, AgentEvent::TextEnd { .. })
            | (
                AgentEvent::ThinkingDelta { .. },
                AgentEvent::ThinkingEnd { .. }
            )
            | (
                _,
                AgentEvent::TurnEnd { .. }
                    | AgentEvent::SessionComplete { .. }
                    | AgentEvent::AgentEnd { .. }
            )
    )
}

//! `Conversation` — multi-turn builder + built-in tool-use loop.
//!
//! Consumers register handlers via the [`Tool`](crate::tool::Tool)
//! trait, configure budget and exhaustion behaviour, then call
//! [`Conversation::run`]. The loop iterates
//! `complete -> tool_calls? -> dispatch -> tool_results -> complete`
//! until the agent stops calling tools or the iteration budget is
//! exhausted.
//!
//! Live event observation is via the [`EventSink`] chain attached to
//! the driving [`LlmClient`] (e.g.
//! [`AnthropicClient::with_event_sink`](crate::client::AnthropicClient::with_event_sink))
//! — `Conversation` does not expose a separate streaming entry point.
//! Consumers that want a `Stream<Item = AgentEvent>` can wrap a
//! short mpsc-backed `EventSink` impl.

use displaydoc::Display;
#[cfg(test)]
use loom_events::DriverKind;
use loom_events::event::Source;
use loom_events::identifier::{SessionId, ToolCallId as EventToolCallId};
use loom_events::{
    AgentEvent, DriverEventPayload, EnvelopeBuilder, EventSink, SessionCommand, SessionScope,
};
use thiserror::Error;

use crate::cache::CacheControl;
use crate::client::{CompletionResponse, LlmClient, LlmError, ToolCallId as LlmToolCallId};
use crate::model_id::ModelId;
#[cfg(test)]
use crate::model_id::SchemaKind;
use crate::observer::result_hasher::{ResultFingerprint, ResultHasher};
use crate::observer::{
    DoomLoopConfig, DoomLoopObserver, DuplicateResultConfig, DuplicateResultObserver,
};
use crate::request::{CompletionRequest, Message, MessageContent, Role};
use crate::tool::{Tool, ToolDef};

/// Behaviour selected by the consumer when the iteration budget is
/// exhausted. Default is [`LoopOutcome::Error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoopOutcome {
    /// Return [`ConversationError::IterationBudgetExhausted`].
    #[default]
    Error,
    /// Return the last `CompletionResponse` produced before the cap.
    ReturnLast,
}

/// Byte budget for a request assembled from a [`Conversation`]. Pinned
/// history is retained even when it exceeds the budget; ordinary history
/// is trimmed at turn boundaries before pinned messages are removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    max_request_bytes: usize,
}

impl ContextBudget {
    /// Construct a budget measured against the estimated request byte
    /// size before provider-specific transport encoding.
    pub const fn max_request_bytes(max_request_bytes: usize) -> Self {
        Self { max_request_bytes }
    }

    /// Configured request byte budget.
    pub const fn max_request_bytes_value(self) -> usize {
        self.max_request_bytes
    }
}

/// Loop-control errors [`Conversation::run`] surfaces above and beyond
/// transport-level [`LlmError`]s. The transport surface stays purely
/// classification-oriented; loop-budget, tool-registry, observer-abort,
/// and tool-result-serialisation failures are loop concerns and live
/// here. `#[non_exhaustive]` so future loop-control variants land
/// additively.
#[non_exhaustive]
#[derive(Debug, Display, Error)]
pub enum ConversationError {
    /// underlying LLM transport failed
    Llm(#[from] LlmError),
    /// conversation iteration budget exhausted after {budget} iterations
    IterationBudgetExhausted {
        /// Cap that was hit. Mirrors
        /// [`Conversation::max_iterations`].
        budget: u32,
    },
    /// model called unregistered tool: {name}
    ToolNotRegistered {
        /// Name of the tool the model asked to invoke.
        name: String,
    },
    /// observer requested session abort: {reason}
    ObserverAbort {
        /// Reason supplied by the observer that emitted
        /// `loom_events::SessionCommand::Abort`. The
        /// `DoomLoopObserver`'s stage-2 reason is `"doom-loop: <tool>"`;
        /// other observers (consumer-supplied) format their own.
        reason: String,
    },
    /// failed to serialise tool result to JSON
    SerializeToolResult(#[from] serde_json::Error),
}

/// Multi-turn conversation with built-in tool-use loop.
pub struct Conversation {
    model: ModelId,
    system: Option<String>,
    tools: Vec<Box<dyn Tool>>,
    max_iterations: u32,
    on_iteration_exhausted: LoopOutcome,
    history: Vec<Message>,
    pinned_history_len: usize,
    context_budget: Option<ContextBudget>,
    doom_loop: Option<DoomLoopObserver>,
    duplicate_result: Option<DuplicateResultObserver>,
    envelope_builder: Option<EnvelopeBuilder>,
    pending_steers: Vec<String>,
}

impl Conversation {
    /// Construct a new conversation rooted at the named model. Per-call
    /// model override happens by re-issuing requests with a different
    /// `ModelId`; the conversation's `ModelId` is the default for
    /// `complete` calls the loop emits.
    ///
    /// Both default observers (`DoomLoopObserver`,
    /// `DuplicateResultObserver`) are constructed into the conversation's
    /// sink chain via `*Config::default()`. Callers that want to consume
    /// the binary's `LoomConfig` should use
    /// [`Conversation::with_observer_configs`] instead; per-conversation
    /// overrides land via [`Conversation::doom_loop`] /
    /// [`Conversation::duplicate_result`] /
    /// [`Conversation::doom_loop_disabled`] /
    /// [`Conversation::duplicate_result_disabled`].
    pub fn new(model: ModelId) -> Self {
        Self::with_observer_configs(
            model,
            DoomLoopConfig::default(),
            DuplicateResultConfig::default(),
        )
    }

    /// Construct a conversation with explicit observer configs sourced
    /// from the binary's `LoomConfig` (or any equivalent consumer-side
    /// config). Disabled observers (`enabled = false`) are not added to
    /// the sink chain.
    pub fn with_observer_configs(
        model: ModelId,
        doom_loop: DoomLoopConfig,
        duplicate_result: DuplicateResultConfig,
    ) -> Self {
        Self {
            model,
            system: None,
            tools: Vec::new(),
            max_iterations: 50,
            on_iteration_exhausted: LoopOutcome::Error,
            history: Vec::new(),
            pinned_history_len: 0,
            context_budget: None,
            doom_loop: build_doom_loop_observer(&doom_loop),
            duplicate_result: build_duplicate_result_observer(&duplicate_result),
            envelope_builder: Some(default_envelope_builder()),
            pending_steers: Vec::new(),
        }
    }

    /// Set the system instruction prefix the loop carries on every
    /// underlying `complete` call.
    pub fn system(mut self, prefix: impl Into<String>) -> Self {
        self.system = Some(prefix.into());
        self
    }

    /// Register a tool handler. Order of registration is preserved; the
    /// loop dispatches by tool name on each model-issued `tool_use`.
    pub fn register(mut self, tool: impl Tool + 'static) -> Self {
        self.tools.push(Box::new(tool));
        self
    }

    /// Register a pre-boxed tool handler. Same semantics as
    /// [`Conversation::register`]; used when the handler is already a
    /// `Box<dyn Tool>` (e.g. tools constructed from a `Vec<Box<dyn
    /// Tool>>` registry like `loom-direct-runner`'s six-tool set).
    pub fn register_boxed(mut self, tool: Box<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Cap iterations the loop runs before applying
    /// [`Conversation::on_iteration_exhausted`].
    pub fn max_iterations(mut self, n: u32) -> Self {
        self.max_iterations = n;
        self
    }

    /// Behaviour when the iteration cap is hit without the agent
    /// stopping.
    pub fn on_iteration_exhausted(mut self, outcome: LoopOutcome) -> Self {
        self.on_iteration_exhausted = outcome;
        self
    }

    /// Apply request-context budgeting when building provider calls.
    /// Pinned history stays verbatim; ordinary history is retained from
    /// the newest turn backwards until the budget is filled.
    pub fn context_budget(mut self, budget: ContextBudget) -> Self {
        self.context_budget = Some(budget);
        self
    }

    /// Replace the default `DoomLoopObserver` with one built from
    /// `config`. When `config.enabled` is false the observer is dropped
    /// from the sink chain entirely, matching the binary-side
    /// `[agent.doom_loop] enabled = false` knob.
    pub fn doom_loop(mut self, config: DoomLoopConfig) -> Self {
        self.doom_loop = build_doom_loop_observer(&config);
        self
    }

    /// Drop the default `DoomLoopObserver` from this conversation's sink
    /// chain. Mirrors `[agent.doom_loop] enabled = false` for callers
    /// that only need to opt out.
    pub fn doom_loop_disabled(mut self) -> Self {
        self.doom_loop = None;
        self
    }

    /// Replace the default `DuplicateResultObserver` with one built from
    /// `config`. When `config.enabled` is false the observer is dropped
    /// from the sink chain entirely, matching the binary-side
    /// `[agent.duplicate_result] enabled = false` knob.
    pub fn duplicate_result(mut self, config: DuplicateResultConfig) -> Self {
        self.duplicate_result = build_duplicate_result_observer(&config);
        self
    }

    /// Drop the default `DuplicateResultObserver` from this
    /// conversation's sink chain. Mirrors
    /// `[agent.duplicate_result] enabled = false`.
    pub fn duplicate_result_disabled(mut self) -> Self {
        self.duplicate_result = None;
        self
    }

    /// Replace the conversation's `EnvelopeBuilder`. External consumers
    /// thread their own event-session scope through this hook so the
    /// observer `AgentEvent::ToolCall` events carry the right metadata.
    /// Without an override the conversation uses a standalone session id
    /// with a constant `ts_ms = 0`; observers key on `(CallKey,
    /// ResultHash)` rather than the timestamp, so the default is
    /// observationally inert.
    pub fn with_envelope_builder(mut self, envelope_builder: EnvelopeBuilder) -> Self {
        self.envelope_builder = Some(envelope_builder);
        self
    }

    /// Append a user turn to the conversation history. Subsequent
    /// [`Conversation::run`] calls include it on the next completion.
    pub fn user(&mut self, content: impl Into<String>) {
        self.history.push(Message::user(content));
    }

    /// Append a user turn carrying a per-block cache marker. Providers
    /// that support typed prompt-cache breakpoints (Anthropic today)
    /// honour the marker; providers without typed-cache support no-op
    /// it without error.
    pub fn user_cached(&mut self, content: impl Into<String>, cache: CacheControl) {
        self.history.push(Message::user_cached(content, cache));
    }

    /// Append the initial rendered prompt as pinned user history. Direct
    /// runners use this for the phase prompt so context-budget trimming
    /// removes ordinary transcript turns before instruction text.
    pub fn user_cached_pinned(&mut self, content: impl Into<String>, cache: CacheControl) {
        self.history.push(Message::user_cached(content, cache));
        self.pinned_history_len = self.history.len();
    }

    /// Run the tool-use loop to completion against `client`. Returns
    /// the final `CompletionResponse` (the assistant turn that did not
    /// emit further tool calls).
    ///
    /// Each dispatched tool call is recorded by the composed observers;
    /// each tool result is fingerprinted once and the shared fingerprint
    /// is fanned into `DoomLoopObserver` / `DuplicateResultObserver`.
    /// After every non-streaming observation the loop drains `react()` on
    /// each composed observer: `SessionCommand::Steer` payloads are
    /// queued as user messages on the next iteration; the first
    /// `SessionCommand::Abort` short-circuits the loop and returns
    /// [`ConversationError::ObserverAbort`].
    pub async fn run<C: LlmClient + Sync + ?Sized>(
        &mut self,
        client: &C,
    ) -> Result<CompletionResponse, ConversationError> {
        let tool_defs: Vec<ToolDef> = self
            .tools
            .iter()
            .map(|tool| ToolDef::from_tool(tool.as_ref()))
            .collect();

        let mut last_response: Option<CompletionResponse> = None;
        let mut iterations: u32 = 0;
        while iterations < self.max_iterations {
            iterations += 1;
            for steer in self.pending_steers.drain(..) {
                self.history.push(Message::user(steer));
            }
            let req = self.build_request(tool_defs.clone());
            let response = client.complete(req).await?;

            if response.tool_calls.is_empty() {
                return Ok(response);
            }

            self.history.push(Message::assistant_tool_use(
                response.text.clone(),
                response.tool_calls.clone(),
            ));

            for call in &response.tool_calls {
                self.observe_tool_call(&call.call_id, &call.name, &call.args);
                if let Some(reason) = self.process_observer_updates(client) {
                    return Err(ConversationError::ObserverAbort { reason });
                }

                let tool = self
                    .tools
                    .iter()
                    .find(|t| t.name() == call.name)
                    .ok_or_else(|| ConversationError::ToolNotRegistered {
                        name: call.name.clone(),
                    })?;
                let output = tool.invoke(call.args.clone()).await?;
                let fingerprint = ResultHasher::result_fingerprint(&output.content);
                let content = serde_json::to_string(&output.content)?;
                self.history.push(Message::tool_result(
                    call.call_id.clone(),
                    content,
                    output.is_error,
                ));

                self.observe_tool_result(&call.call_id, fingerprint);
                if let Some(reason) = self.process_observer_updates(client) {
                    return Err(ConversationError::ObserverAbort { reason });
                }
            }

            last_response = Some(response);
        }

        match self.on_iteration_exhausted {
            LoopOutcome::Error => Err(ConversationError::IterationBudgetExhausted {
                budget: self.max_iterations,
            }),
            LoopOutcome::ReturnLast => {
                last_response.ok_or(ConversationError::IterationBudgetExhausted {
                    budget: self.max_iterations,
                })
            }
        }
    }

    fn observe_tool_call(
        &mut self,
        call_id: &LlmToolCallId,
        tool: &str,
        params: &serde_json::Value,
    ) {
        let Some(envelope) = self.next_observer_envelope() else {
            return;
        };
        let event = AgentEvent::ToolCall {
            envelope,
            id: EventToolCallId::new(call_id.as_str()),
            tool: tool.to_owned(),
            params: params.clone(),
            parent_tool_call_id: None,
        };
        if let Some(observer) = self.doom_loop.as_mut() {
            observer.emit(&event);
        }
        if let Some(observer) = self.duplicate_result.as_mut() {
            observer.emit(&event);
        }
    }

    fn observe_tool_result(&mut self, call_id: &LlmToolCallId, fingerprint: ResultFingerprint) {
        if self.doom_loop.is_none() && self.duplicate_result.is_none() {
            return;
        }
        let event_id = EventToolCallId::new(call_id.as_str());
        if let Some(observer) = self.doom_loop.as_mut() {
            observer.observe_tool_result(&event_id, fingerprint);
        }
        if let Some(observer) = self.duplicate_result.as_mut() {
            observer.observe_tool_result(&event_id, fingerprint);
        }
    }

    fn next_observer_envelope(&mut self) -> Option<loom_events::EventEnvelope> {
        if self.doom_loop.is_none() && self.duplicate_result.is_none() {
            return None;
        }
        self.envelope_builder
            .as_mut()
            .map(|builder| builder.build_with_source(Source::Agent))
    }

    fn process_observer_updates<C: LlmClient + ?Sized>(&mut self, client: &C) -> Option<String> {
        self.emit_pending_observer_events(client);
        self.process_react_commands()
    }

    fn emit_pending_observer_events<C: LlmClient + ?Sized>(&mut self, client: &C) {
        if let Some(observer) = self.doom_loop.as_mut() {
            let tripped = observer.take_pending();
            for trip in tripped {
                let event = self.doom_loop_driver_event(trip);
                client.emit_driver_event(event);
            }
        }
        if let Some(observer) = self.duplicate_result.as_mut() {
            let detections = observer.take_pending();
            for detection in detections {
                let event = self.duplicate_result_driver_event(detection);
                client.emit_driver_event(event);
            }
        }
    }

    fn doom_loop_driver_event(
        &self,
        trip: crate::observer::doom_loop::DoomLoopTripped,
    ) -> DriverEventPayload {
        DriverEventPayload::doom_loop_tripped(
            trip.stage.as_u8(),
            trip.tool,
            trip.params,
            trip.call_id.as_str(),
        )
    }

    fn duplicate_result_driver_event(
        &self,
        detection: crate::observer::duplicate_result::DuplicateDetection,
    ) -> DriverEventPayload {
        DriverEventPayload::duplicate_tool_result(
            detection.original_call_id.as_str(),
            detection.repeated_call_id.as_str(),
            detection.bytes_wasted,
        )
    }

    /// Drain composed observers' `react()` queues in registration order.
    /// `Steer` payloads land in `pending_steers` for injection on the
    /// next iteration. Returns `Some(reason)` on the first `Abort`; the
    /// caller short-circuits the loop with `ConversationError::ObserverAbort`.
    /// Mirrors `loom-workflow`'s `classify_react_commands` priority rule
    /// (Abort is terminal; subsequent commands in the same batch are
    /// dropped).
    fn process_react_commands(&mut self) -> Option<String> {
        let mut commands: Vec<SessionCommand> = Vec::new();
        if let Some(observer) = self.doom_loop.as_mut() {
            commands.extend(observer.react());
        }
        if let Some(observer) = self.duplicate_result.as_mut() {
            commands.extend(observer.react());
        }
        for cmd in commands {
            match cmd {
                SessionCommand::Steer(msg) => self.pending_steers.push(msg),
                SessionCommand::Abort(reason) => return Some(reason),
            }
        }
        None
    }

    /// Read-only view of the conversation's current `ModelId`.
    pub fn model(&self) -> &ModelId {
        &self.model
    }

    /// Read-only view of the iteration budget.
    pub fn max_iterations_value(&self) -> u32 {
        self.max_iterations
    }

    /// Read-only view of the exhaustion behaviour.
    pub fn on_iteration_exhausted_value(&self) -> LoopOutcome {
        self.on_iteration_exhausted
    }

    /// Whether the default `DoomLoopObserver` is composed in this
    /// conversation's sink chain.
    pub fn doom_loop_enabled(&self) -> bool {
        self.doom_loop.is_some()
    }

    /// Whether the default `DuplicateResultObserver` is composed in this
    /// conversation's sink chain.
    pub fn duplicate_result_enabled(&self) -> bool {
        self.duplicate_result.is_some()
    }

    /// Borrow the composed `DoomLoopObserver`, or `None` when the
    /// observer is disabled by config.
    pub fn doom_loop_observer(&self) -> Option<&DoomLoopObserver> {
        self.doom_loop.as_ref()
    }

    /// Borrow the composed `DuplicateResultObserver`, or `None` when the
    /// observer is disabled by config.
    pub fn duplicate_result_observer(&self) -> Option<&DuplicateResultObserver> {
        self.duplicate_result.as_ref()
    }

    /// Total number of messages currently in the conversation history.
    /// Callers snapshot this before [`Conversation::run`] and pass the
    /// snapshot to [`Conversation::history_since`] afterwards to read
    /// only the turns the loop appended.
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// Borrow the slice of history messages appended at or after `from`.
    /// `from` is typically a value returned by [`Conversation::history_len`]
    /// before [`Conversation::run`] was driven, so the slice captures one
    /// run's transcript without aliasing earlier turns.
    pub fn history_since(&self, from: usize) -> &[Message] {
        let from = from.min(self.history.len());
        &self.history[from..]
    }

    fn build_request(&self, tool_defs: Vec<ToolDef>) -> CompletionRequest {
        let mut req = CompletionRequest::new(self.model.clone());
        if let Some(prefix) = &self.system {
            req = req.system(prefix.clone());
        }
        for message in self.history_for_request(&tool_defs) {
            req = req.message(message);
        }
        if !tool_defs.is_empty() {
            req = req.tools(tool_defs);
        }
        req
    }

    fn history_for_request(&self, tool_defs: &[ToolDef]) -> Vec<Message> {
        let Some(budget) = self.context_budget else {
            return self.history.clone();
        };
        let pinned_len = self.pinned_history_len.min(self.history.len());
        let chunks = ordinary_history_chunks(&self.history, pinned_len);
        let Some(newest_chunk) = chunks.last() else {
            return self.history.clone();
        };

        let pinned_bytes = estimate_messages_bytes(&self.history[..pinned_len]);
        let suffix_bytes = estimate_messages_bytes(&self.history[newest_chunk.start..]);
        let mut used = self
            .static_request_bytes(tool_defs)
            .saturating_add(pinned_bytes)
            .saturating_add(suffix_bytes);
        let mut selected_start = newest_chunk.start;
        let prior_chunk_count = chunks.len().saturating_sub(1);

        for chunk in chunks[..prior_chunk_count].iter().rev() {
            let chunk_bytes = estimate_messages_bytes(&self.history[chunk.start..chunk.end]);
            if used.saturating_add(chunk_bytes) > budget.max_request_bytes {
                break;
            }
            used = used.saturating_add(chunk_bytes);
            selected_start = chunk.start;
        }

        let mut messages = Vec::with_capacity(pinned_len + self.history.len() - selected_start);
        messages.extend_from_slice(&self.history[..pinned_len]);
        messages.extend_from_slice(&self.history[selected_start..]);
        messages
    }

    fn static_request_bytes(&self, tool_defs: &[ToolDef]) -> usize {
        let system_bytes = self.system.as_ref().map_or(0, |system| system.len());
        tool_defs.iter().fold(system_bytes, |total, tool| {
            total.saturating_add(estimate_tool_def_bytes(tool))
        })
    }
}

fn ordinary_history_chunks(history: &[Message], pinned_len: usize) -> Vec<std::ops::Range<usize>> {
    let mut chunks = Vec::new();
    let mut start = pinned_len.min(history.len());
    if start >= history.len() {
        return chunks;
    }

    let mut index = start + 1;
    while index < history.len() {
        if history[index].role == Role::User {
            chunks.push(start..index);
            start = index;
        }
        index += 1;
    }
    chunks.push(start..history.len());
    chunks
}

fn estimate_messages_bytes(messages: &[Message]) -> usize {
    messages.iter().fold(0, |total, message| {
        total.saturating_add(estimate_message_bytes(message))
    })
}

fn estimate_message_bytes(message: &Message) -> usize {
    const MESSAGE_OVERHEAD_BYTES: usize = 32;
    const TOOL_CALL_OVERHEAD_BYTES: usize = 32;

    let content_bytes = message.content.iter().fold(0usize, |total, part| {
        total.saturating_add(estimate_content_bytes(part))
    });
    let tool_call_bytes = message.tool_calls.iter().fold(0usize, |total, call| {
        total
            .saturating_add(TOOL_CALL_OVERHEAD_BYTES)
            .saturating_add(call.call_id.as_str().len())
            .saturating_add(call.name.len())
            .saturating_add(call.args.to_string().len())
    });
    let tool_result_id_bytes = message
        .tool_call_id
        .as_ref()
        .map_or(0, |id| id.as_str().len());

    MESSAGE_OVERHEAD_BYTES
        .saturating_add(content_bytes)
        .saturating_add(tool_call_bytes)
        .saturating_add(tool_result_id_bytes)
}

fn estimate_content_bytes(part: &MessageContent) -> usize {
    match part {
        MessageContent::Text { text, .. } => text.len(),
        MessageContent::Binary { binary, .. } => binary
            .bytes
            .len()
            .saturating_add(binary.mime_type.as_str().len())
            .saturating_add(binary.name.as_ref().map_or(0, |name| name.len())),
    }
}

fn estimate_tool_def_bytes(tool: &ToolDef) -> usize {
    tool.name
        .len()
        .saturating_add(tool.description.len())
        .saturating_add(tool.input_schema.to_string().len())
}

/// Default `EnvelopeBuilder` for consumers that don't thread their own
/// event session through [`Conversation::with_envelope_builder`]. Uses a
/// standalone session scope and a constant `ts_ms = 0`. The composed
/// observers key on `(CallKey, ResultHash)`, not `ts_ms`, so the
/// constant clock is observationally inert; consumers that need
/// wall-clock timestamps on the synthesised events supply their own
/// builder via [`Conversation::with_envelope_builder`].
fn default_envelope_builder() -> EnvelopeBuilder {
    EnvelopeBuilder::new(
        SessionScope::phase(SessionId::new("conv-default"), None),
        Source::Agent,
        || 0,
    )
}

fn build_doom_loop_observer(config: &DoomLoopConfig) -> Option<DoomLoopObserver> {
    config
        .enabled
        .then(|| DoomLoopObserver::from_config(config))
}

fn build_duplicate_result_observer(
    config: &DuplicateResultConfig,
) -> Option<DuplicateResultObserver> {
    config
        .enabled
        .then(|| DuplicateResultObserver::from_config(config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{CompletionResponse, ToolUseRequest};
    use crate::model_id::AnthropicModel;
    use crate::request::Role;
    use crate::tool::{InvokeFuture, Tool, ToolOutput};
    use crate::usage::TokenUsage;

    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};

    struct EchoTool {
        seen: Arc<Mutex<Vec<Value>>>,
    }

    impl EchoTool {
        fn new() -> (Self, Arc<Mutex<Vec<Value>>>) {
            let seen = Arc::new(Mutex::new(Vec::new()));
            (Self { seen: seen.clone() }, seen)
        }
    }

    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "echo input"
        }
        fn input_schema(&self) -> Value {
            json!({ "type": "object" })
        }
        fn invoke<'a>(&'a self, args: Value) -> InvokeFuture<'a> {
            let seen = self.seen.clone();
            Box::pin(async move {
                seen.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(args.clone());
                Ok(ToolOutput {
                    content: json!({ "echoed": args }),
                    is_error: false,
                })
            })
        }
    }

    /// Scripted client that returns each scripted response in order so
    /// the loop test can drive multi-iteration flows without a live
    /// provider.
    type CallCount = Arc<Mutex<u32>>;
    type RecordedEvents = Arc<Mutex<Vec<AgentEvent>>>;

    struct ScriptedClient {
        responses: Mutex<Vec<CompletionResponse>>,
        calls: CallCount,
        events: RecordedEvents,
    }

    impl ScriptedClient {
        fn new(responses: Vec<CompletionResponse>) -> (Self, CallCount) {
            let (client, calls, _events) = Self::new_recording(responses);
            (client, calls)
        }

        fn new_recording(responses: Vec<CompletionResponse>) -> (Self, CallCount, RecordedEvents) {
            let calls = Arc::new(Mutex::new(0));
            let events = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    responses: Mutex::new(responses),
                    calls: calls.clone(),
                    events: events.clone(),
                },
                calls,
                events,
            )
        }
    }

    impl LlmClient for ScriptedClient {
        fn schema(&self) -> SchemaKind {
            SchemaKind::Anthropic
        }

        fn supports(&self, _model: &ModelId) -> bool {
            true
        }

        fn emit_driver_event(&self, event: DriverEventPayload) {
            let mut guard = self.events.lock().unwrap_or_else(|p| p.into_inner());
            let envelope = loom_events::EventEnvelope {
                session_id: SessionId::new("scripted-client"),
                bead_id: None,
                molecule_id: None,
                iteration: None,
                source: Source::Driver,
                ts_ms: 0,
                seq: guard.len() as u64,
            };
            guard.push(AgentEvent::from_driver_event(event, envelope));
        }

        fn emit_event(&self, event: &AgentEvent) {
            self.events
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(event.clone());
        }

        fn complete<'a>(
            &'a self,
            _req: CompletionRequest,
        ) -> crate::client::BoxFuture<'a, Result<CompletionResponse, LlmError>> {
            Box::pin(async move {
                *self.calls.lock().unwrap_or_else(|p| p.into_inner()) += 1;
                let mut guard = self.responses.lock().unwrap_or_else(|p| p.into_inner());
                if guard.is_empty() {
                    Err(LlmError::Provider {
                        message: "scripted client out of responses".into(),
                    })
                } else {
                    Ok(guard.remove(0))
                }
            })
        }

        fn complete_structured_raw<'a>(
            &'a self,
            _req: CompletionRequest,
            _schema: serde_json::Value,
            _type_name: String,
        ) -> crate::client::BoxFuture<'a, Result<String, LlmError>> {
            Box::pin(async move {
                Err(LlmError::Provider {
                    message: "complete_structured not exercised in conversation tests".into(),
                })
            })
        }
    }

    struct CapturingClient {
        captured: Arc<Mutex<Vec<CompletionRequest>>>,
        response: CompletionResponse,
    }

    impl CapturingClient {
        fn new(response: CompletionResponse) -> (Self, Arc<Mutex<Vec<CompletionRequest>>>) {
            let captured = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    captured: captured.clone(),
                    response,
                },
                captured,
            )
        }
    }

    impl LlmClient for CapturingClient {
        fn schema(&self) -> SchemaKind {
            SchemaKind::Anthropic
        }

        fn supports(&self, _model: &ModelId) -> bool {
            true
        }

        fn complete<'a>(
            &'a self,
            req: CompletionRequest,
        ) -> crate::client::BoxFuture<'a, Result<CompletionResponse, LlmError>> {
            Box::pin(async move {
                self.captured
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(req);
                Ok(self.response.clone())
            })
        }

        fn complete_structured_raw<'a>(
            &'a self,
            _req: CompletionRequest,
            _schema: serde_json::Value,
            _type_name: String,
        ) -> crate::client::BoxFuture<'a, Result<String, LlmError>> {
            Box::pin(async move {
                Err(LlmError::Provider {
                    message: "complete_structured not exercised in conversation tests".into(),
                })
            })
        }
    }

    fn no_calls(text: &str) -> CompletionResponse {
        CompletionResponse {
            text: text.to_string(),
            usage: TokenUsage::default(),
            tool_calls: Vec::new(),
        }
    }

    fn with_call(name: &str, call_id: &str, args: Value) -> CompletionResponse {
        CompletionResponse {
            text: String::new(),
            usage: TokenUsage::default(),
            tool_calls: vec![ToolUseRequest {
                call_id: LlmToolCallId::parse(call_id).expect("test tool call id parses"),
                name: name.to_string(),
                args,
            }],
        }
    }

    /// Context budgeting keeps the pinned initial prompt and newest
    /// active turn while trimming older ordinary history first. This
    /// pins the `loom-llm` seam Direct uses for its hard-limit fallback.
    #[test]
    fn conversation_context_budget_trims_ordinary_history_before_pinned_prompt() {
        let (client, captured) = CapturingClient::new(no_calls("done"));
        let mut conv = Conversation::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .context_budget(ContextBudget::max_request_bytes(1));
        conv.user_cached_pinned("PINNED INSTRUCTIONS", CacheControl::None);
        conv.user("ordinary history should be dropped");
        conv.user("current active turn");

        tokio_test::block_on(conv.run(&client)).expect("run completes");

        let requests = captured.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(requests.len(), 1);
        let texts: Vec<String> = requests[0]
            .messages
            .iter()
            .map(Message::text_content)
            .collect();
        assert_eq!(texts, vec!["PINNED INSTRUCTIONS", "current active turn"]);
    }

    /// `Conversation::new(ModelId)` returns a builder that accepts the
    /// documented knobs (`system`, `register`, `max_iterations`,
    /// `on_iteration_exhausted`) and persists each setting on the
    /// resulting `Conversation` so the loop reads back the same values
    /// the consumer wrote.
    #[test]
    fn conversation_builder_accepts_documented_knobs() {
        let (tool, _seen) = EchoTool::new();
        let mut conv = Conversation::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .system("be terse")
            .register(tool)
            .max_iterations(7)
            .on_iteration_exhausted(LoopOutcome::ReturnLast);
        conv.user("ping");

        assert_eq!(
            *conv.model(),
            ModelId::Anthropic(AnthropicModel::ClaudeSonnet46)
        );
        assert_eq!(conv.max_iterations_value(), 7);
        assert_eq!(conv.on_iteration_exhausted_value(), LoopOutcome::ReturnLast);
        assert_eq!(conv.history.len(), 1);
        assert_eq!(conv.history[0].role, Role::User);
        assert_eq!(conv.history[0].text_content(), "ping");
        assert_eq!(conv.tools.len(), 1);
        assert_eq!(conv.tools[0].name(), "echo");
        assert_eq!(conv.system.as_deref(), Some("be terse"));
    }

    /// The loop dispatches each tool call the model emits, reflects the
    /// tool result back into the next request, and stops at the first
    /// non-tool-calling assistant turn. The returned response is that
    /// final turn.
    #[test]
    fn conversation_run_completes_loop_and_returns_final_response() {
        let (tool, seen) = EchoTool::new();
        let (client, calls) = ScriptedClient::new(vec![
            with_call("echo", "call-1", json!({ "text": "hello" })),
            no_calls("done"),
        ]);

        let mut conv =
            Conversation::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46)).register(tool);
        conv.user("do the thing");

        let resp = tokio_test::block_on(conv.run(&client)).expect("run completes");

        assert_eq!(resp.text, "done");
        assert!(resp.tool_calls.is_empty());
        assert_eq!(*calls.lock().unwrap_or_else(|p| p.into_inner()), 2);
        let seen_args = seen.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(seen_args, vec![json!({ "text": "hello" })]);

        let history = conv.history.clone();
        let roles: Vec<Role> = history.iter().map(|m| m.role).collect();
        assert_eq!(roles, vec![Role::User, Role::Assistant, Role::Tool]);
        assert_eq!(history[1].tool_calls.len(), 1);
        assert_eq!(history[1].tool_calls[0].name, "echo");
        assert_eq!(
            history[2].tool_call_id.as_ref().map(|id| id.as_str()),
            Some("call-1"),
        );
    }

    /// The loop honours `max_iterations`; an unending tool-call stream
    /// terminates with `IterationBudgetExhausted` when
    /// `on_iteration_exhausted` is the default `Error`.
    #[test]
    fn conversation_loop_respects_max_iterations() {
        let (tool, _seen) = EchoTool::new();
        let infinite =
            std::iter::repeat_with(|| with_call("echo", "call", json!({ "text": "again" })))
                .take(32)
                .collect();
        let (client, calls) = ScriptedClient::new(infinite);

        let mut conv = Conversation::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .register(tool)
            .max_iterations(3);
        conv.user("loop");

        let err = tokio_test::block_on(conv.run(&client)).expect_err("loop exhausts budget");
        match err {
            ConversationError::IterationBudgetExhausted { budget } => assert_eq!(budget, 3),
            other => panic!("expected IterationBudgetExhausted, got {other:?}"),
        }
        assert_eq!(*calls.lock().unwrap_or_else(|p| p.into_inner()), 3);
    }

    /// With `LoopOutcome::ReturnLast`, exhausting the budget returns
    /// the last response the loop saw rather than an error.
    #[test]
    fn conversation_loop_return_last_on_exhausted() {
        let (tool, _seen) = EchoTool::new();
        let (client, _calls) = ScriptedClient::new(vec![
            with_call("echo", "call-a", json!({ "text": "1" })),
            with_call("echo", "call-b", json!({ "text": "2" })),
        ]);

        let mut conv = Conversation::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .register(tool)
            .max_iterations(2)
            .on_iteration_exhausted(LoopOutcome::ReturnLast);
        conv.user("loop");

        let resp = tokio_test::block_on(conv.run(&client)).expect("returns last response");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].call_id.as_str(), "call-b");
    }

    /// The default `DuplicateResultObserver` ships enabled, and the
    /// builder's `duplicate_result_disabled` toggle takes effect
    /// — mirroring `[agent.duplicate_result] enabled = false` in the
    /// CLI-side config.
    #[test]
    fn duplicate_result_config_disable_path() {
        let on = Conversation::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46));
        assert!(on.duplicate_result_enabled());
        let off = Conversation::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .duplicate_result_disabled();
        assert!(!off.duplicate_result_enabled());
    }

    /// The default `DoomLoopObserver` ships enabled, and the builder's
    /// `doom_loop_disabled` toggle mirrors `[agent.doom_loop] enabled =
    /// false` in the CLI-side config.
    #[test]
    fn doom_loop_config_disable_path() {
        let on = Conversation::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46));
        assert!(on.doom_loop_enabled());
        let off = Conversation::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .doom_loop_disabled();
        assert!(!off.doom_loop_enabled());
    }

    /// `Conversation::new` default-constructs both observers into the
    /// sink chain with the spec defaults so consumer-driven
    /// `Conversation` runs get the safety nets out of the box.
    #[test]
    fn conversation_new_default_constructs_observers() {
        let conv = Conversation::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46));
        let doom = conv.doom_loop_observer().expect("doom loop composed");
        assert_eq!(doom.window(), 5);
        assert_eq!(doom.threshold(), 3);
        assert_eq!(doom.stage_2_after_stage_1(), 3);
        let dup = conv
            .duplicate_result_observer()
            .expect("duplicate result composed");
        assert_eq!(
            dup.min_bytes(),
            crate::observer::duplicate_result::DEFAULT_MIN_BYTES
        );
    }

    /// `.doom_loop(config)` replaces the default observer with one built
    /// from the supplied knobs; `.duplicate_result(config)` does the same
    /// for the other observer.
    #[test]
    fn observer_builder_knobs_apply_custom_config() {
        let conv = Conversation::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .doom_loop(DoomLoopConfig {
                enabled: true,
                window: 8,
                threshold: 4,
                stage_2_after_stage_1: 2,
            })
            .duplicate_result(DuplicateResultConfig {
                enabled: true,
                min_bytes: 1024,
            });
        let doom = conv.doom_loop_observer().expect("doom loop composed");
        assert_eq!(doom.window(), 8);
        assert_eq!(doom.threshold(), 4);
        assert_eq!(doom.stage_2_after_stage_1(), 2);
        let dup = conv
            .duplicate_result_observer()
            .expect("duplicate result composed");
        assert_eq!(dup.min_bytes(), 1024);
    }

    /// A config with `enabled = false` drops the observer from the sink
    /// chain entirely — the same outcome as calling
    /// `.doom_loop_disabled()` / `.duplicate_result_disabled()`.
    #[test]
    fn observer_config_enabled_false_drops_observer() {
        let conv = Conversation::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .doom_loop(DoomLoopConfig {
                enabled: false,
                ..DoomLoopConfig::default()
            })
            .duplicate_result(DuplicateResultConfig {
                enabled: false,
                ..DuplicateResultConfig::default()
            });
        assert!(!conv.doom_loop_enabled());
        assert!(!conv.duplicate_result_enabled());
        assert!(conv.doom_loop_observer().is_none());
        assert!(conv.duplicate_result_observer().is_none());
    }

    /// `Conversation::with_observer_configs` is the constructor that
    /// reads from the binary's `LoomConfig` shape. Disabled observers
    /// (`enabled = false`) are not added — matching the bead's
    /// "Disabled observers (enabled = false) are not added" rule.
    #[test]
    fn with_observer_configs_honours_enabled_flags() {
        let conv = Conversation::with_observer_configs(
            ModelId::Anthropic(AnthropicModel::ClaudeSonnet46),
            DoomLoopConfig {
                enabled: false,
                ..DoomLoopConfig::default()
            },
            DuplicateResultConfig::default(),
        );
        assert!(!conv.doom_loop_enabled());
        assert!(conv.duplicate_result_enabled());
    }

    /// Dropping the loop future before it resolves cancels work at both
    /// await points the loop owns: an in-flight LLM completion and an
    /// in-flight tool invocation. Drop sentinels on the pending futures
    /// prove the child futures are actually dropped rather than merely
    /// left unreachable.
    #[test]
    fn conversation_loop_cancellation_aborts_in_flight_work() {
        struct PendingOnDrop<T> {
            dropped: Arc<std::sync::atomic::AtomicBool>,
            _output: std::marker::PhantomData<T>,
        }

        impl<T> PendingOnDrop<T> {
            fn new(dropped: Arc<std::sync::atomic::AtomicBool>) -> Self {
                Self {
                    dropped,
                    _output: std::marker::PhantomData,
                }
            }
        }

        impl<T> std::future::Future for PendingOnDrop<T> {
            type Output = T;

            fn poll(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Self::Output> {
                std::task::Poll::Pending
            }
        }

        impl<T> Drop for PendingOnDrop<T> {
            fn drop(&mut self) {
                self.dropped
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }

        struct PendingClient {
            dropped: Arc<std::sync::atomic::AtomicBool>,
        }

        impl LlmClient for PendingClient {
            fn schema(&self) -> SchemaKind {
                SchemaKind::Anthropic
            }

            fn complete<'a>(
                &'a self,
                _req: CompletionRequest,
            ) -> crate::client::BoxFuture<'a, Result<CompletionResponse, LlmError>> {
                Box::pin(PendingOnDrop::new(self.dropped.clone()))
            }

            fn complete_structured_raw<'a>(
                &'a self,
                _req: CompletionRequest,
                _schema: serde_json::Value,
                _type_name: String,
            ) -> crate::client::BoxFuture<'a, Result<String, LlmError>> {
                Box::pin(async move {
                    Err(LlmError::Provider {
                        message: "structured path not exercised".into(),
                    })
                })
            }
        }

        let llm_dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let llm_client = PendingClient {
            dropped: llm_dropped.clone(),
        };
        let mut llm_conv = Conversation::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46));
        llm_conv.user("hang in LLM");
        poll_once_then_drop(llm_conv.run(&llm_client));
        assert!(
            llm_dropped.load(std::sync::atomic::Ordering::SeqCst),
            "dropping Conversation::run must drop the in-flight LLM future",
        );

        struct PendingTool {
            dropped: Arc<std::sync::atomic::AtomicBool>,
        }
        impl Tool for PendingTool {
            fn name(&self) -> &str {
                "pending"
            }
            fn description(&self) -> &str {
                "never resolves"
            }
            fn input_schema(&self) -> Value {
                json!({ "type": "object" })
            }
            fn invoke<'a>(&'a self, _args: Value) -> InvokeFuture<'a> {
                Box::pin(PendingOnDrop::new(self.dropped.clone()))
            }
        }

        let tool_dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (client, calls) = ScriptedClient::new(vec![with_call("pending", "call-1", json!({}))]);
        let mut tool_conv = Conversation::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .register(PendingTool {
                dropped: tool_dropped.clone(),
            });
        tool_conv.user("hang in tool");

        poll_once_then_drop(tool_conv.run(&client));
        assert_eq!(*calls.lock().unwrap_or_else(|p| p.into_inner()), 1);
        assert!(
            tool_dropped.load(std::sync::atomic::Ordering::SeqCst),
            "dropping Conversation::run must drop the in-flight tool future",
        );
    }

    fn poll_once_then_drop<F>(future: F)
    where
        F: std::future::Future,
    {
        let mut fut = Box::pin(future);
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let poll = fut.as_mut().poll(&mut cx);
        assert!(matches!(poll, std::task::Poll::Pending));
        drop(fut);
    }

    /// A doom-loop scenario driven through `run` short-circuits with
    /// [`ConversationError::ObserverAbort`] once the composed `DoomLoopObserver`
    /// trips stage 2. This is the spec-promised behaviour for external
    /// consumers — the safety net fires automatically with no extra
    /// wiring from the caller.
    #[test]
    fn conversation_run_observer_abort_short_circuits_loop() {
        let (tool, _seen) = EchoTool::new();
        let identical = || with_call("echo", "call-loop", json!({ "text": "spin" }));
        let scripted = std::iter::repeat_with(identical).take(20).collect();
        let (client, calls) = ScriptedClient::new(scripted);

        let mut conv = Conversation::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .register(tool)
            .doom_loop(DoomLoopConfig {
                enabled: true,
                window: 5,
                threshold: 3,
                stage_2_after_stage_1: 1,
            })
            .max_iterations(20);
        conv.user("spin forever");

        let err = tokio_test::block_on(conv.run(&client)).expect_err("observer aborts loop");
        match err {
            ConversationError::ObserverAbort { reason } => {
                assert_eq!(reason, "doom-loop: echo");
            }
            other => panic!("expected ObserverAbort, got {other:?}"),
        }
        let observed = *calls.lock().unwrap_or_else(|p| p.into_inner());
        assert!(
            observed <= 4,
            "loop must short-circuit by iteration 4 (stage 1 at 3, stage 2 at 4); \
             saw {observed} completions",
        );
    }

    /// Stage 1 emits a live `DriverKind::DoomLoopTripped` event through
    /// the LLM client's sink path while still steering the next turn.
    #[test]
    fn doom_loop_stage_1_emits_driver_event() {
        let (tool, _seen) = EchoTool::new();
        let scripted = vec![
            with_call("echo", "stage1-a", json!({ "text": "spin" })),
            with_call("echo", "stage1-b", json!({ "text": "spin" })),
            with_call("echo", "stage1-c", json!({ "text": "spin" })),
            no_calls("done"),
        ];
        let (client, _calls, events) = ScriptedClient::new_recording(scripted);

        let mut conv = Conversation::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .register(tool)
            .doom_loop(DoomLoopConfig {
                enabled: true,
                window: 5,
                threshold: 3,
                stage_2_after_stage_1: 10,
            })
            .max_iterations(10);
        conv.user("spin forever");

        let resp = tokio_test::block_on(conv.run(&client)).expect("stage 1 only steers");
        assert_eq!(resp.text, "done");

        let recorded = events.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(recorded.len(), 1);
        match &recorded[0] {
            AgentEvent::DriverEvent {
                envelope,
                driver_kind,
                payload,
                ..
            } => {
                assert_eq!(*driver_kind, DriverKind::DoomLoopTripped);
                assert_eq!(envelope.source, Source::Driver);
                assert_eq!(payload["stage"], 1);
                assert_eq!(payload["tool"], "echo");
                assert_eq!(payload["call_id"], "stage1-c");
            }
            other => panic!("expected doom-loop driver event, got {other:?}"),
        }
    }

    /// Stage 2 emits the live `DriverKind::DoomLoopTripped` event before
    /// the observer abort short-circuits the loop.
    #[test]
    fn doom_loop_stage_2_emits_driver_event() {
        let (tool, _seen) = EchoTool::new();
        let scripted = vec![
            with_call("echo", "stage2-a", json!({ "text": "spin" })),
            with_call("echo", "stage2-b", json!({ "text": "spin" })),
            with_call("echo", "stage2-c", json!({ "text": "spin" })),
            with_call("echo", "stage2-d", json!({ "text": "spin" })),
        ];
        let (client, _calls, events) = ScriptedClient::new_recording(scripted);

        let mut conv = Conversation::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .register(tool)
            .doom_loop(DoomLoopConfig {
                enabled: true,
                window: 5,
                threshold: 3,
                stage_2_after_stage_1: 1,
            })
            .max_iterations(10);
        conv.user("spin forever");

        let err = tokio_test::block_on(conv.run(&client)).expect_err("stage 2 aborts");
        match err {
            ConversationError::ObserverAbort { reason } => assert_eq!(reason, "doom-loop: echo"),
            other => panic!("expected ObserverAbort, got {other:?}"),
        }

        let recorded = events.lock().unwrap_or_else(|p| p.into_inner());
        let stages: Vec<u64> = recorded
            .iter()
            .filter_map(|event| match event {
                AgentEvent::DriverEvent {
                    driver_kind,
                    payload,
                    ..
                } if *driver_kind == DriverKind::DoomLoopTripped => payload["stage"].as_u64(),
                _ => None,
            })
            .collect();
        assert_eq!(stages, vec![1, 2]);
        match recorded.last().expect("stage 2 event present") {
            AgentEvent::DriverEvent { payload, .. } => {
                assert_eq!(payload["call_id"], "stage2-d");
                assert_eq!(payload["tool"], "echo");
            }
            other => panic!("expected driver event, got {other:?}"),
        }
    }

    /// Duplicate-result observability emits a live
    /// `DriverKind::DuplicateToolResult` event whose payload carries the
    /// original call, repeated call, and canonical bytes wasted.
    #[test]
    fn duplicate_result_event_payload_carries_bytes_wasted() {
        let (tool, _seen) = EchoTool::new();
        let scripted = vec![
            with_call("echo", "dup-a", json!({ "text": "same" })),
            with_call("echo", "dup-b", json!({ "text": "same" })),
            no_calls("done"),
        ];
        let (client, _calls, events) = ScriptedClient::new_recording(scripted);

        let mut conv = Conversation::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .register(tool)
            .doom_loop_disabled()
            .duplicate_result(DuplicateResultConfig {
                enabled: true,
                min_bytes: 1,
            })
            .max_iterations(10);
        conv.user("fetch duplicates");

        tokio_test::block_on(conv.run(&client)).expect("duplicate observer is non-terminal");
        let expected_bytes = serde_json::to_string(&json!({ "echoed": { "text": "same" } }))
            .expect("fixture serializes")
            .len() as u64;

        let recorded = events.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(recorded.len(), 1);
        match &recorded[0] {
            AgentEvent::DriverEvent {
                envelope,
                driver_kind,
                payload,
                ..
            } => {
                assert_eq!(*driver_kind, DriverKind::DuplicateToolResult);
                assert_eq!(envelope.source, Source::Driver);
                assert_eq!(payload["original_call_id"], "dup-a");
                assert_eq!(payload["repeated_call_id"], "dup-b");
                assert_eq!(payload["bytes_wasted"], expected_bytes);
            }
            other => panic!("expected duplicate-result driver event, got {other:?}"),
        }
    }

    /// `SessionCommand::Steer` returned from a composed observer is
    /// queued and injected as a user message on the next iteration so
    /// the agent's next turn sees the nudge.
    #[test]
    fn conversation_run_steer_command_reaches_next_iteration() {
        let (tool, _seen) = EchoTool::new();
        let identical = || with_call("echo", "call-steer", json!({ "text": "stuck" }));
        let mut scripted: Vec<CompletionResponse> = (0..3).map(|_| identical()).collect();
        scripted.push(no_calls("done"));
        let (client, _calls) = ScriptedClient::new(scripted);

        let mut conv = Conversation::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .register(tool)
            .doom_loop(DoomLoopConfig {
                enabled: true,
                window: 5,
                threshold: 3,
                stage_2_after_stage_1: 10,
            })
            .max_iterations(10);
        conv.user("first user turn");

        let resp = tokio_test::block_on(conv.run(&client)).expect("run completes");
        assert!(resp.tool_calls.is_empty());

        let steer_turns: Vec<&Message> = conv
            .history
            .iter()
            .filter(|m| m.role == Role::User && m.text_content().contains("doom-loop suspected"))
            .collect();
        assert_eq!(
            steer_turns.len(),
            1,
            "exactly one steer must land as a user message in history",
        );
    }

    /// Disabling both observers via the builder means the run loop
    /// synthesises no `ToolCall` / `ToolResult` envelopes — confirmed
    /// indirectly here by driving an otherwise-doom-looping program past
    /// stage 2's threshold without triggering an abort.
    #[test]
    fn conversation_run_observers_disabled_skips_event_synthesis() {
        let (tool, _seen) = EchoTool::new();
        let identical = || with_call("echo", "call-noop", json!({ "text": "spin" }));
        let mut scripted: Vec<CompletionResponse> = (0..5).map(|_| identical()).collect();
        scripted.push(no_calls("done"));
        let (client, _calls) = ScriptedClient::new(scripted);

        let mut conv = Conversation::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .register(tool)
            .doom_loop_disabled()
            .duplicate_result_disabled()
            .max_iterations(10);
        conv.user("spin");

        let resp =
            tokio_test::block_on(conv.run(&client)).expect("disabled observers do not abort");
        assert_eq!(resp.text, "done");
    }
}

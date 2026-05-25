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
//! [`Client::with_event_sink`](crate::client::Client::with_event_sink)) —
//! `Conversation` does not expose a separate streaming entry point.
//! Consumers that want a `Stream<Item = AgentEvent>` can wrap a
//! short mpsc-backed `EventSink` impl.

use displaydoc::Display;
use loom_events::event::Source;
use loom_events::identifier::{BeadId, ParseBeadIdError, ToolCallId};
use loom_events::{AgentEvent, EnvelopeBuilder, EventSink, SessionCommand};
use thiserror::Error;

use crate::cache::CacheControl;
use crate::client::{CompletionResponse, LlmClient, LlmError};
use crate::model_id::ModelId;
use crate::observer::{
    DoomLoopConfig, DoomLoopObserver, DuplicateResultConfig, DuplicateResultObserver,
};
use crate::request::{CompletionRequest, Message};
use crate::tool::{Tool, ToolDef};

/// Behaviour selected by the consumer when the iteration budget is
/// exhausted. Default is [`LoopOutcome::Error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoopOutcome {
    /// Return [`LlmError::IterationBudgetExhausted`].
    #[default]
    Error,
    /// Return the last `CompletionResponse` produced before the cap.
    ReturnLast,
}

/// Failure modes for [`Conversation`] construction. Surfaced when an
/// internal invariant of the synthetic default state cannot be
/// established — for example, the hardcoded synthetic bead id used by
/// the default [`EnvelopeBuilder`] failing to parse.
#[derive(Debug, Display, Error)]
pub enum ConversationBuildError {
    /// failed to construct the synthetic default `EnvelopeBuilder`
    DefaultBead(#[from] ParseBeadIdError),
}

/// Multi-turn conversation with built-in tool-use loop.
pub struct Conversation {
    model: ModelId,
    system: Option<String>,
    tools: Vec<Box<dyn Tool>>,
    max_iterations: u32,
    on_iteration_exhausted: LoopOutcome,
    history: Vec<Message>,
    doom_loop: Option<DoomLoopObserver>,
    duplicate_result: Option<DuplicateResultObserver>,
    envelope_builder: EnvelopeBuilder,
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
    pub fn new(model: ModelId) -> Result<Self, ConversationBuildError> {
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
    ) -> Result<Self, ConversationBuildError> {
        Ok(Self {
            model,
            system: None,
            tools: Vec::new(),
            max_iterations: 50,
            on_iteration_exhausted: LoopOutcome::Error,
            history: Vec::new(),
            doom_loop: build_doom_loop_observer(&doom_loop),
            duplicate_result: build_duplicate_result_observer(&duplicate_result),
            envelope_builder: default_envelope_builder()?,
            pending_steers: Vec::new(),
        })
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
    /// thread their own bead / molecule / iteration identity through this
    /// hook so the synthetic `AgentEvent::ToolCall` / `AgentEvent::ToolResult`
    /// events the loop emits into composed observers carry the right
    /// per-spawn metadata. Without an override the conversation uses a
    /// synthetic `wx-conv` bead id with a constant `ts_ms = 0`;
    /// observers key on `(CallKey, ResultHash)` rather than the
    /// timestamp, so the default is observationally inert.
    pub fn with_envelope_builder(mut self, envelope_builder: EnvelopeBuilder) -> Self {
        self.envelope_builder = envelope_builder;
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

    /// Run the tool-use loop to completion against `client`. Returns
    /// the final `CompletionResponse` (the assistant turn that did not
    /// emit further tool calls).
    ///
    /// Each dispatched tool call synthesises an `AgentEvent::ToolCall` /
    /// `AgentEvent::ToolResult` pair that is fanned into the composed
    /// `DoomLoopObserver` / `DuplicateResultObserver`. After every
    /// non-streaming event the loop drains `react()` on each composed
    /// observer: `SessionCommand::Steer` payloads are queued as user
    /// messages on the next iteration; the first `SessionCommand::Abort`
    /// short-circuits the loop and returns
    /// [`LlmError::ObserverAbort`].
    pub async fn run<C: LlmClient + Sync>(
        &mut self,
        client: &C,
    ) -> Result<CompletionResponse, LlmError> {
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
                self.observe_tool_call(call.call_id.clone(), &call.name, &call.args);
                if let Some(reason) = self.process_react_commands() {
                    return Err(LlmError::ObserverAbort { reason });
                }

                let tool = self
                    .tools
                    .iter()
                    .find(|t| t.name() == call.name)
                    .ok_or_else(|| LlmError::ToolNotRegistered {
                        name: call.name.clone(),
                    })?;
                let output = tool.invoke(call.args.clone()).await?;
                let content = serde_json::to_string(&output.content)?;
                self.history.push(Message::tool_result(
                    call.call_id.clone(),
                    content.clone(),
                    output.is_error,
                ));

                self.observe_tool_result(call.call_id.clone(), &content, output.is_error);
                if let Some(reason) = self.process_react_commands() {
                    return Err(LlmError::ObserverAbort { reason });
                }
            }

            last_response = Some(response);
        }

        match self.on_iteration_exhausted {
            LoopOutcome::Error => Err(LlmError::IterationBudgetExhausted {
                budget: self.max_iterations,
            }),
            LoopOutcome::ReturnLast => last_response.ok_or(LlmError::IterationBudgetExhausted {
                budget: self.max_iterations,
            }),
        }
    }

    fn observe_tool_call(&mut self, call_id: String, tool: &str, params: &serde_json::Value) {
        if self.doom_loop.is_none() && self.duplicate_result.is_none() {
            return;
        }
        let event = AgentEvent::ToolCall {
            envelope: self.envelope_builder.build_with_source(Source::Agent),
            id: ToolCallId::new(call_id),
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

    fn observe_tool_result(&mut self, call_id: String, output: &str, is_error: bool) {
        if self.doom_loop.is_none() && self.duplicate_result.is_none() {
            return;
        }
        let event = AgentEvent::ToolResult {
            envelope: self.envelope_builder.build_with_source(Source::Agent),
            id: ToolCallId::new(call_id),
            output: output.to_owned(),
            is_error,
        };
        if let Some(observer) = self.doom_loop.as_mut() {
            observer.emit(&event);
        }
        if let Some(observer) = self.duplicate_result.as_mut() {
            observer.emit(&event);
        }
    }

    /// Drain composed observers' `react()` queues in registration order.
    /// `Steer` payloads land in `pending_steers` for injection on the
    /// next iteration. Returns `Some(reason)` on the first `Abort`; the
    /// caller short-circuits the loop with `LlmError::ObserverAbort`.
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
        for message in &self.history {
            req = req.message(message.clone());
        }
        if !tool_defs.is_empty() {
            req = req.tools(tool_defs);
        }
        req
    }
}

/// Synthetic `EnvelopeBuilder` for consumers that don't thread their own
/// bead identity through [`Conversation::with_envelope_builder`]. Uses
/// the constant `wx-conv` bead id (replay tools see it as a distinct
/// stream) and a constant `ts_ms = 0`. The composed observers key on
/// `(CallKey, ResultHash)`, not `ts_ms`, so the constant clock is
/// observationally inert; consumers that need wall-clock timestamps on
/// the synthesised events supply their own builder via
/// [`Conversation::with_envelope_builder`] (the only path that draws on
/// the dedicated `SystemClock` impls in `loom-driver` / `loom-render`).
fn default_envelope_builder() -> Result<EnvelopeBuilder, ConversationBuildError> {
    let bead = BeadId::new("wx-conv")?;
    Ok(EnvelopeBuilder::new(bead, None, 0, Source::Agent, || 0))
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
    struct ScriptedClient {
        responses: Mutex<Vec<CompletionResponse>>,
        calls: Arc<Mutex<u32>>,
    }

    impl ScriptedClient {
        fn new(responses: Vec<CompletionResponse>) -> (Self, Arc<Mutex<u32>>) {
            let calls = Arc::new(Mutex::new(0));
            (
                Self {
                    responses: Mutex::new(responses),
                    calls: calls.clone(),
                },
                calls,
            )
        }
    }

    impl LlmClient for ScriptedClient {
        async fn complete(&self, _req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
            *self.calls.lock().unwrap_or_else(|p| p.into_inner()) += 1;
            let mut guard = self.responses.lock().unwrap_or_else(|p| p.into_inner());
            if guard.is_empty() {
                Err(LlmError::Provider {
                    message: "scripted client out of responses".into(),
                })
            } else {
                Ok(guard.remove(0))
            }
        }

        async fn complete_structured<T>(&self, _req: CompletionRequest) -> Result<T, LlmError>
        where
            T: serde::de::DeserializeOwned + schemars::JsonSchema + Send,
        {
            Err(LlmError::Provider {
                message: "complete_structured not exercised in conversation tests".into(),
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
                call_id: call_id.to_string(),
                name: name.to_string(),
                args,
            }],
        }
    }

    /// `Conversation::new(ModelId)` returns a builder that accepts the
    /// documented knobs (`system`, `register`, `max_iterations`,
    /// `on_iteration_exhausted`) and persists each setting on the
    /// resulting `Conversation` so the loop reads back the same values
    /// the consumer wrote.
    #[test]
    fn conversation_builder_accepts_documented_knobs() {
        let (tool, _seen) = EchoTool::new();
        let mut conv = Conversation::new(ModelId::ClaudeSonnet46)
            .expect("conversation builds")
            .system("be terse")
            .register(tool)
            .max_iterations(7)
            .on_iteration_exhausted(LoopOutcome::ReturnLast);
        conv.user("ping");

        assert_eq!(*conv.model(), ModelId::ClaudeSonnet46);
        assert_eq!(conv.max_iterations_value(), 7);
        assert_eq!(conv.on_iteration_exhausted_value(), LoopOutcome::ReturnLast);
        assert_eq!(conv.history.len(), 1);
        assert_eq!(conv.history[0].role, Role::User);
        assert_eq!(conv.history[0].content, "ping");
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

        let mut conv = Conversation::new(ModelId::ClaudeSonnet46)
            .expect("conversation builds")
            .register(tool);
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
        assert_eq!(history[2].tool_call_id.as_deref(), Some("call-1"));
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

        let mut conv = Conversation::new(ModelId::ClaudeSonnet46)
            .expect("conversation builds")
            .register(tool)
            .max_iterations(3);
        conv.user("loop");

        let err = tokio_test::block_on(conv.run(&client)).expect_err("loop exhausts budget");
        match err {
            LlmError::IterationBudgetExhausted { budget } => assert_eq!(budget, 3),
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

        let mut conv = Conversation::new(ModelId::ClaudeSonnet46)
            .expect("conversation builds")
            .register(tool)
            .max_iterations(2)
            .on_iteration_exhausted(LoopOutcome::ReturnLast);
        conv.user("loop");

        let resp = tokio_test::block_on(conv.run(&client)).expect("returns last response");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].call_id, "call-b");
    }

    /// The default `DuplicateResultObserver` ships enabled, and the
    /// builder's `duplicate_result_disabled` toggle takes effect
    /// — mirroring `[agent.duplicate_result] enabled = false` in the
    /// CLI-side config.
    #[test]
    fn duplicate_result_config_disable_path() {
        let on = Conversation::new(ModelId::ClaudeSonnet46).expect("conversation builds");
        assert!(on.duplicate_result_enabled());
        let off = Conversation::new(ModelId::ClaudeSonnet46)
            .expect("conversation builds")
            .duplicate_result_disabled();
        assert!(!off.duplicate_result_enabled());
    }

    /// The default `DoomLoopObserver` ships enabled, and the builder's
    /// `doom_loop_disabled` toggle mirrors `[agent.doom_loop] enabled =
    /// false` in the CLI-side config.
    #[test]
    fn doom_loop_config_disable_path() {
        let on = Conversation::new(ModelId::ClaudeSonnet46).expect("conversation builds");
        assert!(on.doom_loop_enabled());
        let off = Conversation::new(ModelId::ClaudeSonnet46)
            .expect("conversation builds")
            .doom_loop_disabled();
        assert!(!off.doom_loop_enabled());
    }

    /// `Conversation::new` default-constructs both observers into the
    /// sink chain with the spec defaults so consumer-driven
    /// `Conversation` runs get the safety nets out of the box.
    #[test]
    fn conversation_new_default_constructs_observers() {
        let conv = Conversation::new(ModelId::ClaudeSonnet46).expect("conversation builds");
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
        let conv = Conversation::new(ModelId::ClaudeSonnet46)
            .expect("conversation builds")
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
        let conv = Conversation::new(ModelId::ClaudeSonnet46)
            .expect("conversation builds")
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
            ModelId::ClaudeSonnet46,
            DoomLoopConfig {
                enabled: false,
                ..DoomLoopConfig::default()
            },
            DuplicateResultConfig::default(),
        )
        .expect("conversation builds");
        assert!(!conv.doom_loop_enabled());
        assert!(conv.duplicate_result_enabled());
    }

    /// Dropping the loop future before it resolves cancels any work
    /// awaiting inside the loop — this is the standard tokio
    /// drop-cancels-future semantics, but the conversation must not
    /// install any guards that fight it. Pinning a future that awaits a
    /// pending tool dispatch and dropping it after one poll proves the
    /// invariant.
    #[test]
    fn conversation_loop_cancellation_aborts_in_flight_work() {
        struct PendingTool;
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
                Box::pin(std::future::pending())
            }
        }

        let (client, calls) = ScriptedClient::new(vec![with_call("pending", "call-1", json!({}))]);

        let mut conv = Conversation::new(ModelId::ClaudeSonnet46)
            .expect("conversation builds")
            .register(PendingTool);
        conv.user("hang");

        let mut fut = Box::pin(conv.run(&client));
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let poll = fut.as_mut().poll(&mut cx);
        assert!(matches!(poll, std::task::Poll::Pending));
        drop(fut);
        assert_eq!(*calls.lock().unwrap_or_else(|p| p.into_inner()), 1);
    }

    /// A doom-loop scenario driven through `run` short-circuits with
    /// [`LlmError::ObserverAbort`] once the composed `DoomLoopObserver`
    /// trips stage 2. This is the spec-promised behaviour for external
    /// consumers — the safety net fires automatically with no extra
    /// wiring from the caller.
    #[test]
    fn conversation_run_observer_abort_short_circuits_loop() {
        let (tool, _seen) = EchoTool::new();
        let identical = || with_call("echo", "call-loop", json!({ "text": "spin" }));
        let scripted = std::iter::repeat_with(identical).take(20).collect();
        let (client, calls) = ScriptedClient::new(scripted);

        let mut conv = Conversation::new(ModelId::ClaudeSonnet46)
            .expect("conversation builds")
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
            LlmError::ObserverAbort { reason } => {
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

        let mut conv = Conversation::new(ModelId::ClaudeSonnet46)
            .expect("conversation builds")
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
            .filter(|m| m.role == Role::User && m.content.contains("doom-loop suspected"))
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

        let mut conv = Conversation::new(ModelId::ClaudeSonnet46)
            .expect("conversation builds")
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

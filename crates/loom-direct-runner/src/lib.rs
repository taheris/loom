//! `loom-direct-runner` library surface.
//!
//! The binary is a thin shell around [`run_session`] — generic over an
//! [`LlmClient`] so tests can drive a scripted mock provider without
//! reaching the network.
//!
//! Pipeline:
//!
//! 1. Read JSONL frames from stdin as [`DirectCommand`] values.
//! 2. On [`DirectCommand::Prompt`], append the user turn and drive
//!    [`Conversation::run`] once.
//! 3. Walk the resulting transcript and emit [`DirectEvent`] frames to
//!    stdout — `tool_call` / `tool_result` for each assistant + tool
//!    pair, `text_delta` + `text_end` for the final assistant text,
//!    then `turn_end`.
//! 4. On EOF or [`DirectCommand::Abort`], emit `session_complete` and
//!    return.

use std::io;
use std::sync::{Arc, Mutex};

use loom_agent::direct::backend::{DirectCommand, DirectEvent};
use loom_agent::direct::tools::{
    Bash, Edit, Glob, Grep, OffloadRecord, Read, ToolContext, ToolContextError, Write,
};
use loom_driver::agent::SpawnConfig;
use loom_events::DriverEventPayload;
use loom_events::identifier::ToolCallId;
use loom_llm::api_key::ApiKey;
use loom_llm::cache::{CacheControl, CacheTtl};
use loom_llm::client::{
    AnthropicClient, BoxFuture, CompletionResponse, GeminiClient, LlmClient, LlmError, OpenAiClient,
};
use loom_llm::conversation::{ContextBudget, Conversation};
use loom_llm::model_id::{AnthropicModel, ModelId, SchemaKind};
use loom_llm::request::{CompletionRequest, Message, Role};
use loom_llm::tool::Tool;
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tracing::{debug, info, warn};

/// TTL the runner attaches to every user prompt so the underlying
/// provider treats each turn as a cache breakpoint. Anthropic honours
/// this directly; OpenAI / Gemini no-op the marker per
/// [`CacheControl`]'s contract.
const PROMPT_CACHE_TTL: CacheTtl = CacheTtl::Hours1;

/// Default Conversation model when the [`SpawnConfig`] omits one.
const DEFAULT_MODEL: ModelId = ModelId::Anthropic(AnthropicModel::ClaudeSonnet46);

/// Default inline content cap when an older spawn config omits output limits.
const DEFAULT_MAX_INLINE_BYTES: usize = 16_384;

/// Default request-context budget used before the runner trims ordinary
/// transcript turns while retaining the initial rendered prompt.
const DEFAULT_CONTEXT_BUDGET_BYTES: usize = 1_048_576;

/// Build the canonical six-tool registry the Direct backend registers
/// with every Conversation. Order matches the spec's tool list (`Read`,
/// `Write`, `Edit`, `Bash`, `Grep`, `Glob`).
pub fn six_tools(ctx: ToolContext) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(Read::new(ctx.clone())),
        Box::new(Write::new(ctx.clone())),
        Box::new(Edit::new(ctx.clone())),
        Box::new(Bash::new(ctx.clone())),
        Box::new(Grep::new(ctx.clone())),
        Box::new(Glob::new(ctx)),
    ]
}

/// Construct the Conversation `loom-direct-runner` drives. The model is
/// resolved from [`SpawnConfig::model_id`] via [`ModelId::from_str`]; when
/// absent the runner falls back to [`DEFAULT_MODEL`]. The six sandbox-aware
/// tools are registered in the canonical order, and both default observers
/// stay enabled.
pub fn build_conversation(config: &SpawnConfig) -> Conversation {
    build_conversation_with_context(config, tool_context(config))
}

fn build_conversation_with_context(config: &SpawnConfig, ctx: ToolContext) -> Conversation {
    let mut conv = Conversation::new(configured_model(config));
    for tool in six_tools(ctx) {
        conv = conv.register_boxed(tool);
    }
    conv
}

fn tool_context(config: &SpawnConfig) -> ToolContext {
    let max_inline_bytes = config
        .output_limits
        .map_or(DEFAULT_MAX_INLINE_BYTES, |limits| limits.max_inline_bytes);
    ToolContext::new(config.scratch_dir.join("offload"), max_inline_bytes)
}

fn configured_model(config: &SpawnConfig) -> ModelId {
    config.model_id.as_deref().map_or_else(
        || {
            config
                .model
                .as_ref()
                .map_or(DEFAULT_MODEL, |sel| ModelId::from_str(&sel.model_id))
        },
        ModelId::from_str,
    )
}

/// Resolve the configured model's schema, read the matching credential
/// from the per-schema environment variable, and construct the typed
/// per-schema [`LlmClient`] the runner drives. Returned boxed so the
/// runner does not branch on schema at the call site.
pub fn build_client_for_config(
    config: &SpawnConfig,
) -> Result<Box<dyn LlmClient + Send + Sync>, RunnerError> {
    let model = configured_model(config);
    match model.schema() {
        SchemaKind::Anthropic => {
            let api_key = read_api_key("ANTHROPIC_API_KEY")?;
            Ok(Box::new(AnthropicClient::new(api_key)))
        }
        SchemaKind::OpenAi => {
            let api_key = read_api_key("OPENAI_API_KEY")?;
            Ok(Box::new(OpenAiClient::new(api_key)))
        }
        SchemaKind::Gemini => {
            let api_key = read_api_key("GEMINI_API_KEY")?;
            Ok(Box::new(GeminiClient::new(api_key)))
        }
        other => Err(RunnerError::UnsupportedSchema { schema: other }),
    }
}

fn read_api_key(var: &str) -> Result<ApiKey, RunnerError> {
    let raw = std::env::var(var).map_err(|source| RunnerError::ApiKeyEnv {
        var: var.to_string(),
        source,
    })?;
    ApiKey::new(raw).map_err(|err| RunnerError::ApiKey {
        var: var.to_string(),
        source: err,
    })
}

/// Drive one Direct session against `client`. Reads JSONL commands from
/// `stdin`, emits JSONL events to `stdout`, returns when stdin closes or
/// the runner receives [`DirectCommand::Abort`].
pub async fn run_session<C, R, W>(
    client: C,
    config: SpawnConfig,
    stdin: R,
    stdout: W,
) -> Result<(), RunnerError>
where
    C: LlmClient + Sync,
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    run_session_with_context_budget(
        client,
        config,
        stdin,
        stdout,
        ContextBudget::max_request_bytes(DEFAULT_CONTEXT_BUDGET_BYTES),
    )
    .await
}

async fn run_session_with_context_budget<C, R, W>(
    client: C,
    config: SpawnConfig,
    stdin: R,
    stdout: W,
    context_budget: ContextBudget,
) -> Result<(), RunnerError>
where
    C: LlmClient + Sync,
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let ctx = tool_context(&config);
    let mut conv =
        build_conversation_with_context(&config, ctx.clone()).context_budget(context_budget);
    let driver_events = Arc::new(Mutex::new(Vec::<DriverEventPayload>::new()));
    let recording = UsageRecordingClient {
        inner: client,
        driver_events: driver_events.clone(),
    };
    let mut emitter = Emitter::new(stdout);
    let mut lines = stdin.lines();
    let mut exit_code: i32 = 0;
    let mut initial_prompt_pending = true;

    while let Some(line) = lines.next_line().await.map_err(RunnerError::Io)? {
        let trimmed = line.trim_end_matches('\r');
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<DirectCommand>(trimmed) {
            Ok(DirectCommand::Prompt { message }) => {
                debug!(bytes = message.len(), "received prompt");
                let pin_initial_prompt = initial_prompt_pending;
                initial_prompt_pending = false;
                if let Err(err) = run_prompt(
                    &mut conv,
                    &recording,
                    &driver_events,
                    &ctx,
                    &mut emitter,
                    message,
                    pin_initial_prompt,
                )
                .await
                {
                    warn!(error = %err, "prompt failed");
                    emitter
                        .emit(&DirectEvent::Error {
                            message: err.to_string(),
                        })
                        .await?;
                    exit_code = 1;
                }
            }
            Ok(DirectCommand::Steer { message }) => {
                debug!(bytes = message.len(), "received steer");
                conv.user_cached(message, CacheControl::Ephemeral(PROMPT_CACHE_TTL));
            }
            Ok(DirectCommand::Abort) => {
                info!("received abort, terminating session");
                break;
            }
            Err(err) => {
                warn!(error = %err, line = %trimmed, "malformed command frame");
                emitter
                    .emit(&DirectEvent::Error {
                        message: format!("invalid command frame: {err}"),
                    })
                    .await?;
                exit_code = 1;
            }
        }
    }

    emitter
        .emit(&DirectEvent::SessionComplete {
            exit_code,
            cost_usd: None,
        })
        .await?;
    Ok(())
}

async fn run_prompt<C, W>(
    conv: &mut Conversation,
    client: &UsageRecordingClient<C>,
    driver_events: &Mutex<Vec<DriverEventPayload>>,
    ctx: &ToolContext,
    emitter: &mut Emitter<W>,
    message: String,
    pin_initial_prompt: bool,
) -> Result<(), RunnerError>
where
    C: LlmClient + Sync,
    W: AsyncWrite + Unpin,
{
    let history_pivot = conv.history_len();
    if pin_initial_prompt {
        conv.user_cached_pinned(message, CacheControl::Ephemeral(PROMPT_CACHE_TTL));
    } else {
        conv.user_cached(message, CacheControl::Ephemeral(PROMPT_CACHE_TTL));
    }
    let response = match conv.run(client).await {
        Ok(response) => response,
        Err(err) => {
            for event in drain_driver_events(driver_events) {
                emitter.emit(&driver_event(&event)).await?;
            }
            for record in ctx.drain_offloads().map_err(RunnerError::ToolContext)? {
                emitter.emit(&offload_event(&record)).await?;
            }
            return Err(RunnerError::Llm(err.to_string()));
        }
    };

    for message in conv.history_since(history_pivot) {
        for event in events_from_history(message) {
            emitter.emit(&event).await?;
        }
    }

    for record in ctx.drain_offloads().map_err(RunnerError::ToolContext)? {
        emitter.emit(&offload_event(&record)).await?;
    }

    if !response.text.is_empty() {
        emitter
            .emit(&DirectEvent::TextDelta {
                text: response.text.clone(),
            })
            .await?;
        emitter.emit(&DirectEvent::TextEnd).await?;
    }
    for event in drain_driver_events(driver_events) {
        emitter.emit(&driver_event(&event)).await?;
    }
    emitter.emit(&DirectEvent::TurnEnd).await?;
    Ok(())
}

fn drain_driver_events(events: &Mutex<Vec<DriverEventPayload>>) -> Vec<DriverEventPayload> {
    let mut guard = events.lock().unwrap_or_else(|poison| poison.into_inner());
    std::mem::take(&mut *guard)
}

fn driver_event(event: &DriverEventPayload) -> DirectEvent {
    DirectEvent::DriverEvent {
        driver_kind: event.driver_kind.clone(),
        summary: event.summary.clone(),
        payload: event.payload.clone(),
    }
}

fn offload_event(record: &OffloadRecord) -> DirectEvent {
    driver_event(&DriverEventPayload::offload(
        record.tool.clone(),
        record.total_bytes,
    ))
}

/// Decorator around an inner [`LlmClient`] that captures driver-origin
/// payloads so the host-side session stamper owns final `AgentEvent`
/// construction.
struct UsageRecordingClient<C> {
    inner: C,
    driver_events: Arc<Mutex<Vec<DriverEventPayload>>>,
}

impl<C> UsageRecordingClient<C> {
    fn record(&self, event: DriverEventPayload) {
        self.driver_events
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(event);
    }
}

impl<C: LlmClient + Sync> LlmClient for UsageRecordingClient<C> {
    fn schema(&self) -> SchemaKind {
        self.inner.schema()
    }

    fn supports(&self, model: &ModelId) -> bool {
        self.inner.supports(model)
    }

    fn emit_driver_event(&self, event: DriverEventPayload) {
        self.record(event);
    }

    fn complete<'a>(
        &'a self,
        req: CompletionRequest,
    ) -> BoxFuture<'a, Result<CompletionResponse, LlmError>> {
        Box::pin(async move {
            let model = req.model.clone();
            let resp = self.inner.complete(req).await?;
            self.record(DriverEventPayload::token_usage(
                model.as_wire(),
                resp.usage.input,
                resp.usage.output,
                resp.usage.cache_read,
                resp.usage.cache_write,
            ));
            Ok(resp)
        })
    }

    fn complete_structured_raw<'a>(
        &'a self,
        req: CompletionRequest,
        schema: serde_json::Value,
        type_name: String,
    ) -> BoxFuture<'a, Result<String, LlmError>> {
        self.inner.complete_structured_raw(req, schema, type_name)
    }
}

fn events_from_history(message: &Message) -> Vec<DirectEvent> {
    match message.role {
        Role::User => Vec::new(),
        Role::Assistant => message
            .tool_calls
            .iter()
            .map(|call| DirectEvent::ToolCall {
                id: ToolCallId::new(call.call_id.as_str()),
                tool: call.name.clone(),
                params: call.args.clone(),
                parent_tool_call_id: None,
            })
            .collect(),
        Role::Tool => message
            .tool_call_id
            .as_ref()
            .map_or_else(Vec::new, |call_id| {
                vec![DirectEvent::ToolResult {
                    id: ToolCallId::new(call_id.as_str()),
                    output: tool_result_payload(&message.text_content()),
                    is_error: message.tool_is_error,
                }]
            }),
    }
}

fn tool_result_payload(content: &str) -> String {
    match serde_json::from_str::<Value>(content) {
        Ok(Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => content.to_string(),
    }
}

/// Buffer-flushing JSONL writer. Each call to [`Emitter::emit`] writes
/// one line + `\n` and flushes so the host-side parser sees the frame
/// before the runner buffers the next.
struct Emitter<W: AsyncWrite + Unpin> {
    writer: W,
}

impl<W: AsyncWrite + Unpin> Emitter<W> {
    fn new(writer: W) -> Self {
        Self { writer }
    }

    async fn emit(&mut self, event: &DirectEvent) -> Result<(), RunnerError> {
        let mut line = serde_json::to_string(event).map_err(RunnerError::EncodeJson)?;
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .await
            .map_err(RunnerError::Io)?;
        self.writer.flush().await.map_err(RunnerError::Io)?;
        Ok(())
    }
}

/// Errors the runner surfaces to its caller.
#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum RunnerError {
    /// stdin/stdout io failure: {0}
    Io(#[source] io::Error),
    /// failed to encode event frame: {0}
    EncodeJson(#[source] serde_json::Error),
    /// llm error during conversation run: {0}
    Llm(String),
    /// Direct tool context failure
    ToolContext(#[source] ToolContextError),
    /// failed to read API key env var {var}
    ApiKeyEnv {
        /// Name of the env var the runner read.
        var: String,
        /// Underlying environment lookup error.
        #[source]
        source: std::env::VarError,
    },
    /// invalid API key sourced from {var}: {source}
    ApiKey {
        /// Name of the env var the runner read.
        var: String,
        /// Underlying [`loom_llm::api_key::ApiKeyError`] from
        /// [`ApiKey::new`].
        #[source]
        source: loom_llm::api_key::ApiKeyError,
    },
    /// runner does not have a per-schema Client for {schema:?}
    UnsupportedSchema {
        /// Schema the configured model resolves to.
        schema: SchemaKind,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::agent::{OutputLimits, RePinContent};
    use loom_driver::config::{LoomConfig, Phase};
    use loom_events::identifier::{BeadId, SessionId};
    use loom_events::{AgentEvent, DriverKind, EnvelopeBuilder, SessionScope, Source};
    use loom_llm::client::{
        CompletionResponse, LlmError, ToolCallId as LlmToolCallId, ToolUseRequest,
    };
    use loom_llm::request::CompletionRequest;
    use loom_llm::usage::TokenUsage;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Mutex;

    const PLANNING_PROMPT_INTERVIEW_MODES: &str =
        include_str!("../../../tests/fixtures/planning_prompt_interview_modes.md");
    const POLISH_MODE_DEFINITION: &str = "- polish / do-a-polish: report-only mode. Review the proposed wording and report suggested edits, but do not modify files or apply edits unless the human explicitly asks you to make the edits.";
    const ONE_BY_ONE_MODE_DEFINITION: &str = "- one-by-one: ask exactly one design question per turn, then wait for the human's answer before asking the next question or changing topics.";

    fn sample_config(model_id: Option<&str>) -> SpawnConfig {
        SpawnConfig {
            image_ref: "localhost/wrix-test:direct".into(),
            image_source: PathBuf::from("/nix/store/zzz-test.tar"),
            image_source_kind: Some(loom_driver::agent::ImageSourceKind::NixDescriptor),
            profile_config: None,
            workspace: PathBuf::from("/workspace"),
            env: vec![("WRIX_AGENT".into(), "direct".into())],
            mounts: vec![],
            initial_prompt: "hello".into(),
            agent_args: vec![],
            repin: RePinContent {
                orientation: String::new(),
                pinned_context: String::new(),
                partial_bodies: vec![],
            },
            skills: None,
            scratch_dir: PathBuf::new(),
            model_id: model_id.map(str::to_string),
            model: None,
            thinking_level: None,
            output_limits: None,
            shutdown_grace: None,
            denied_tools: Vec::new(),
            handshake_timeout: None,
            stall_warn_interval: None,
            launcher_env: Vec::new(),
        }
    }

    /// Scripted client that hands back pre-baked responses in order.
    /// Mirrors the `ScriptedClient` pattern used by Conversation's own
    /// loop tests — the runner needs no live provider to exercise its
    /// JSONL wire emission.
    struct ScriptedClient {
        responses: Mutex<Vec<CompletionResponse>>,
    }

    impl ScriptedClient {
        fn new(responses: Vec<CompletionResponse>) -> Self {
            Self {
                responses: Mutex::new(responses),
            }
        }
    }

    impl LlmClient for ScriptedClient {
        fn schema(&self) -> SchemaKind {
            SchemaKind::Anthropic
        }

        fn supports(&self, _model: &ModelId) -> bool {
            true
        }

        fn complete<'a>(
            &'a self,
            _req: CompletionRequest,
        ) -> BoxFuture<'a, Result<CompletionResponse, LlmError>> {
            Box::pin(async move {
                let mut guard = self
                    .responses
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                if guard.is_empty() {
                    Err(LlmError::Provider {
                        message: "scripted client exhausted".into(),
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
        ) -> BoxFuture<'a, Result<String, LlmError>> {
            Box::pin(async move {
                Err(LlmError::Provider {
                    message: "structured not used in runner tests".into(),
                })
            })
        }
    }

    fn final_text(text: &str) -> CompletionResponse {
        CompletionResponse {
            text: text.into(),
            usage: TokenUsage::default(),
            tool_calls: Vec::new(),
        }
    }

    /// The runner registers exactly six tools by name, in the canonical
    /// order documented in `specs/agent.md` § Direct Backend.
    #[test]
    fn direct_runner_registers_canonical_six_tools() {
        let tools = six_tools(tool_context(&sample_config(None)));
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["Read", "Write", "Edit", "Bash", "Grep", "Glob"]);
    }

    /// Both default observers ship enabled when the runner builds its
    /// Conversation — matches `Conversation::new`'s defaults so the
    /// CLI-side `[agent.doom_loop]` / `[agent.duplicate_result]` config
    /// surface is the only opt-out path.
    #[test]
    fn direct_runner_composes_default_observers() {
        let conv = build_conversation(&sample_config(None));
        assert!(
            conv.doom_loop_enabled(),
            "DoomLoopObserver enabled by default in runner Conversation",
        );
        assert!(
            conv.duplicate_result_enabled(),
            "DuplicateResultObserver enabled by default in runner Conversation",
        );
    }

    /// Per-phase direct config is installed on `SpawnConfig::model_id` before
    /// the runner constructs its `Conversation`, so the live config seam drives
    /// `ModelId::from_str` rather than a test-populated model field.
    #[test]
    fn direct_model_id_respects_phase_config() {
        let cfg = LoomConfig::from_toml_str(
            "[phase.gate.review]\nagent.backend = \"direct\"\nagent.model_id = \"claude-sonnet-4-6\"\n",
        )
        .expect("parse config");
        let selection = cfg.agent_for(Phase::Review).expect("resolve review agent");
        let mut spawn = sample_config(None);
        selection.apply_to_spawn_config(&mut spawn, cfg.direct_output_limits());

        let conv = build_conversation(&spawn);
        assert_eq!(
            *conv.model(),
            ModelId::Anthropic(AnthropicModel::ClaudeSonnet46),
        );
    }

    /// The `model_id` field from `SpawnConfig` resolves through
    /// `ModelId::from_str` so a known string like `claude-sonnet-4-6`
    /// produces the typed variant rather than falling through to
    /// `Other`. Unknown strings round-trip via `Other` so external
    /// consumers can name not-yet-supported models without a minor bump.
    #[test]
    fn direct_runner_uses_spawn_config_model_id() {
        let conv = build_conversation(&sample_config(Some("claude-sonnet-4-6")));
        assert_eq!(
            *conv.model(),
            ModelId::Anthropic(AnthropicModel::ClaudeSonnet46),
        );

        let conv_unknown = build_conversation(&sample_config(Some("claude-future-x")));
        assert_eq!(
            *conv_unknown.model(),
            ModelId::Anthropic(AnthropicModel::Other("claude-future-x".to_string())),
        );

        let conv_default = build_conversation(&sample_config(None));
        assert_eq!(*conv_default.model(), DEFAULT_MODEL);
    }

    /// End-to-end JSONL drive: feed the runner one prompt frame against
    /// a scripted client that returns final assistant text, and assert
    /// the emitted JSONL frames match the wire shape the host's
    /// `DirectParser` decodes. Pins compatibility with the Pi/Claude
    /// per-frame line discipline (one JSON object + `\n`) and the
    /// `DirectEvent` tag/variant set.
    #[test]
    fn direct_runner_emits_agent_event_jsonl_compatible_with_common_agent_events() {
        let client = ScriptedClient::new(vec![final_text("hello back")]);
        let stdin = b"{\"type\":\"prompt\",\"message\":\"hi\"}\n".to_vec();
        let mut stdout: Vec<u8> = Vec::new();

        tokio_test::block_on(run_session(
            client,
            sample_config(Some("claude-sonnet-4-6")),
            tokio::io::BufReader::new(&stdin[..]),
            &mut stdout,
        ))
        .expect("run_session completes");

        let lines: Vec<&str> = std::str::from_utf8(&stdout)
            .expect("utf-8 stdout")
            .lines()
            .collect();

        let parsed: Vec<DirectEvent> = lines
            .iter()
            .map(|l| serde_json::from_str(l).expect("each line parses as DirectEvent"))
            .collect();

        let kinds: Vec<&str> = lines
            .iter()
            .map(|l| {
                serde_json::from_str::<Value>(l)
                    .expect("json")
                    .get("type")
                    .and_then(Value::as_str)
                    .map_or("<missing>", |s| match s {
                        "text_delta" => "text_delta",
                        "text_end" => "text_end",
                        "driver_event" => "driver_event",
                        "turn_end" => "turn_end",
                        "session_complete" => "session_complete",
                        other => panic!("unexpected event type {other}"),
                    })
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "text_delta",
                "text_end",
                "driver_event",
                "turn_end",
                "session_complete",
            ],
        );

        match &parsed[0] {
            DirectEvent::TextDelta { text } => assert_eq!(text, "hello back"),
            other => panic!("expected TextDelta, got {other:?}"),
        }
        match &parsed[4] {
            DirectEvent::SessionComplete {
                exit_code,
                cost_usd,
            } => {
                assert_eq!(*exit_code, 0);
                assert!(cost_usd.is_none());
            }
            other => panic!("expected SessionComplete, got {other:?}"),
        }
    }

    /// Tool-call + tool-result pairs flow through into the wire stream
    /// in the same `tool_call -> tool_result` order Conversation's loop
    /// dispatched them. Pins the history-walk that recovers per-call
    /// observability when `Conversation::run` only returns the final
    /// response.
    #[test]
    fn direct_runner_emits_tool_call_and_result_frames_in_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("hello.txt");
        std::fs::write(&target, "hi\n").expect("write fixture");

        let with_call = CompletionResponse {
            text: String::new(),
            usage: TokenUsage::default(),
            tool_calls: vec![ToolUseRequest {
                call_id: LlmToolCallId::parse("call-1").expect("test tool call id parses"),
                name: "Read".into(),
                args: json!({ "file_path": target }),
            }],
        };
        let client = ScriptedClient::new(vec![with_call, final_text("done")]);
        let stdin = b"{\"type\":\"prompt\",\"message\":\"please read\"}\n".to_vec();
        let mut stdout: Vec<u8> = Vec::new();

        tokio_test::block_on(run_session(
            client,
            sample_config(None),
            tokio::io::BufReader::new(&stdin[..]),
            &mut stdout,
        ))
        .expect("run_session completes");

        let events: Vec<DirectEvent> = std::str::from_utf8(&stdout)
            .expect("utf-8")
            .lines()
            .map(|l| serde_json::from_str(l).expect("parse line"))
            .collect();

        let mut iter = events.into_iter();
        match iter.next().expect("tool_call") {
            DirectEvent::ToolCall {
                id, tool, params, ..
            } => {
                assert_eq!(id.as_str(), "call-1");
                assert_eq!(tool, "Read");
                assert!(
                    params.get("file_path").is_some(),
                    "params forwarded: {params}"
                );
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
        match iter.next().expect("tool_result") {
            DirectEvent::ToolResult {
                id,
                is_error,
                output,
            } => {
                assert_eq!(id.as_str(), "call-1");
                assert!(!is_error, "Read of real file succeeds: {output}");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
        let kinds: Vec<&'static str> = iter
            .map(|e| match e {
                DirectEvent::TextDelta { .. } => "text_delta",
                DirectEvent::TextEnd => "text_end",
                DirectEvent::DriverEvent { .. } => "driver_event",
                DirectEvent::TurnEnd => "turn_end",
                DirectEvent::SessionComplete { .. } => "session_complete",
                other => panic!("unexpected trailing event {other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "text_delta",
                "text_end",
                "driver_event",
                "driver_event",
                "turn_end",
                "session_complete",
            ],
        );
    }

    /// Malformed JSONL on stdin emits an Error frame but does not crash
    /// the runner; the session-complete still fires at EOF so the host
    /// observes a clean termination.
    #[test]
    fn malformed_command_emits_error_and_keeps_session_alive() {
        let client = ScriptedClient::new(Vec::new());
        let stdin = b"not json\n".to_vec();
        let mut stdout: Vec<u8> = Vec::new();

        tokio_test::block_on(run_session(
            client,
            sample_config(None),
            tokio::io::BufReader::new(&stdin[..]),
            &mut stdout,
        ))
        .expect("run_session completes");

        let events: Vec<DirectEvent> = std::str::from_utf8(&stdout)
            .expect("utf-8")
            .lines()
            .map(|l| serde_json::from_str(l).expect("parse"))
            .collect();
        assert_eq!(events.len(), 2);
        assert!(
            matches!(events[0], DirectEvent::Error { .. }),
            "first event Error: {:?}",
            events[0],
        );
        match &events[1] {
            DirectEvent::SessionComplete { exit_code, .. } => assert_eq!(*exit_code, 1),
            other => panic!("expected SessionComplete, got {other:?}"),
        }
    }

    /// Client that captures every [`CompletionRequest`] the runner
    /// constructs into a shared [`Arc<Mutex<Vec<_>>>`] so a test can
    /// inspect the lowered messages and tool definitions without
    /// reaching a live provider. The shared handle stays alive after
    /// `run_session` consumes the client.
    struct CapturingClient {
        captured: std::sync::Arc<Mutex<Vec<CompletionRequest>>>,
        response: CompletionResponse,
    }

    impl CapturingClient {
        fn new(
            response: CompletionResponse,
        ) -> (Self, std::sync::Arc<Mutex<Vec<CompletionRequest>>>) {
            let captured = std::sync::Arc::new(Mutex::new(Vec::new()));
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
        ) -> BoxFuture<'a, Result<CompletionResponse, LlmError>> {
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
        ) -> BoxFuture<'a, Result<String, LlmError>> {
            Box::pin(async move {
                Err(LlmError::Provider {
                    message: "structured not used in runner tests".into(),
                })
            })
        }
    }

    fn command_frame(cmd: &DirectCommand) -> String {
        let mut line = serde_json::to_string(cmd).expect("command serializes");
        line.push('\n');
        line
    }

    fn forced_budget_planning_request() -> CompletionRequest {
        let (client, captured) = CapturingClient::new(final_text("ok"));
        let stale_history = [
            "ordinary history alpha: prior assistant summary and tool chatter".repeat(8),
            "ordinary history beta: stale grep/read/bash outputs".repeat(8),
        ];
        let current_prompt = "Current request: continue the planning interview.";
        let stdin = [
            command_frame(&DirectCommand::Prompt {
                message: PLANNING_PROMPT_INTERVIEW_MODES.to_string(),
            }),
            command_frame(&DirectCommand::Steer {
                message: stale_history[0].clone(),
            }),
            command_frame(&DirectCommand::Steer {
                message: stale_history[1].clone(),
            }),
            command_frame(&DirectCommand::Prompt {
                message: current_prompt.to_string(),
            }),
        ]
        .concat()
        .into_bytes();
        let mut stdout: Vec<u8> = Vec::new();

        tokio_test::block_on(run_session_with_context_budget(
            client,
            sample_config(Some("claude-sonnet-4-6")),
            tokio::io::BufReader::new(&stdin[..]),
            &mut stdout,
            ContextBudget::max_request_bytes(1),
        ))
        .expect("run_session completes");

        let requests = captured.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(requests.len(), 2, "two prompt frames produce requests");
        requests[1].clone()
    }

    fn assert_pinned_planning_prompt_survives(request: &CompletionRequest) {
        let texts: Vec<String> = request.messages.iter().map(Message::text_content).collect();
        assert_eq!(
            texts,
            vec![
                PLANNING_PROMPT_INTERVIEW_MODES.to_string(),
                "Current request: continue the planning interview.".to_string(),
            ],
            "forced budget should keep the pinned initial prompt and active turn only",
        );
        let request_text = texts.join("\n---\n");
        assert!(
            request_text.contains(POLISH_MODE_DEFINITION),
            "polish mode definition missing from request: {request_text}",
        );
        assert!(
            request_text.contains(ONE_BY_ONE_MODE_DEFINITION),
            "one-by-one mode definition missing from request: {request_text}",
        );
        assert!(
            request_text.contains("## Context Pinning"),
            "instruction/context section missing from request: {request_text}",
        );
        assert!(
            request_text.contains("## Final Protocol"),
            "phase protocol section missing from request: {request_text}",
        );
        assert!(
            !request_text.contains("ordinary history alpha"),
            "ordinary history alpha should be trimmed before pinned prompt: {request_text}",
        );
        assert!(
            !request_text.contains("ordinary history beta"),
            "ordinary history beta should be trimmed before pinned prompt: {request_text}",
        );
    }

    /// Direct context-budget trimming treats the first rendered phase
    /// prompt as pinned instruction context. Under forced byte pressure,
    /// ordinary history is dropped while the planning prompt's Interview
    /// Modes section stays verbatim in the LLM request.
    #[test]
    fn direct_context_budget_preserves_initial_prompt_pin() {
        let request = forced_budget_planning_request();
        assert_pinned_planning_prompt_survives(&request);
    }

    /// Hard-limit fallback preserves instruction, phase protocol, and
    /// mode sections before ordinary transcript turns. This is the
    /// Direct-backed verifier for the harness compaction-recovery
    /// fallback contract.
    #[test]
    fn hard_limit_fallback_preserves_pinned_instruction_sections() {
        let request = forced_budget_planning_request();
        assert_pinned_planning_prompt_survives(&request);
    }

    /// Spec contract (`specs/agent.md` § Direct Backend): per-call
    /// `CacheControl::Ephemeral(CacheTtl)` markers in the runner's prompt
    /// construction flow through to the provider request. The runner
    /// attaches an ephemeral cache marker to every incoming user prompt
    /// so subsequent turns hit cache on the established prefix;
    /// `llm`'s `multi_provider` adapter lowers the marker to the
    /// Anthropic adapter's `cache_control` field (Anthropic-confirmed
    /// path) and the OpenAI/Gemini adapters no-op it without error.
    #[test]
    fn direct_cache_control_propagates_to_anthropic_request() {
        let (client, captured) = CapturingClient::new(final_text("ok"));
        let stdin =
            b"{\"type\":\"prompt\",\"message\":\"orient me on spec X\"}\n{\"type\":\"steer\",\"message\":\"focus on cache\"}\n{\"type\":\"prompt\",\"message\":\"continue\"}\n"
                .to_vec();
        let mut stdout: Vec<u8> = Vec::new();

        tokio_test::block_on(run_session(
            client,
            sample_config(Some("claude-sonnet-4-6")),
            tokio::io::BufReader::new(&stdin[..]),
            &mut stdout,
        ))
        .expect("run_session completes");

        let requests: Vec<CompletionRequest> =
            captured.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(
            requests.len(),
            2,
            "two prompt frames produce two completion requests",
        );
        assert_eq!(
            requests[0].model,
            ModelId::Anthropic(AnthropicModel::ClaudeSonnet46),
            "request targets the phase-configured Anthropic model",
        );
        let cached_blocks: Vec<&Message> = requests[0]
            .messages
            .iter()
            .filter(|m| {
                m.content
                    .iter()
                    .any(|part| matches!(part.cache(), CacheControl::Ephemeral(_)))
            })
            .collect();
        assert_eq!(
            cached_blocks.len(),
            1,
            "the first prompt becomes a cached user block: {:?}",
            requests[0].messages,
        );
        let first_cache = cached_blocks[0].content[0].cache();
        assert!(
            matches!(first_cache, CacheControl::Ephemeral(CacheTtl::Hours1)),
            "ephemeral 1h marker reaches the request: {first_cache:?}",
        );

        let second_cached: Vec<&Message> = requests[1]
            .messages
            .iter()
            .filter(|m| {
                m.content
                    .iter()
                    .any(|part| matches!(part.cache(), CacheControl::Ephemeral(_)))
            })
            .collect();
        assert_eq!(
            second_cached.len(),
            3,
            "first prompt, steer, and second prompt each become cache breakpoints: {:?}",
            requests[1].messages,
        );
    }

    /// Spec contract (`specs/agent.md` § Direct Backend):
    /// `DriverKind::TokenUsage` event emits on every completion within
    /// Direct sessions. The runner wraps the LLM client so each
    /// `complete*` call records its `TokenUsage`; the typed driver frame
    /// reaches stdout in turn-completion order and the host's parser lifts it into an
    /// `AgentEvent::DriverEvent { driver_kind: TokenUsage, .. }` with
    /// `source: Source::Driver`.
    #[test]
    fn direct_emits_token_usage_per_completion() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cargo = dir.path().join("Cargo.toml");
        std::fs::write(&cargo, "[package]\nname = \"x\"\n").expect("write fixture");

        let with_call = CompletionResponse {
            text: String::new(),
            usage: TokenUsage {
                input: 500,
                output: 120,
                cache_read: 200,
                cache_write: 50,
            },
            tool_calls: vec![ToolUseRequest {
                call_id: LlmToolCallId::parse("call-1").expect("test tool call id parses"),
                name: "Read".into(),
                args: json!({ "file_path": cargo }),
            }],
        };
        let final_resp = CompletionResponse {
            text: "done".into(),
            usage: TokenUsage {
                input: 800,
                output: 60,
                cache_read: 600,
                cache_write: 0,
            },
            tool_calls: Vec::new(),
        };

        let client = ScriptedClient::new(vec![with_call, final_resp]);
        let stdin = b"{\"type\":\"prompt\",\"message\":\"please read\"}\n".to_vec();
        let mut stdout: Vec<u8> = Vec::new();

        tokio_test::block_on(run_session(
            client,
            sample_config(Some("claude-sonnet-4-6")),
            tokio::io::BufReader::new(&stdin[..]),
            &mut stdout,
        ))
        .expect("run_session completes");

        let events: Vec<DirectEvent> = std::str::from_utf8(&stdout)
            .expect("utf-8")
            .lines()
            .map(|l| serde_json::from_str(l).expect("parse"))
            .collect();

        let usages: Vec<&DirectEvent> = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    DirectEvent::DriverEvent {
                        driver_kind: DriverKind::TokenUsage,
                        ..
                    }
                )
            })
            .collect();
        assert_eq!(
            usages.len(),
            2,
            "one token-usage driver frame per completion (two completions for a tool-using turn): {events:?}",
        );

        match usages[0] {
            DirectEvent::DriverEvent { payload, .. } => {
                assert_eq!(payload["model"], "claude-sonnet-4-6");
                assert_eq!(payload["input"], 500);
                assert_eq!(payload["output"], 120);
                assert_eq!(payload["cache_read"], 200);
                assert_eq!(payload["cache_write"], 50);
            }
            other => panic!("expected token usage DriverEvent, got {other:?}"),
        }
        match usages[1] {
            DirectEvent::DriverEvent { payload, .. } => {
                assert_eq!(payload["model"], "claude-sonnet-4-6");
                assert_eq!(payload["input"], 800);
                assert_eq!(payload["output"], 60);
                assert_eq!(payload["cache_read"], 600);
                assert_eq!(payload["cache_write"], 0);
            }
            other => panic!("expected token usage DriverEvent, got {other:?}"),
        }
    }

    #[test]
    fn direct_runner_emits_observer_driver_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("large.txt");
        std::fs::write(&target, "x".repeat(300)).expect("write fixture");
        let repeated_read = || CompletionResponse {
            text: String::new(),
            usage: TokenUsage::default(),
            tool_calls: vec![ToolUseRequest {
                call_id: LlmToolCallId::parse("call-read").expect("test tool call id parses"),
                name: "Read".into(),
                args: json!({ "file_path": target.clone() }),
            }],
        };
        let client = ScriptedClient::new(vec![
            repeated_read(),
            repeated_read(),
            repeated_read(),
            final_text("done"),
        ]);
        let stdin = b"{\"type\":\"prompt\",\"message\":\"please read\"}\n".to_vec();
        let mut stdout: Vec<u8> = Vec::new();

        tokio_test::block_on(run_session(
            client,
            sample_config(Some("claude-sonnet-4-6")),
            tokio::io::BufReader::new(&stdin[..]),
            &mut stdout,
        ))
        .expect("run_session completes");

        let events: Vec<DirectEvent> = std::str::from_utf8(&stdout)
            .expect("utf-8")
            .lines()
            .map(|l| serde_json::from_str(l).expect("parse"))
            .collect();
        assert!(
            events.iter().any(|event| matches!(
                event,
                DirectEvent::DriverEvent {
                    driver_kind: DriverKind::DoomLoopTripped,
                    ..
                }
            )),
            "doom-loop observer payload must reach the Direct wire: {events:?}",
        );
        assert!(
            events.iter().any(|event| matches!(
                event,
                DirectEvent::DriverEvent {
                    driver_kind: DriverKind::DuplicateToolResult,
                    ..
                }
            )),
            "duplicate-result observer payload must reach the Direct wire: {events:?}",
        );
    }

    /// Spec contract (`specs/agent.md` § Direct output bounding): every
    /// successful offload emits a driver event carrying the tool name and
    /// offloaded byte count. The test drives a real `Read` tool call over
    /// the cap, observes the runner's offload driver frame, and
    /// verifies the host-side lift produces a `driver_event` with
    /// `DriverKind::Offload`.
    #[test]
    fn offload_emits_driver_event_with_tool_and_byte_count() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("large.txt");
        let body = "alpha\nbeta\n";
        std::fs::write(&target, body).expect("write fixture");

        let with_call = CompletionResponse {
            text: String::new(),
            usage: TokenUsage::default(),
            tool_calls: vec![ToolUseRequest {
                call_id: LlmToolCallId::parse("call-1").expect("test tool call id parses"),
                name: "Read".into(),
                args: json!({ "file_path": target }),
            }],
        };
        let client = ScriptedClient::new(vec![with_call, final_text("done")]);
        let stdin = b"{\"type\":\"prompt\",\"message\":\"please read\"}\n".to_vec();
        let mut stdout: Vec<u8> = Vec::new();
        let mut config = sample_config(Some("claude-sonnet-4-6"));
        config.scratch_dir = dir.path().join("scratch");
        config.output_limits = Some(OutputLimits {
            max_inline_bytes: "alpha".len(),
        });

        tokio_test::block_on(run_session(
            client,
            config,
            tokio::io::BufReader::new(&stdin[..]),
            &mut stdout,
        ))
        .expect("run_session completes");

        let events: Vec<DirectEvent> = std::str::from_utf8(&stdout)
            .expect("utf-8")
            .lines()
            .map(|l| serde_json::from_str(l).expect("parse"))
            .collect();
        let offloads: Vec<&DirectEvent> = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    DirectEvent::DriverEvent {
                        driver_kind: DriverKind::Offload,
                        ..
                    }
                )
            })
            .collect();
        assert_eq!(offloads.len(), 1, "one offload event: {events:?}");

        let offload = offloads[0];
        match offload {
            DirectEvent::DriverEvent { payload, .. } => {
                assert_eq!(payload["tool"], "Read");
                assert_eq!(payload["total_bytes"], body.len());
            }
            other => panic!("expected offload DriverEvent, got {other:?}"),
        }

        let mut builder = EnvelopeBuilder::new(
            SessionScope::bead(
                SessionId::new("sess-test"),
                BeadId::new("lm-test").expect("valid id"),
                None,
                0,
            ),
            Source::Agent,
            || 1,
        );
        let agent_event = AgentEvent::from_parsed(offload.clone().into_parsed(), builder.build());
        match agent_event {
            AgentEvent::DriverEvent {
                envelope,
                driver_kind,
                summary,
                payload,
            } => {
                assert_eq!(envelope.source, Source::Driver);
                assert_eq!(driver_kind, DriverKind::Offload);
                assert_eq!(summary, format!("Read offloaded {} bytes", body.len()));
                assert_eq!(payload["tool"], "Read");
                assert_eq!(payload["total_bytes"], body.len());
            }
            other => panic!("expected DriverEvent, got {other:?}"),
        }
    }

    /// `abort` halts the session immediately and emits a clean
    /// session_complete — no remaining stdin frames are processed.
    #[test]
    fn abort_command_terminates_loop_with_zero_exit() {
        let client = ScriptedClient::new(Vec::new());
        let stdin =
            b"{\"type\":\"abort\"}\n{\"type\":\"prompt\",\"message\":\"never seen\"}\n".to_vec();
        let mut stdout: Vec<u8> = Vec::new();

        tokio_test::block_on(run_session(
            client,
            sample_config(None),
            tokio::io::BufReader::new(&stdin[..]),
            &mut stdout,
        ))
        .expect("run_session completes");

        let events: Vec<DirectEvent> = std::str::from_utf8(&stdout)
            .expect("utf-8")
            .lines()
            .map(|l| serde_json::from_str(l).expect("parse"))
            .collect();
        assert_eq!(events.len(), 1);
        match &events[0] {
            DirectEvent::SessionComplete { exit_code, .. } => assert_eq!(*exit_code, 0),
            other => panic!("expected SessionComplete, got {other:?}"),
        }
    }
}

//! Concrete `LlmClient` impl on top of the multi-provider `genai` crate.
//!
//! The wrapper insulates consumers from `genai`'s API churn: every
//! consumer-facing type is defined inside `llm`, and swapping
//! `genai` for another underlying crate is an internal change with no
//! breaking surface impact.

use std::sync::{Arc, Mutex};

use genai::chat::{
    CacheControl as GenAiCacheControl, ChatMessage, ChatOptions, ChatRequest, ChatResponse,
    ChatResponseFormat, ChatRole, JsonSpec, MessageContent, MessageOptions, Tool as GenAiTool,
    ToolCall as GenAiToolCall, ToolResponse as GenAiToolResponse, Usage as GenAiUsage,
};
use loom_events::{AgentEvent, DriverKind, EnvelopeBuilder, EventSink, Source};
use schemars::{JsonSchema, SchemaGenerator};
use serde::de::DeserializeOwned;

use crate::cache::{CacheControl, CacheTtl};
use crate::client::{CompletionResponse, LlmClient, LlmError, ToolUseRequest};
use crate::model_id::ModelId;
use crate::request::{CompletionRequest, Message, Role};
use crate::tool::ToolDef;
use crate::usage::TokenUsage;

/// Concrete `LlmClient` over the underlying multi-provider crate. The
/// struct carries no model — every `complete*` call routes to the
/// `ModelId` named on the request, so a single instance fans out across
/// providers and per-call model variants.
///
/// An optional event sink + [`EnvelopeBuilder`] pair attached via
/// [`Client::with_event_sink`] receives a
/// [`DriverKind::TokenUsage`] [`AgentEvent`] after every successful
/// `complete*` call. When none is attached the event is silently
/// dropped — there is no global state, no logging fallback, and no
/// observable side effect on the call.
#[derive(Clone, Default)]
pub struct Client {
    inner: genai::Client,
    usage_emitter: Option<Arc<Mutex<UsageEmitter>>>,
}

struct UsageEmitter {
    sink: Box<dyn EventSink>,
    envelope_builder: EnvelopeBuilder,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("inner", &self.inner)
            .field("usage_emitter_attached", &self.usage_emitter.is_some())
            .finish()
    }
}

impl Client {
    /// Construct a client using the underlying crate's default
    /// configuration. Anthropic credentials are read from
    /// `ANTHROPIC_API_KEY`; other providers use their respective env
    /// vars per the underlying crate's resolvers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a client around an explicit underlying `genai::Client`.
    /// Useful when consumers want to inject a pre-configured HTTP client,
    /// custom auth resolver, or alternate endpoints.
    pub fn from_genai(inner: genai::Client) -> Self {
        Self {
            inner,
            usage_emitter: None,
        }
    }

    /// Attach the active event sink chain plus an [`EnvelopeBuilder`]
    /// that stamps the per-spawn bead / molecule / iteration / source
    /// metadata onto each emitted event. Returns the configured client.
    ///
    /// After this is set, every `complete*` call that succeeds emits a
    /// [`DriverKind::TokenUsage`] [`AgentEvent`] into the sink before
    /// returning the response. When this is not set the event is
    /// silently dropped.
    pub fn with_event_sink<S>(mut self, sink: S, envelope_builder: EnvelopeBuilder) -> Self
    where
        S: EventSink + 'static,
    {
        self.usage_emitter = Some(Arc::new(Mutex::new(UsageEmitter {
            sink: Box::new(sink),
            envelope_builder,
        })));
        self
    }

    fn emit_usage(&self, model: &ModelId, usage: &TokenUsage) {
        let Some(emitter) = &self.usage_emitter else {
            return;
        };
        let mut guard = match emitter.lock() {
            Ok(g) => g,
            Err(poison) => poison.into_inner(),
        };
        let envelope = guard.envelope_builder.build_with_source(Source::Driver);
        let event = AgentEvent::DriverEvent {
            envelope,
            driver_kind: DriverKind::TokenUsage,
            summary: format!(
                "{} input={} output={} cache_read={} cache_write={}",
                model_id_to_provider_name(model),
                usage.input,
                usage.output,
                usage.cache_read,
                usage.cache_write,
            ),
            payload: serde_json::json!({
                "model": model_id_to_provider_name(model),
                "input": usage.input,
                "output": usage.output,
                "cache_read": usage.cache_read,
                "cache_write": usage.cache_write,
            }),
        };
        guard.sink.emit(&event);
    }
}

impl LlmClient for Client {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let model = req.model.clone();
        let model_name = model_id_to_provider_name(&model);
        let (chat_req, options) = to_genai_chat_request(req);
        let resp = self
            .inner
            .exec_chat(&model_name, chat_req, Some(&options))
            .await
            .map_err(|err| LlmError::Provider {
                message: err.to_string(),
            })?;
        let response = from_genai_chat_response(resp);
        self.emit_usage(&model, &response.usage);
        Ok(response)
    }

    async fn complete_structured<T>(&self, req: CompletionRequest) -> Result<T, LlmError>
    where
        T: DeserializeOwned + JsonSchema + Send,
    {
        let model = req.model.clone();
        let model_name = model_id_to_provider_name(&model);
        let (chat_req, options) = to_genai_structured_chat_options::<T>(req);
        let resp = self
            .inner
            .exec_chat(&model_name, chat_req, Some(&options))
            .await
            .map_err(|err| LlmError::Provider {
                message: err.to_string(),
            })?;
        let completion = from_genai_chat_response(resp);
        self.emit_usage(&model, &completion.usage);
        parse_structured_text::<T>(&completion.text)
    }
}

/// Lower a [`CompletionRequest`] to the genai chat request + options pair
/// and attach `T`'s JSON schema as the structured-output spec. The same
/// `JsonSpec` round-trips through every adapter — Anthropic's
/// `output_config.format = json_schema`, OpenAI's `response_format =
/// json_schema`, and Gemini's `responseMimeType = "application/json"` +
/// `responseJsonSchema` — so the provider mechanism is hidden behind one
/// call shape and only the `ModelId` variant decides routing.
pub(crate) fn to_genai_structured_chat_options<T: JsonSchema>(
    req: CompletionRequest,
) -> (ChatRequest, ChatOptions) {
    let (chat_req, mut options) = to_genai_chat_request(req);
    options.response_format = Some(ChatResponseFormat::JsonSpec(JsonSpec::new(
        json_spec_name::<T>(),
        json_schema_value::<T>(),
    )));
    (chat_req, options)
}

fn json_schema_value<T: JsonSchema>() -> serde_json::Value {
    SchemaGenerator::default()
        .into_root_schema_for::<T>()
        .to_value()
}

/// Sanitize `T::schema_name()` for the OpenAI structured-output API,
/// which only accepts ASCII alphanumerics plus `-` and `_`. The other
/// adapters ignore the name field, so the same sanitization is safe
/// across providers.
fn json_spec_name<T: JsonSchema>() -> String {
    T::schema_name()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub(crate) fn parse_structured_text<T: DeserializeOwned>(text: &str) -> Result<T, LlmError> {
    serde_json::from_str(text).map_err(|err| LlmError::MalformedJson(err.to_string()))
}

/// Map a typed [`ModelId`] to the underlying crate's model-name string.
/// Delegates to [`ModelId::as_wire`] so the canonical mapping lives on
/// the public type.
pub(crate) fn model_id_to_provider_name(model: &ModelId) -> String {
    model.as_wire()
}

pub(crate) fn to_genai_chat_request(req: CompletionRequest) -> (ChatRequest, ChatOptions) {
    let CompletionRequest {
        model: _,
        system,
        messages,
        max_tokens,
        tools,
    } = req;

    let chat_messages: Vec<ChatMessage> = messages.into_iter().map(to_chat_message).collect();

    let mut chat_req = ChatRequest::new(chat_messages);
    if let Some(prefix) = system {
        chat_req = chat_req.with_system(prefix);
    }
    if !tools.is_empty() {
        chat_req = chat_req.with_tools(tools.into_iter().map(to_genai_tool));
    }

    let mut options = ChatOptions::default();
    if let Some(n) = max_tokens {
        options.max_tokens = Some(n);
    }

    (chat_req, options)
}

fn to_genai_tool(def: ToolDef) -> GenAiTool {
    GenAiTool::new(def.name)
        .with_description(def.description)
        .with_schema(def.input_schema)
}

fn to_chat_message(msg: Message) -> ChatMessage {
    let role = match msg.role {
        Role::User => ChatRole::User,
        Role::Assistant => ChatRole::Assistant,
        Role::Tool => ChatRole::Tool,
    };

    let content = build_message_content(&msg);
    let mut chat_msg = ChatMessage::new(role, content);
    if let Some(cache) = cache_control_to_genai(&msg.cache) {
        let opts = MessageOptions::default().with_cache_control(cache);
        chat_msg = chat_msg.with_options(opts);
    }
    chat_msg
}

fn build_message_content(msg: &Message) -> MessageContent {
    if let Some(call_id) = msg.tool_call_id.as_ref() {
        let response = GenAiToolResponse::new(call_id.clone(), msg.content.clone());
        return MessageContent::from(response);
    }
    if !msg.tool_calls.is_empty() {
        let calls: Vec<GenAiToolCall> = msg.tool_calls.iter().map(to_genai_tool_call).collect();
        let mut content = MessageContent::from_tool_calls(calls);
        if !msg.content.is_empty() {
            content.prepend(genai::chat::ContentPart::Text(msg.content.clone()));
        }
        return content;
    }
    MessageContent::from_text(msg.content.clone())
}

fn to_genai_tool_call(call: &ToolUseRequest) -> GenAiToolCall {
    GenAiToolCall {
        call_id: call.call_id.clone(),
        fn_name: call.name.clone(),
        fn_arguments: call.args.clone(),
        thought_signatures: None,
    }
}

fn from_genai_tool_call(call: &GenAiToolCall) -> ToolUseRequest {
    ToolUseRequest {
        call_id: call.call_id.clone(),
        name: call.fn_name.clone(),
        args: call.fn_arguments.clone(),
    }
}

pub(crate) fn cache_control_to_genai(cache: &CacheControl) -> Option<GenAiCacheControl> {
    match cache {
        CacheControl::None => None,
        CacheControl::Ephemeral(CacheTtl::Minutes5) => Some(GenAiCacheControl::Ephemeral5m),
        CacheControl::Ephemeral(CacheTtl::Hours1) => Some(GenAiCacheControl::Ephemeral1h),
        CacheControl::Ephemeral(CacheTtl::Hours24) => Some(GenAiCacheControl::Ephemeral24h),
    }
}

pub(crate) fn from_genai_chat_response(resp: ChatResponse) -> CompletionResponse {
    let usage = from_genai_usage(&resp.usage);
    let tool_calls: Vec<ToolUseRequest> = resp
        .tool_calls()
        .into_iter()
        .map(from_genai_tool_call)
        .collect();
    let text = resp.into_first_text().unwrap_or_default();
    CompletionResponse {
        text,
        usage,
        tool_calls,
    }
}

fn from_genai_usage(usage: &GenAiUsage) -> TokenUsage {
    let input = usage.prompt_tokens.unwrap_or(0).max(0) as u32;
    let output = usage.completion_tokens.unwrap_or(0).max(0) as u32;
    let (cache_read, cache_write) =
        usage
            .prompt_tokens_details
            .as_ref()
            .map_or((0_u32, 0_u32), |details| {
                (
                    details.cached_tokens.unwrap_or(0).max(0) as u32,
                    details.cache_creation_tokens.unwrap_or(0).max(0) as u32,
                )
            });
    TokenUsage {
        input,
        output,
        cache_read,
        cache_write,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheTtl;
    use crate::model_id::{AnthropicModel, GeminiModel, OpenAiModel};
    use genai::ModelIden;
    use genai::adapter::AdapterKind;
    use genai::chat::{PromptTokensDetails, Usage as GenAiUsage};
    use loom_events::identifier::BeadId;
    use std::sync::{Arc, Mutex};

    /// `loom_llm::Client` is `Send + Sync + 'static` — required for the
    /// `LlmClient: Send + Sync` bound and for spawning across tokio
    /// tasks.
    #[test]
    fn client_is_send_sync_static() {
        fn assert_bounds<T: Send + Sync + 'static>() {}
        assert_bounds::<Client>();
    }

    /// `Client` impls `LlmClient` — the trait bound is satisfied at the
    /// type-check level. The dummy generic forces monomorphization
    /// against the concrete type.
    #[test]
    fn client_impls_llm_client() {
        fn assert_impl<T: LlmClient>() {}
        assert_impl::<Client>();
    }

    /// Known `ModelId` variants resolve to the documented provider
    /// model strings; the `Other(String)` arm passes through so
    /// consumers can name not-yet-listed models without a minor bump.
    #[test]
    fn model_id_to_provider_name_maps_known_and_other_variants() {
        assert_eq!(
            model_id_to_provider_name(&ModelId::Anthropic(AnthropicModel::ClaudeOpus47)),
            "claude-opus-4-7"
        );
        assert_eq!(
            model_id_to_provider_name(&ModelId::Anthropic(AnthropicModel::ClaudeSonnet46)),
            "claude-sonnet-4-6"
        );
        assert_eq!(
            model_id_to_provider_name(&ModelId::Anthropic(AnthropicModel::ClaudeHaiku45)),
            "claude-haiku-4-5"
        );
        assert_eq!(
            model_id_to_provider_name(&ModelId::OpenAi(OpenAiModel::Gpt55)),
            "gpt-5.5"
        );
        assert_eq!(
            model_id_to_provider_name(&ModelId::Gemini(GeminiModel::Gemini31Pro)),
            "gemini-3.1-pro"
        );
        assert_eq!(
            model_id_to_provider_name(&ModelId::Gemini(GeminiModel::Gemini35Flash)),
            "gemini-3.5-flash"
        );
        let custom = "claude-3-7-sonnet-future";
        assert_eq!(
            model_id_to_provider_name(&ModelId::Anthropic(AnthropicModel::Other(
                custom.to_string()
            ))),
            custom,
        );
    }

    /// Every documented `CacheTtl` maps to the matching genai variant;
    /// `CacheControl::None` lowers to no `MessageOptions` so the wire
    /// payload remains pristine when no cache breakpoint is requested.
    #[test]
    fn cache_control_lowers_to_matching_genai_variant() {
        assert_eq!(cache_control_to_genai(&CacheControl::None), None);
        assert_eq!(
            cache_control_to_genai(&CacheControl::Ephemeral(CacheTtl::Minutes5)),
            Some(GenAiCacheControl::Ephemeral5m),
        );
        assert_eq!(
            cache_control_to_genai(&CacheControl::Ephemeral(CacheTtl::Hours1)),
            Some(GenAiCacheControl::Ephemeral1h),
        );
        assert_eq!(
            cache_control_to_genai(&CacheControl::Ephemeral(CacheTtl::Hours24)),
            Some(GenAiCacheControl::Ephemeral24h),
        );
    }

    /// Cache markers land on the matching message in the lowered
    /// `ChatRequest`, so the Anthropic adapter places the cache
    /// breakpoint at the per-content-block position the consumer
    /// chose. Adjacent uncached messages carry no `MessageOptions` so
    /// the wire payload reflects the per-block precision the typed
    /// surface promises.
    #[test]
    fn message_text_cached_marks_per_block_in_anthropic_request() {
        let req = CompletionRequest::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .system("be terse")
            .user("hi")
            .user_cached("doc", CacheControl::Ephemeral(CacheTtl::Hours1))
            .max_tokens(512);

        let (chat_req, options) = to_genai_chat_request(req);

        assert_eq!(chat_req.system.as_deref(), Some("be terse"));
        assert_eq!(chat_req.messages.len(), 2);
        assert_eq!(chat_req.messages[0].role, ChatRole::User);
        assert!(chat_req.messages[0].options.is_none());
        assert_eq!(chat_req.messages[1].role, ChatRole::User);
        let cache = chat_req.messages[1]
            .options
            .as_ref()
            .and_then(|o| o.cache_control.as_ref())
            .expect("second message carries cache_control");
        assert_eq!(cache, &GenAiCacheControl::Ephemeral1h);
        assert_eq!(options.max_tokens, Some(512));
    }

    /// Lowering a cache-marked request when the request targets an
    /// OpenAI model is a no-op error path: the marker is carried into
    /// the lowered representation (the underlying adapter is free to
    /// drop it), and the conversion never fails. The wrapper does not
    /// silently strip the marker either — the per-provider decision
    /// lives in the underlying adapter, not in this crate.
    #[test]
    fn cache_marker_no_ops_on_openai_provider() {
        let req = CompletionRequest::new(ModelId::OpenAi(OpenAiModel::Gpt55))
            .user("hi")
            .user_cached("doc", CacheControl::Ephemeral(CacheTtl::Minutes5));

        let (chat_req, _) = to_genai_chat_request(req);
        assert_eq!(chat_req.messages.len(), 2);
        let cache = chat_req.messages[1]
            .options
            .as_ref()
            .and_then(|o| o.cache_control.as_ref())
            .expect("cache marker is preserved through lowering");
        assert_eq!(cache, &GenAiCacheControl::Ephemeral5m);
    }

    fn make_usage(prompt: i32, completion: i32, cached: i32, cache_creation: i32) -> GenAiUsage {
        GenAiUsage {
            prompt_tokens: Some(prompt),
            completion_tokens: Some(completion),
            total_tokens: Some(prompt + completion),
            prompt_tokens_details: Some(PromptTokensDetails {
                cache_creation_tokens: Some(cache_creation),
                cached_tokens: Some(cached),
                ..PromptTokensDetails::default()
            }),
            completion_tokens_details: None,
        }
    }

    fn make_response(usage: GenAiUsage) -> ChatResponse {
        let model = ModelIden::new(AdapterKind::Anthropic, "claude-sonnet-4-6");
        ChatResponse {
            content: MessageContent::from_text("the answer"),
            reasoning_content: None,
            model_iden: model.clone(),
            provider_model_iden: model,
            stop_reason: None,
            usage,
            captured_raw_body: None,
            response_id: None,
        }
    }

    /// `ChatResponse → CompletionResponse` carries the final text and
    /// the four-field `TokenUsage`: `input`, `output`, `cache_read`,
    /// `cache_write`. The cache fields are pulled from the provider's
    /// prompt-tokens detail block. Pricing is consumer-owned (see
    /// `specs/llm.md` § TokenUsage); the exhaustive destructure below
    /// would fail to compile if a fifth field appeared on the struct.
    #[test]
    fn completion_response_carries_token_usage_without_cost() {
        let usage = make_usage(1_000, 250, 600, 400);
        let resp = make_response(usage);

        let completion = from_genai_chat_response(resp);
        assert_eq!(completion.text, "the answer");
        let TokenUsage {
            input,
            output,
            cache_read,
            cache_write,
        } = completion.usage;
        assert_eq!(input, 1_000);
        assert_eq!(output, 250);
        assert_eq!(cache_read, 600);
        assert_eq!(cache_write, 400);
    }

    /// `complete_structured::<T>` lowers a `CompletionRequest` into a
    /// genai `ChatOptions` whose `response_format = JsonSpec` carries
    /// `T`'s JSON schema regardless of which provider the request's
    /// `ModelId` routes to. The same lowering function is invoked on
    /// the Anthropic, OpenAI, and Gemini paths, so consumers never see
    /// the provider mechanism — switching providers is a `ModelId`
    /// variant change.
    #[test]
    fn complete_structured_attaches_json_schema_for_every_provider() {
        #[derive(serde::Deserialize, schemars::JsonSchema)]
        #[expect(
            dead_code,
            reason = "fields are referenced through schemars-derived schema, not by Rust code"
        )]
        struct AnswerShape {
            title: String,
            count: u32,
        }

        let models = [
            ModelId::Anthropic(AnthropicModel::ClaudeSonnet46),
            ModelId::OpenAi(OpenAiModel::Gpt55),
            ModelId::Gemini(GeminiModel::Gemini31Pro),
        ];
        for model in models {
            let req = CompletionRequest::new(model.clone()).user("structured please");
            let (_chat_req, options) = to_genai_structured_chat_options::<AnswerShape>(req);
            let format = options
                .response_format
                .as_ref()
                .expect("response_format set for structured call");
            let ChatResponseFormat::JsonSpec(spec) = format else {
                panic!("expected JsonSpec response format for {model:?}, got {format:?}");
            };
            let props = spec
                .schema
                .get("properties")
                .and_then(|v| v.as_object())
                .expect("schema carries object properties");
            assert!(props.contains_key("title"), "schema names `title` field");
            assert!(props.contains_key("count"), "schema names `count` field");
        }
    }

    /// `parse_structured_text::<T>` deserializes the same canonical JSON
    /// payload into the same `T` regardless of which provider produced
    /// it. The downstream `complete_structured` code path treats all
    /// three adapters identically — only the `ModelId`-driven route
    /// through the underlying crate differs, so the "same call shape,
    /// same returned `T`" promise holds across Anthropic, OpenAI, and
    /// Gemini.
    #[test]
    fn complete_structured_returns_typed_t_across_providers() {
        #[derive(Debug, PartialEq, serde::Deserialize, schemars::JsonSchema)]
        struct AnswerShape {
            title: String,
            count: u32,
        }

        let json_text = r#"{"title":"forty-two","count":42}"#;
        let providers = [
            (
                ModelId::Anthropic(AnthropicModel::ClaudeSonnet46),
                AdapterKind::Anthropic,
            ),
            (ModelId::OpenAi(OpenAiModel::Gpt55), AdapterKind::OpenAI),
            (
                ModelId::Gemini(GeminiModel::Gemini31Pro),
                AdapterKind::Gemini,
            ),
        ];
        let expected = AnswerShape {
            title: "forty-two".to_string(),
            count: 42,
        };

        for (model_id, adapter_kind) in providers {
            let resp = make_text_response(
                adapter_kind,
                &model_id_to_provider_name(&model_id),
                json_text,
            );
            let completion = from_genai_chat_response(resp);
            let value: AnswerShape =
                parse_structured_text(&completion.text).expect("structured payload parses");
            assert_eq!(value, expected, "same T across providers: {adapter_kind:?}",);
        }
    }

    fn make_text_response(adapter: AdapterKind, model_name: &str, text: &str) -> ChatResponse {
        let model = ModelIden::new(adapter, model_name.to_string());
        ChatResponse {
            content: MessageContent::from_text(text),
            reasoning_content: None,
            model_iden: model.clone(),
            provider_model_iden: model,
            stop_reason: None,
            usage: GenAiUsage::default(),
            captured_raw_body: None,
            response_id: None,
        }
    }

    /// Recording sink that snapshots every emitted event so tests can
    /// assert on the driver-event payload reaching the active chain.
    #[derive(Clone, Default)]
    struct RecordingSink {
        events: Arc<Mutex<Vec<AgentEvent>>>,
    }

    impl EventSink for RecordingSink {
        fn emit(&mut self, event: &AgentEvent) {
            self.events
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(event.clone());
        }
    }

    fn test_envelope_builder() -> EnvelopeBuilder {
        let mut clock = 0_i64;
        EnvelopeBuilder::new(
            BeadId::new("lm-test").expect("valid bead id"),
            None,
            0,
            Source::Agent,
            move || {
                clock += 1;
                clock
            },
        )
    }

    /// Every successful `complete*` call emits a
    /// `DriverKind::TokenUsage` driver event into the configured sink
    /// chain. The event's payload carries the four-field `TokenUsage`
    /// plus the model identifier so SaaS billing pipelines see cache
    /// hits and compute their own per-tenant cost from the raw counts.
    ///
    /// `complete()` itself requires a live provider, so this test
    /// invokes the private emission helper that `complete()` calls
    /// after every successful response — the same code path, exercised
    /// without the network round-trip.
    #[test]
    fn complete_emits_token_usage_driver_event() {
        let sink = RecordingSink::default();
        let recorded = sink.events.clone();
        let client = Client::new().with_event_sink(sink, test_envelope_builder());

        let usage = TokenUsage {
            input: 1_000,
            output: 250,
            cache_read: 600,
            cache_write: 400,
        };
        client.emit_usage(&ModelId::Anthropic(AnthropicModel::ClaudeSonnet46), &usage);

        let events = recorded.lock().expect("recording sink mutex");
        assert_eq!(events.len(), 1, "exactly one driver event emitted");
        match &events[0] {
            AgentEvent::DriverEvent {
                envelope,
                driver_kind,
                payload,
                summary,
            } => {
                assert_eq!(*driver_kind, DriverKind::TokenUsage);
                assert_eq!(envelope.source, Source::Driver);
                assert_eq!(payload["model"], "claude-sonnet-4-6");
                assert_eq!(payload["input"], 1_000);
                assert_eq!(payload["output"], 250);
                assert_eq!(payload["cache_read"], 600);
                assert_eq!(payload["cache_write"], 400);
                let obj = payload.as_object().expect("payload is JSON object");
                let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
                keys.sort_unstable();
                assert_eq!(
                    keys,
                    vec!["cache_read", "cache_write", "input", "model", "output"],
                    "payload pins the spec'd field set with no extras",
                );
                assert!(
                    summary.contains("claude-sonnet-4-6"),
                    "summary names the model: {summary}",
                );
            }
            other => panic!("expected DriverEvent, got {other:?}"),
        }
    }

    /// When no sink is attached the emission silently drops the event
    /// — there is no global state, no logging fallback, and no
    /// observable side effect. Calling `emit_usage` on a sinkless
    /// client must be a no-op.
    #[test]
    fn emit_usage_is_silent_drop_when_no_sink_attached() {
        let client = Client::new();
        let usage = TokenUsage {
            input: 100,
            output: 10,
            cache_read: 0,
            cache_write: 0,
        };
        client.emit_usage(&ModelId::Anthropic(AnthropicModel::ClaudeSonnet46), &usage);
    }
}

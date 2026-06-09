//! Per-schema `LlmClient` impls on top of the multi-provider `genai`
//! crate.
//!
//! Each [`SchemaKind`] gets a dedicated Client type: [`AnthropicClient`],
//! [`OpenAiClient`], [`GeminiClient`]. The three genai-backed Clients
//! share a process-wide [`genai::Client`] via [`shared_genai_client`] so
//! connection pooling and rate-limit tracking work across schemas in the
//! same process. No public signature mentions `genai::Client` — the
//! wrapper insulates consumers from the underlying crate's API churn.

use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine;
use genai::chat::{
    CacheControl as GenAiCacheControl, ChatMessage, ChatOptions, ChatRequest, ChatResponse,
    ChatResponseFormat, ChatRole, ContentPart, JsonSpec, MessageContent as GenAiMessageContent,
    MessageOptions, Tool as GenAiTool, ToolCall as GenAiToolCall,
    ToolResponse as GenAiToolResponse, Usage as GenAiUsage,
};
use loom_events::event::Source;
use loom_events::{AgentEvent, DriverKind, EnvelopeBuilder, EventSink};
#[cfg(test)]
use schemars::{JsonSchema, SchemaGenerator};
#[cfg(test)]
use serde::de::DeserializeOwned;

use crate::api_key::ApiKey;
use crate::cache::{CacheControl, CacheTtl};
use crate::client::{BoxFuture, CompletionResponse, LlmClient, LlmError, ToolUseRequest};
use crate::model_id::{ModelId, SchemaKind};
use crate::request::{CompletionRequest, Message, MessageContent, Role};
use crate::tool::ToolDef;
use crate::usage::TokenUsage;

/// Process-wide shared `genai::Client`. Built once on first use and
/// handed out as `Arc::clone` to every per-schema Client so connection
/// pooling and rate-limit tracking flow across schemas in the same
/// process. Constructing a fresh `genai::Client` per Client type would
/// defeat both.
fn shared_genai_client() -> Arc<genai::Client> {
    static CELL: OnceLock<Arc<genai::Client>> = OnceLock::new();
    CELL.get_or_init(|| Arc::new(genai::Client::default()))
        .clone()
}

/// Client targeting the [`SchemaKind::Anthropic`] schema. Built on top
/// of the shared [`genai::Client`].
pub struct AnthropicClient {
    inner: Arc<genai::Client>,
    api_key: ApiKey,
    sinks: Mutex<Vec<Box<dyn EventSink>>>,
    envelope_builder: Mutex<Option<EnvelopeBuilder>>,
}

impl AnthropicClient {
    /// Wire-format discriminator this Client targets. Fixed at
    /// construction; per-call selection varies the `ModelId` within
    /// this schema.
    pub const SCHEMA: SchemaKind = SchemaKind::Anthropic;

    /// Construct a Client carrying `api_key` as its credential and
    /// sharing the process-wide [`genai::Client`].
    pub fn new(api_key: ApiKey) -> Self {
        Self {
            inner: shared_genai_client(),
            api_key,
            sinks: Mutex::new(Vec::new()),
            envelope_builder: Mutex::new(None),
        }
    }

    /// Attach an [`EventSink`] to this Client's chain. Each call
    /// appends; multiple calls compose. Every successful `complete*`
    /// call fans a [`DriverKind::TokenUsage`] [`AgentEvent`] into every
    /// attached sink in registration order.
    pub fn with_event_sink<S>(self, sink: S) -> Self
    where
        S: EventSink + 'static,
    {
        push_sink(&self.sinks, Box::new(sink));
        self
    }

    /// Replace the synthetic [`EnvelopeBuilder`] with one that stamps
    /// the caller's bead / molecule / iteration identity onto every
    /// emitted event.
    pub fn with_envelope_builder(self, envelope_builder: EnvelopeBuilder) -> Self {
        set_envelope_builder(&self.envelope_builder, envelope_builder);
        self
    }

    /// Borrow the credential the Client was constructed with. Callers
    /// SHOULD NOT log or emit this value; per RS-15 the wrapped string
    /// is meant for wire-level auth resolvers only.
    pub fn api_key(&self) -> &ApiKey {
        &self.api_key
    }

    fn emit_usage(&self, model: &ModelId, usage: &TokenUsage) {
        emit_usage_to_chain(&self.envelope_builder, &self.sinks, model, usage);
    }
}

impl std::fmt::Debug for AnthropicClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        debug_per_schema_client(f, "AnthropicClient", Self::SCHEMA, &self.sinks)
    }
}

impl LlmClient for AnthropicClient {
    fn schema(&self) -> SchemaKind {
        Self::SCHEMA
    }

    fn complete<'a>(
        &'a self,
        req: CompletionRequest,
    ) -> BoxFuture<'a, Result<CompletionResponse, LlmError>> {
        let model = req.model.clone();
        if model.schema() != Self::SCHEMA {
            return Box::pin(async move {
                Err(LlmError::IncompatibleModel {
                    model,
                    expected: Self::SCHEMA,
                })
            });
        }
        if let Err(err) = validate_binary_payloads(&req) {
            return Box::pin(async move { Err(err) });
        }
        Box::pin(async move {
            let model_name = model_id_to_provider_name(&model);
            let (chat_req, options) = to_genai_chat_request(req);
            let resp = self
                .inner
                .exec_chat(&model_name, chat_req, Some(&options))
                .await
                .map_err(|err| LlmError::Provider {
                    message: err.to_string(),
                })?;
            let response = chat_response_to_completion(resp);
            self.emit_usage(&model, &response.usage);
            Ok(response)
        })
    }

    fn complete_structured_raw<'a>(
        &'a self,
        req: CompletionRequest,
        schema: serde_json::Value,
        type_name: String,
    ) -> BoxFuture<'a, Result<String, LlmError>> {
        let model = req.model.clone();
        if model.schema() != Self::SCHEMA {
            return Box::pin(async move {
                Err(LlmError::IncompatibleModel {
                    model,
                    expected: Self::SCHEMA,
                })
            });
        }
        if let Err(err) = validate_binary_payloads(&req) {
            return Box::pin(async move { Err(err) });
        }
        Box::pin(async move {
            let model_name = model_id_to_provider_name(&model);
            let (chat_req, options) = to_genai_structured_chat_options_raw(req, schema, type_name);
            let resp = self
                .inner
                .exec_chat(&model_name, chat_req, Some(&options))
                .await
                .map_err(|err| LlmError::Provider {
                    message: err.to_string(),
                })?;
            let completion = chat_response_to_completion(resp);
            self.emit_usage(&model, &completion.usage);
            Ok(completion.text)
        })
    }
}

/// Client targeting the [`SchemaKind::OpenAi`] schema. Built on top of
/// the shared [`genai::Client`].
pub struct OpenAiClient {
    inner: Arc<genai::Client>,
    api_key: ApiKey,
    sinks: Mutex<Vec<Box<dyn EventSink>>>,
    envelope_builder: Mutex<Option<EnvelopeBuilder>>,
}

impl OpenAiClient {
    /// Wire-format discriminator this Client targets.
    pub const SCHEMA: SchemaKind = SchemaKind::OpenAi;

    /// Construct a Client carrying `api_key` and sharing the process-
    /// wide [`genai::Client`].
    pub fn new(api_key: ApiKey) -> Self {
        Self {
            inner: shared_genai_client(),
            api_key,
            sinks: Mutex::new(Vec::new()),
            envelope_builder: Mutex::new(None),
        }
    }

    /// Attach an [`EventSink`] to this Client's chain.
    pub fn with_event_sink<S>(self, sink: S) -> Self
    where
        S: EventSink + 'static,
    {
        push_sink(&self.sinks, Box::new(sink));
        self
    }

    /// Replace the synthetic [`EnvelopeBuilder`] with one that stamps
    /// the caller's identity onto every emitted event.
    pub fn with_envelope_builder(self, envelope_builder: EnvelopeBuilder) -> Self {
        set_envelope_builder(&self.envelope_builder, envelope_builder);
        self
    }

    /// Borrow the credential the Client was constructed with.
    pub fn api_key(&self) -> &ApiKey {
        &self.api_key
    }

    fn emit_usage(&self, model: &ModelId, usage: &TokenUsage) {
        emit_usage_to_chain(&self.envelope_builder, &self.sinks, model, usage);
    }
}

impl std::fmt::Debug for OpenAiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        debug_per_schema_client(f, "OpenAiClient", Self::SCHEMA, &self.sinks)
    }
}

impl LlmClient for OpenAiClient {
    fn schema(&self) -> SchemaKind {
        Self::SCHEMA
    }

    fn complete<'a>(
        &'a self,
        req: CompletionRequest,
    ) -> BoxFuture<'a, Result<CompletionResponse, LlmError>> {
        let model = req.model.clone();
        if model.schema() != Self::SCHEMA {
            return Box::pin(async move {
                Err(LlmError::IncompatibleModel {
                    model,
                    expected: Self::SCHEMA,
                })
            });
        }
        if let Err(err) = validate_binary_payloads(&req) {
            return Box::pin(async move { Err(err) });
        }
        Box::pin(async move {
            let model_name = model_id_to_provider_name(&model);
            let (chat_req, options) = to_genai_chat_request(req);
            let resp = self
                .inner
                .exec_chat(&model_name, chat_req, Some(&options))
                .await
                .map_err(|err| LlmError::Provider {
                    message: err.to_string(),
                })?;
            let response = chat_response_to_completion(resp);
            self.emit_usage(&model, &response.usage);
            Ok(response)
        })
    }

    fn complete_structured_raw<'a>(
        &'a self,
        req: CompletionRequest,
        schema: serde_json::Value,
        type_name: String,
    ) -> BoxFuture<'a, Result<String, LlmError>> {
        let model = req.model.clone();
        if model.schema() != Self::SCHEMA {
            return Box::pin(async move {
                Err(LlmError::IncompatibleModel {
                    model,
                    expected: Self::SCHEMA,
                })
            });
        }
        if let Err(err) = validate_binary_payloads(&req) {
            return Box::pin(async move { Err(err) });
        }
        Box::pin(async move {
            let model_name = model_id_to_provider_name(&model);
            let (chat_req, options) = to_genai_structured_chat_options_raw(req, schema, type_name);
            let resp = self
                .inner
                .exec_chat(&model_name, chat_req, Some(&options))
                .await
                .map_err(|err| LlmError::Provider {
                    message: err.to_string(),
                })?;
            let completion = chat_response_to_completion(resp);
            self.emit_usage(&model, &completion.usage);
            Ok(completion.text)
        })
    }
}

/// Client targeting the [`SchemaKind::Gemini`] schema. Built on top of
/// the shared [`genai::Client`].
pub struct GeminiClient {
    inner: Arc<genai::Client>,
    api_key: ApiKey,
    sinks: Mutex<Vec<Box<dyn EventSink>>>,
    envelope_builder: Mutex<Option<EnvelopeBuilder>>,
}

impl GeminiClient {
    /// Wire-format discriminator this Client targets.
    pub const SCHEMA: SchemaKind = SchemaKind::Gemini;

    /// Construct a Client carrying `api_key` and sharing the process-
    /// wide [`genai::Client`].
    pub fn new(api_key: ApiKey) -> Self {
        Self {
            inner: shared_genai_client(),
            api_key,
            sinks: Mutex::new(Vec::new()),
            envelope_builder: Mutex::new(None),
        }
    }

    /// Attach an [`EventSink`] to this Client's chain.
    pub fn with_event_sink<S>(self, sink: S) -> Self
    where
        S: EventSink + 'static,
    {
        push_sink(&self.sinks, Box::new(sink));
        self
    }

    /// Replace the synthetic [`EnvelopeBuilder`] with one that stamps
    /// the caller's identity onto every emitted event.
    pub fn with_envelope_builder(self, envelope_builder: EnvelopeBuilder) -> Self {
        set_envelope_builder(&self.envelope_builder, envelope_builder);
        self
    }

    /// Borrow the credential the Client was constructed with.
    pub fn api_key(&self) -> &ApiKey {
        &self.api_key
    }

    fn emit_usage(&self, model: &ModelId, usage: &TokenUsage) {
        emit_usage_to_chain(&self.envelope_builder, &self.sinks, model, usage);
    }
}

impl std::fmt::Debug for GeminiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        debug_per_schema_client(f, "GeminiClient", Self::SCHEMA, &self.sinks)
    }
}

impl LlmClient for GeminiClient {
    fn schema(&self) -> SchemaKind {
        Self::SCHEMA
    }

    fn complete<'a>(
        &'a self,
        req: CompletionRequest,
    ) -> BoxFuture<'a, Result<CompletionResponse, LlmError>> {
        let model = req.model.clone();
        if model.schema() != Self::SCHEMA {
            return Box::pin(async move {
                Err(LlmError::IncompatibleModel {
                    model,
                    expected: Self::SCHEMA,
                })
            });
        }
        if let Err(err) = validate_binary_payloads(&req) {
            return Box::pin(async move { Err(err) });
        }
        Box::pin(async move {
            let model_name = model_id_to_provider_name(&model);
            let (chat_req, options) = to_genai_chat_request(req);
            let resp = self
                .inner
                .exec_chat(&model_name, chat_req, Some(&options))
                .await
                .map_err(|err| LlmError::Provider {
                    message: err.to_string(),
                })?;
            let response = chat_response_to_completion(resp);
            self.emit_usage(&model, &response.usage);
            Ok(response)
        })
    }

    fn complete_structured_raw<'a>(
        &'a self,
        req: CompletionRequest,
        schema: serde_json::Value,
        type_name: String,
    ) -> BoxFuture<'a, Result<String, LlmError>> {
        let model = req.model.clone();
        if model.schema() != Self::SCHEMA {
            return Box::pin(async move {
                Err(LlmError::IncompatibleModel {
                    model,
                    expected: Self::SCHEMA,
                })
            });
        }
        if let Err(err) = validate_binary_payloads(&req) {
            return Box::pin(async move { Err(err) });
        }
        Box::pin(async move {
            let model_name = model_id_to_provider_name(&model);
            let (chat_req, options) = to_genai_structured_chat_options_raw(req, schema, type_name);
            let resp = self
                .inner
                .exec_chat(&model_name, chat_req, Some(&options))
                .await
                .map_err(|err| LlmError::Provider {
                    message: err.to_string(),
                })?;
            let completion = chat_response_to_completion(resp);
            self.emit_usage(&model, &completion.usage);
            Ok(completion.text)
        })
    }
}

pub(super) fn push_sink(sinks: &Mutex<Vec<Box<dyn EventSink>>>, sink: Box<dyn EventSink>) {
    sinks.lock().unwrap_or_else(|p| p.into_inner()).push(sink);
}

pub(super) fn set_envelope_builder(
    slot: &Mutex<Option<EnvelopeBuilder>>,
    envelope_builder: EnvelopeBuilder,
) {
    *slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(envelope_builder);
}

pub(super) fn emit_usage_to_chain(
    envelope_builder: &Mutex<Option<EnvelopeBuilder>>,
    sinks: &Mutex<Vec<Box<dyn EventSink>>>,
    model: &ModelId,
    usage: &TokenUsage,
) {
    let envelope = {
        let mut guard = envelope_builder.lock().unwrap_or_else(|p| p.into_inner());
        match guard.as_mut() {
            Some(builder) => builder.build_with_source(Source::Driver),
            None => return,
        }
    };
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
    let mut guard = sinks.lock().unwrap_or_else(|p| p.into_inner());
    for sink in guard.iter_mut() {
        sink.emit(&event);
    }
}

pub(super) fn debug_per_schema_client(
    f: &mut std::fmt::Formatter<'_>,
    name: &str,
    schema: SchemaKind,
    sinks: &Mutex<Vec<Box<dyn EventSink>>>,
) -> std::fmt::Result {
    let count = sinks
        .lock()
        .map(|g| g.len())
        .unwrap_or_else(|p| p.into_inner().len());
    f.debug_struct(name)
        .field("schema", &schema)
        .field("sinks_attached", &count)
        .finish()
}

/// Lower a [`CompletionRequest`] to the genai chat request + options
/// pair and attach the supplied JSON schema as the structured-output
/// spec. The same `JsonSpec` round-trips through every adapter —
/// Anthropic's `output_config.format = json_schema`, OpenAI's
/// `response_format = json_schema`, and Gemini's
/// `responseMimeType = "application/json"` + `responseJsonSchema` — so
/// the provider mechanism is hidden behind one call shape and only the
/// `ModelId` variant decides routing.
pub(crate) fn to_genai_structured_chat_options_raw(
    req: CompletionRequest,
    schema: serde_json::Value,
    type_name: String,
) -> (ChatRequest, ChatOptions) {
    let (chat_req, mut options) = to_genai_chat_request(req);
    options.response_format = Some(ChatResponseFormat::JsonSpec(JsonSpec::new(
        type_name, schema,
    )));
    (chat_req, options)
}

#[cfg(test)]
pub(crate) fn to_genai_structured_chat_options<T: JsonSchema>(
    req: CompletionRequest,
) -> (ChatRequest, ChatOptions) {
    let schema = SchemaGenerator::default()
        .into_root_schema_for::<T>()
        .to_value();
    let type_name = json_spec_name::<T>();
    to_genai_structured_chat_options_raw(req, schema, type_name)
}

#[cfg(test)]
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

#[cfg(test)]
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

fn build_message_content(msg: &Message) -> GenAiMessageContent {
    if let Some(call_id) = msg.tool_call_id.as_ref() {
        let response = GenAiToolResponse::new(call_id.clone(), msg.text_content());
        return GenAiMessageContent::from(response);
    }
    if !msg.tool_calls.is_empty() {
        let calls: Vec<GenAiToolCall> = msg.tool_calls.iter().map(to_genai_tool_call).collect();
        let mut content = GenAiMessageContent::from_tool_calls(calls);
        let text = msg.text_content();
        if !text.is_empty() {
            content.prepend(ContentPart::Text(text));
        }
        return content;
    }
    GenAiMessageContent::from_parts(
        msg.content
            .iter()
            .map(to_genai_content_part)
            .collect::<Vec<_>>(),
    )
}

fn to_genai_content_part(part: &MessageContent) -> ContentPart {
    match part {
        MessageContent::Text(text) => ContentPart::Text(text.clone()),
        MessageContent::Binary(binary) => {
            let encoded = base64::engine::general_purpose::STANDARD.encode(binary.bytes.as_ref());
            ContentPart::from_binary_base64(binary.mime_type.as_str(), encoded, binary.name.clone())
        }
    }
}

pub(crate) fn validate_binary_payloads(req: &CompletionRequest) -> Result<(), LlmError> {
    for message in &req.messages {
        for part in &message.content {
            if let MessageContent::Binary(binary) = part
                && binary.bytes.is_empty()
            {
                return Err(LlmError::IncompatibleRequest {
                    reason: format!("empty binary payload for MIME type {}", binary.mime_type),
                });
            }
        }
    }
    Ok(())
}

fn to_genai_tool_call(call: &ToolUseRequest) -> GenAiToolCall {
    GenAiToolCall {
        call_id: call.call_id.clone(),
        fn_name: call.name.clone(),
        fn_arguments: call.args.clone(),
        thought_signatures: None,
    }
}

fn chat_tool_call_to_request(call: &GenAiToolCall) -> ToolUseRequest {
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

pub(crate) fn chat_response_to_completion(resp: ChatResponse) -> CompletionResponse {
    let usage = usage_to_token_usage(&resp.usage);
    let tool_calls: Vec<ToolUseRequest> = resp
        .tool_calls()
        .into_iter()
        .map(chat_tool_call_to_request)
        .collect();
    let text = resp.into_first_text().unwrap_or_default();
    CompletionResponse {
        text,
        usage,
        tool_calls,
    }
}

fn usage_to_token_usage(usage: &GenAiUsage) -> TokenUsage {
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

    fn test_api_key() -> ApiKey {
        ApiKey::new("test-key".to_string()).expect("non-empty key")
    }

    /// Per-schema Clients are `Send + Sync + 'static` — required for the
    /// `LlmClient: Send + Sync` bound and for spawning across tokio
    /// tasks.
    #[test]
    fn per_schema_clients_are_send_sync_static() {
        fn assert_bounds<T: Send + Sync + 'static>() {}
        assert_bounds::<AnthropicClient>();
        assert_bounds::<OpenAiClient>();
        assert_bounds::<GeminiClient>();
    }

    /// Each per-schema Client impls `LlmClient` — the trait bound is
    /// satisfied at the type-check level. The dummy generic forces
    /// monomorphization against the concrete type.
    #[test]
    fn per_schema_clients_impl_llm_client() {
        fn assert_impl<T: LlmClient>() {}
        assert_impl::<AnthropicClient>();
        assert_impl::<OpenAiClient>();
        assert_impl::<GeminiClient>();
    }

    /// Each per-schema Client's `pub const SCHEMA` constant matches the
    /// runtime [`LlmClient::schema`] return. The const-vs-runtime
    /// agreement is what callers rely on for compile-time routing
    /// (`AnthropicClient::SCHEMA`) to stay in lock-step with dispatch
    /// (`client.schema()`).
    #[test]
    fn client_const_schema_matches_runtime_schema() {
        let anthropic = AnthropicClient::new(test_api_key());
        assert_eq!(AnthropicClient::SCHEMA, anthropic.schema());
        assert_eq!(AnthropicClient::SCHEMA, SchemaKind::Anthropic);

        let openai = OpenAiClient::new(test_api_key());
        assert_eq!(OpenAiClient::SCHEMA, openai.schema());
        assert_eq!(OpenAiClient::SCHEMA, SchemaKind::OpenAi);

        let gemini = GeminiClient::new(test_api_key());
        assert_eq!(GeminiClient::SCHEMA, gemini.schema());
        assert_eq!(GeminiClient::SCHEMA, SchemaKind::Gemini);
    }

    /// `LlmClient::supports(&model)` returns `model.schema() ==
    /// self.schema()` for every Client × ModelId pair. The default
    /// trait impl carries the equality; this exhaustive cross-product
    /// pins per-schema Clients refuse models from other schemas while
    /// accepting any model whose schema matches their own (including
    /// the `Other(String)` inner arms).
    #[test]
    fn supports_matches_schema_equality() {
        let anthropic = AnthropicClient::new(test_api_key());
        let openai = OpenAiClient::new(test_api_key());
        let gemini = GeminiClient::new(test_api_key());

        let models = [
            ModelId::Anthropic(AnthropicModel::ClaudeSonnet46),
            ModelId::Anthropic(AnthropicModel::Other("future".into())),
            ModelId::OpenAi(OpenAiModel::Gpt55),
            ModelId::OpenAi(OpenAiModel::Other("future".into())),
            ModelId::Gemini(GeminiModel::Gemini31Pro),
            ModelId::Gemini(GeminiModel::Other("future".into())),
        ];

        for model in &models {
            assert_eq!(
                anthropic.supports(model),
                model.schema() == SchemaKind::Anthropic,
                "AnthropicClient::supports({model:?})",
            );
            assert_eq!(
                openai.supports(model),
                model.schema() == SchemaKind::OpenAi,
                "OpenAiClient::supports({model:?})",
            );
            assert_eq!(
                gemini.supports(model),
                model.schema() == SchemaKind::Gemini,
                "GeminiClient::supports({model:?})",
            );
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

    /// Attaching two sinks via repeated `.with_event_sink(...)` builder
    /// calls accumulates them into a chain; a synthetic `emit_usage`
    /// drives the same private emission path `complete*` exercises, and
    /// every attached sink records exactly one `DriverKind::TokenUsage`
    /// event.
    #[test]
    fn client_with_event_sink_attaches_chain_and_receives_usage_events() {
        let sink_a = RecordingSink::default();
        let sink_b = RecordingSink::default();
        let recorded_a = sink_a.events.clone();
        let recorded_b = sink_b.events.clone();

        let client = AnthropicClient::new(test_api_key())
            .with_envelope_builder(test_envelope_builder())
            .with_event_sink(sink_a)
            .with_event_sink(sink_b);

        let usage = TokenUsage {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
        };
        client.emit_usage(&ModelId::Anthropic(AnthropicModel::ClaudeSonnet46), &usage);

        let events_a = recorded_a.lock().expect("sink_a mutex");
        let events_b = recorded_b.lock().expect("sink_b mutex");
        assert_eq!(events_a.len(), 1, "sink_a sees the event");
        assert_eq!(events_b.len(), 1, "sink_b sees the event");
        for events in [&events_a, &events_b] {
            match &events[0] {
                AgentEvent::DriverEvent { driver_kind, .. } => {
                    assert_eq!(*driver_kind, DriverKind::TokenUsage);
                }
                other => panic!("expected DriverEvent, got {other:?}"),
            }
        }
    }

    /// Calling `complete` with a `ModelId` whose `schema()` does not
    /// match the Client's `SCHEMA` returns `LlmError::IncompatibleModel`
    /// synchronously without issuing a network call. The Client wraps
    /// the shared `genai::Client` but never invokes `exec_chat`; the
    /// check sits at the top of the impl ahead of any wire setup, so
    /// the test reads the error without a real provider round-trip.
    #[test]
    fn incompatible_modelid_returns_typed_error_without_network() {
        let client = AnthropicClient::new(test_api_key());
        let req = CompletionRequest::new(ModelId::OpenAi(OpenAiModel::Gpt55));
        let future = client.complete(req);
        let outcome = tokio_test::block_on(future);
        match outcome {
            Err(LlmError::IncompatibleModel { model, expected }) => {
                assert_eq!(model, ModelId::OpenAi(OpenAiModel::Gpt55));
                assert_eq!(expected, SchemaKind::Anthropic);
            }
            other => panic!("expected IncompatibleModel, got {other:?}"),
        }

        let openai = OpenAiClient::new(test_api_key());
        let req = CompletionRequest::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46));
        match tokio_test::block_on(openai.complete(req)) {
            Err(LlmError::IncompatibleModel { expected, .. }) => {
                assert_eq!(expected, SchemaKind::OpenAi);
            }
            other => panic!("expected IncompatibleModel, got {other:?}"),
        }

        let gemini = GeminiClient::new(test_api_key());
        let req = CompletionRequest::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46));
        match tokio_test::block_on(gemini.complete(req)) {
            Err(LlmError::IncompatibleModel { expected, .. }) => {
                assert_eq!(expected, SchemaKind::Gemini);
            }
            other => panic!("expected IncompatibleModel, got {other:?}"),
        }
    }

    /// Known `ModelId` variants resolve to the documented provider
    /// model strings; the `Other(String)` arm passes through so
    /// consumers can name not-yet-listed models without a minor bump.
    #[test]
    fn model_id_to_provider_name_maps_known_and_other_variants() {
        assert_eq!(
            model_id_to_provider_name(&ModelId::Anthropic(AnthropicModel::ClaudeOpus48)),
            "claude-opus-4-8"
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

    #[test]
    fn empty_binary_payload_returns_incompatible_request() {
        let req = CompletionRequest::new(ModelId::Gemini(GeminiModel::Gemini31Pro))
            .user("see attached")
            .user_binary(crate::request::MimeType::APPLICATION_PDF, Vec::<u8>::new());

        match validate_binary_payloads(&req) {
            Err(LlmError::IncompatibleRequest { reason }) => {
                assert!(reason.contains("empty binary payload"));
                assert!(reason.contains("application/pdf"));
            }
            other => panic!("expected IncompatibleRequest, got {other:?}"),
        }
    }

    #[test]
    fn complete_structured_accepts_multimodal_messages() {
        #[derive(serde::Deserialize, schemars::JsonSchema)]
        #[expect(
            dead_code,
            reason = "fields are referenced through schemars-derived schema, not by Rust code"
        )]
        struct SummaryShape {
            summary: String,
        }

        let req = CompletionRequest::new(ModelId::Gemini(GeminiModel::Gemini31Pro))
            .user("summarize")
            .user_binary(crate::request::MimeType::APPLICATION_PDF, vec![1_u8, 2, 3]);
        let (chat_req, options) = to_genai_structured_chat_options::<SummaryShape>(req);

        assert_eq!(chat_req.messages.len(), 1);
        let parts: Vec<&ContentPart> = (&chat_req.messages[0].content).into_iter().collect();
        assert_eq!(parts.len(), 2);
        assert!(matches!(parts[0], ContentPart::Text(text) if text == "summarize"));
        assert!(matches!(parts[1], ContentPart::Binary(_)));
        assert!(matches!(
            options.response_format,
            Some(ChatResponseFormat::JsonSpec(_)),
        ));
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
            content: GenAiMessageContent::from_text("the answer"),
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

        let completion = chat_response_to_completion(resp);
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
            let completion = chat_response_to_completion(resp);
            let value: AnswerShape =
                parse_structured_text(&completion.text).expect("structured payload parses");
            assert_eq!(value, expected, "same T across providers: {adapter_kind:?}",);
        }
    }

    fn make_text_response(adapter: AdapterKind, model_name: &str, text: &str) -> ChatResponse {
        let model = ModelIden::new(adapter, model_name.to_string());
        ChatResponse {
            content: GenAiMessageContent::from_text(text),
            reasoning_content: None,
            model_iden: model.clone(),
            provider_model_iden: model,
            stop_reason: None,
            usage: GenAiUsage::default(),
            captured_raw_body: None,
            response_id: None,
        }
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
        let client = AnthropicClient::new(test_api_key())
            .with_envelope_builder(test_envelope_builder())
            .with_event_sink(sink);

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
        let client = AnthropicClient::new(test_api_key());
        let usage = TokenUsage {
            input: 100,
            output: 10,
            cache_read: 0,
            cache_write: 0,
        };
        client.emit_usage(&ModelId::Anthropic(AnthropicModel::ClaudeSonnet46), &usage);
    }

    /// The shared `genai::Client` is one process-wide instance: the
    /// `OnceLock` cell hands the same `Arc` to every per-schema Client
    /// construction. `Arc::ptr_eq` confirms they point at the same
    /// inner — the invariant the bead's connection-pool sharing relies
    /// on.
    #[test]
    fn shared_genai_client_is_process_wide_arc() {
        let a = shared_genai_client();
        let b = shared_genai_client();
        assert!(Arc::ptr_eq(&a, &b));
    }
}

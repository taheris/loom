//! Per-schema `LlmClient` impls on top of the multi-provider `genai`
//! crate.
//!
//! Each [`SchemaKind`] gets a dedicated Client type: [`AnthropicClient`],
//! [`OpenAiClient`], [`GeminiClient`]. Each Client owns a `genai::Client`
//! configured with the credential supplied at construction. No public
//! signature mentions `genai::Client` — the wrapper insulates consumers
//! from the underlying crate's API churn.

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use base64::Engine;
#[cfg(test)]
use genai::ServiceTarget;
use genai::adapter::AdapterKind;
use genai::chat::{
    CacheControl as GenAiCacheControl, ChatMessage, ChatOptions, ChatRequest, ChatResponse,
    ChatResponseFormat, ChatRole, ContentPart, JsonSpec, MessageContent as GenAiMessageContent,
    MessageOptions, Tool as GenAiTool, ToolCall as GenAiToolCall,
    ToolResponse as GenAiToolResponse, Usage as GenAiUsage,
};
use genai::resolver::AuthData;
#[cfg(test)]
use genai::resolver::Endpoint;
#[cfg(test)]
use loom_events::DriverKind;
use loom_events::event::Source;
use loom_events::identifier::SessionId;
use loom_events::{AgentEvent, DriverEventPayload, EnvelopeBuilder, EventSink, SessionScope};
#[cfg(test)]
use schemars::{JsonSchema, SchemaGenerator};

use crate::api_key::ApiKey;
use crate::cache::{CacheControl, CacheTtl};
use crate::client::{
    BoxFuture, CompletionResponse, DEFAULT_RETRY_AFTER, LlmCapability, LlmClient, LlmError,
    ToolCallId, ToolUseRequest, parse_retry_after,
};
use crate::model_id::{ModelId, SchemaKind};
use crate::request::{CompletionRequest, Message, MessageContent, Role};
use crate::tool::ToolDef;
use crate::usage::TokenUsage;

const ANTHROPIC_ADAPTER: AdapterKind = AdapterKind::Anthropic;
const OPENAI_ADAPTER: AdapterKind = AdapterKind::OpenAIResp;
const GEMINI_ADAPTER: AdapterKind = AdapterKind::Gemini;

fn genai_client_for_schema(adapter_kind: AdapterKind, api_key: &ApiKey) -> Arc<genai::Client> {
    let key = api_key.expose().to_owned();
    Arc::new(
        genai::Client::builder()
            .with_adapter_kind(adapter_kind)
            .with_auth_resolver_fn(move |_model| Ok(Some(AuthData::from_single(key.clone()))))
            .build(),
    )
}

#[cfg(test)]
fn genai_client_for_schema_endpoint(
    adapter_kind: AdapterKind,
    api_key: &ApiKey,
    base_url: String,
) -> Arc<genai::Client> {
    let key = api_key.expose().to_owned();
    Arc::new(
        genai::Client::builder()
            .with_adapter_kind(adapter_kind)
            .with_service_target_resolver_fn(move |mut target: ServiceTarget| {
                target.endpoint = Endpoint::from_owned(base_url.clone());
                target.auth = AuthData::from_single(key.clone());
                Ok(target)
            })
            .build(),
    )
}

/// Client targeting the [`SchemaKind::Anthropic`] schema.
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

    /// Construct a Client carrying `api_key` as its credential.
    pub fn new(api_key: ApiKey) -> Self {
        let inner = genai_client_for_schema(ANTHROPIC_ADAPTER, &api_key);
        Self {
            inner,
            api_key,
            sinks: Mutex::new(Vec::new()),
            envelope_builder: Mutex::new(Some(default_envelope_builder())),
        }
    }

    #[cfg(test)]
    fn with_mock_endpoint(mut self, base_url: String) -> Self {
        self.inner = genai_client_for_schema_endpoint(ANTHROPIC_ADAPTER, &self.api_key, base_url);
        self
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

    /// Replace the default [`EnvelopeBuilder`] with one that stamps the
    /// caller's event-session scope onto every emitted event.
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

    fn emit_driver_event(&self, event: DriverEventPayload) {
        emit_driver_event_to_chain(&self.envelope_builder, &self.sinks, event);
    }

    fn emit_event(&self, event: &AgentEvent) {
        emit_event_to_chain(&self.sinks, event);
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
        if let Err(err) = validate_binary_payloads_for_schema(Self::SCHEMA, &req) {
            return Box::pin(async move { Err(err) });
        }
        Box::pin(async move {
            let model_name = model_id_to_provider_name(&model);
            let (chat_req, options) = to_genai_chat_request(req);
            let resp = self
                .inner
                .exec_chat(&model_name, chat_req, Some(&options))
                .await
                .map_err(genai_error_to_llm)?;
            let response = chat_response_to_completion(resp)?;
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
        if let Err(err) = validate_binary_payloads_for_schema(Self::SCHEMA, &req) {
            return Box::pin(async move { Err(err) });
        }
        Box::pin(async move {
            let model_name = model_id_to_provider_name(&model);
            let (chat_req, options) = to_genai_structured_chat_options_raw(req, schema, type_name);
            let resp = self
                .inner
                .exec_chat(&model_name, chat_req, Some(&options))
                .await
                .map_err(genai_error_to_llm)?;
            let completion = chat_response_to_completion(resp)?;
            self.emit_usage(&model, &completion.usage);
            Ok(completion.text)
        })
    }
}

/// Client targeting the [`SchemaKind::OpenAi`] schema.
pub struct OpenAiClient {
    inner: Arc<genai::Client>,
    api_key: ApiKey,
    sinks: Mutex<Vec<Box<dyn EventSink>>>,
    envelope_builder: Mutex<Option<EnvelopeBuilder>>,
}

impl OpenAiClient {
    /// Wire-format discriminator this Client targets.
    pub const SCHEMA: SchemaKind = SchemaKind::OpenAi;

    /// Construct a Client carrying `api_key` as its credential.
    pub fn new(api_key: ApiKey) -> Self {
        let inner = genai_client_for_schema(OPENAI_ADAPTER, &api_key);
        Self {
            inner,
            api_key,
            sinks: Mutex::new(Vec::new()),
            envelope_builder: Mutex::new(Some(default_envelope_builder())),
        }
    }

    #[cfg(test)]
    fn with_mock_endpoint(mut self, base_url: String) -> Self {
        self.inner = genai_client_for_schema_endpoint(OPENAI_ADAPTER, &self.api_key, base_url);
        self
    }

    /// Attach an [`EventSink`] to this Client's chain.
    pub fn with_event_sink<S>(self, sink: S) -> Self
    where
        S: EventSink + 'static,
    {
        push_sink(&self.sinks, Box::new(sink));
        self
    }

    /// Replace the default [`EnvelopeBuilder`] with one that stamps the
    /// caller's event-session scope onto every emitted event.
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

    fn emit_driver_event(&self, event: DriverEventPayload) {
        emit_driver_event_to_chain(&self.envelope_builder, &self.sinks, event);
    }

    fn emit_event(&self, event: &AgentEvent) {
        emit_event_to_chain(&self.sinks, event);
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
        if let Err(err) = validate_binary_payloads_for_schema(Self::SCHEMA, &req) {
            return Box::pin(async move { Err(err) });
        }
        Box::pin(async move {
            let model_name = model_id_to_provider_name(&model);
            let (chat_req, options) = to_genai_chat_request(req);
            let resp = self
                .inner
                .exec_chat(&model_name, chat_req, Some(&options))
                .await
                .map_err(genai_error_to_llm)?;
            let response = chat_response_to_completion(resp)?;
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
        if let Err(err) = validate_binary_payloads_for_schema(Self::SCHEMA, &req) {
            return Box::pin(async move { Err(err) });
        }
        Box::pin(async move {
            let model_name = model_id_to_provider_name(&model);
            let (chat_req, options) = to_genai_structured_chat_options_raw(req, schema, type_name);
            let resp = self
                .inner
                .exec_chat(&model_name, chat_req, Some(&options))
                .await
                .map_err(genai_error_to_llm)?;
            let completion = chat_response_to_completion(resp)?;
            self.emit_usage(&model, &completion.usage);
            Ok(completion.text)
        })
    }
}

/// Client targeting the [`SchemaKind::Gemini`] schema.
pub struct GeminiClient {
    inner: Arc<genai::Client>,
    api_key: ApiKey,
    sinks: Mutex<Vec<Box<dyn EventSink>>>,
    envelope_builder: Mutex<Option<EnvelopeBuilder>>,
}

impl GeminiClient {
    /// Wire-format discriminator this Client targets.
    pub const SCHEMA: SchemaKind = SchemaKind::Gemini;

    /// Construct a Client carrying `api_key` as its credential.
    pub fn new(api_key: ApiKey) -> Self {
        let inner = genai_client_for_schema(GEMINI_ADAPTER, &api_key);
        Self {
            inner,
            api_key,
            sinks: Mutex::new(Vec::new()),
            envelope_builder: Mutex::new(Some(default_envelope_builder())),
        }
    }

    #[cfg(test)]
    fn with_mock_endpoint(mut self, base_url: String) -> Self {
        self.inner = genai_client_for_schema_endpoint(GEMINI_ADAPTER, &self.api_key, base_url);
        self
    }

    /// Attach an [`EventSink`] to this Client's chain.
    pub fn with_event_sink<S>(self, sink: S) -> Self
    where
        S: EventSink + 'static,
    {
        push_sink(&self.sinks, Box::new(sink));
        self
    }

    /// Replace the default [`EnvelopeBuilder`] with one that stamps the
    /// caller's event-session scope onto every emitted event.
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

    fn emit_driver_event(&self, event: DriverEventPayload) {
        emit_driver_event_to_chain(&self.envelope_builder, &self.sinks, event);
    }

    fn emit_event(&self, event: &AgentEvent) {
        emit_event_to_chain(&self.sinks, event);
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
        if let Err(err) = validate_binary_payloads_for_schema(Self::SCHEMA, &req) {
            return Box::pin(async move { Err(err) });
        }
        Box::pin(async move {
            let model_name = model_id_to_provider_name(&model);
            let (chat_req, options) = to_genai_chat_request(req);
            let resp = self
                .inner
                .exec_chat(&model_name, chat_req, Some(&options))
                .await
                .map_err(genai_error_to_llm)?;
            let response = chat_response_to_completion(resp)?;
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
        if let Err(err) = validate_binary_payloads_for_schema(Self::SCHEMA, &req) {
            return Box::pin(async move { Err(err) });
        }
        Box::pin(async move {
            let model_name = model_id_to_provider_name(&model);
            let (chat_req, options) = to_genai_structured_chat_options_raw(req, schema, type_name);
            let resp = self
                .inner
                .exec_chat(&model_name, chat_req, Some(&options))
                .await
                .map_err(genai_error_to_llm)?;
            let completion = chat_response_to_completion(resp)?;
            self.emit_usage(&model, &completion.usage);
            Ok(completion.text)
        })
    }
}

pub(super) fn default_envelope_builder() -> EnvelopeBuilder {
    EnvelopeBuilder::new(
        SessionScope::phase(SessionId::new("llm-client-default"), None),
        Source::Driver,
        || 0,
    )
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

pub(super) fn emit_event_to_chain(sinks: &Mutex<Vec<Box<dyn EventSink>>>, event: &AgentEvent) {
    let mut guard = sinks.lock().unwrap_or_else(|p| p.into_inner());
    for sink in guard.iter_mut() {
        sink.emit(event);
    }
}

pub(super) fn emit_driver_event_to_chain(
    envelope_builder: &Mutex<Option<EnvelopeBuilder>>,
    sinks: &Mutex<Vec<Box<dyn EventSink>>>,
    payload: DriverEventPayload,
) {
    let envelope = {
        let mut guard = envelope_builder.lock().unwrap_or_else(|p| p.into_inner());
        match guard.as_mut() {
            Some(builder) => builder.build_with_source(Source::Driver),
            None => return,
        }
    };
    let event = AgentEvent::from_driver_event(payload, envelope);
    emit_event_to_chain(sinks, &event);
}

pub(super) fn emit_usage_to_chain(
    envelope_builder: &Mutex<Option<EnvelopeBuilder>>,
    sinks: &Mutex<Vec<Box<dyn EventSink>>>,
    model: &ModelId,
    usage: &TokenUsage,
) {
    emit_driver_event_to_chain(
        envelope_builder,
        sinks,
        DriverEventPayload::token_usage(
            model_id_to_provider_name(model),
            usage.input,
            usage.output,
            usage.cache_read,
            usage.cache_write,
        ),
    );
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

fn genai_error_to_llm(err: genai::Error) -> LlmError {
    let diagnostic = err.to_string();
    match err {
        genai::Error::ChatReqHasNoMessages { .. }
        | genai::Error::LastChatMessageIsNotUser { .. }
        | genai::Error::MessageRoleNotSupported { .. }
        | genai::Error::MessageContentTypeNotSupported { .. }
        | genai::Error::JsonModeWithoutInstruction
        | genai::Error::VerbosityParsing { .. }
        | genai::Error::ReasoningParsingError { .. }
        | genai::Error::ServiceTierParsing { .. }
        | genai::Error::PromptCacheRetentionParsing { .. }
        | genai::Error::AdapterNotSupported { .. }
        | genai::Error::AdapterKindMismatch { .. } => {
            LlmError::IncompatibleRequest { reason: diagnostic }
        }
        genai::Error::NoChatResponse { .. }
        | genai::Error::InvalidJsonResponseElement { .. }
        | genai::Error::ChatResponseGeneration { .. }
        | genai::Error::StreamParse { .. }
        | genai::Error::JsonValueExt(_)
        | genai::Error::SerdeJson(_) => LlmError::MalformedJson(diagnostic),
        genai::Error::RequiresApiKey { .. }
        | genai::Error::NoAuthResolver { .. }
        | genai::Error::NoAuthData { .. } => LlmError::AuthFailed { reason: diagnostic },
        genai::Error::ModelMapperFailed { cause, .. } => resolver_error_to_llm(cause),
        genai::Error::Resolver { resolver_error, .. } => resolver_error_to_llm(resolver_error),
        genai::Error::WebAdapterCall { webc_error, .. }
        | genai::Error::WebModelCall { webc_error, .. } => webc_error_to_llm(webc_error),
        genai::Error::WebStream { cause, .. } => LlmError::Transport(cause),
        genai::Error::HttpError { status, body, .. } => {
            classify_status_code(status.as_u16(), None, body, current_system_time())
        }
        genai::Error::ChatResponse { body, .. } => LlmError::Provider {
            message: body.to_string(),
        },
        genai::Error::Internal(message) => LlmError::Provider { message },
    }
}

fn resolver_error_to_llm(err: genai::resolver::Error) -> LlmError {
    let diagnostic = err.to_string();
    match err {
        genai::resolver::Error::ApiKeyEnvNotFound { .. }
        | genai::resolver::Error::ResolverAuthDataNotSingleValue => {
            LlmError::AuthFailed { reason: diagnostic }
        }
        genai::resolver::Error::Custom(message) => LlmError::Provider { message },
    }
}

fn webc_error_to_llm(err: genai::webc::Error) -> LlmError {
    let diagnostic = err.to_string();
    match err {
        genai::webc::Error::ResponseFailedNotJson { .. }
        | genai::webc::Error::ResponseFailedInvalidJson { .. }
        | genai::webc::Error::JsonValueExt(_) => LlmError::MalformedJson(diagnostic),
        genai::webc::Error::ResponseFailedStatus {
            status,
            body,
            headers,
        } => {
            let retry_after = match headers.get("retry-after") {
                Some(value) => match value.to_str() {
                    Ok(raw) => parse_retry_after(raw, current_system_time()),
                    Err(_err) => DEFAULT_RETRY_AFTER,
                },
                None => DEFAULT_RETRY_AFTER,
            };
            classify_status_code_with_retry_after(status.as_u16(), retry_after, body)
        }
        genai::webc::Error::Reqwest(err) => {
            if err.is_timeout() {
                LlmError::Timeout
            } else if err.is_decode() {
                LlmError::MalformedJson(err.to_string())
            } else if let Some(status) = err.status() {
                classify_status_code(
                    status.as_u16(),
                    None,
                    err.to_string(),
                    current_system_time(),
                )
            } else {
                LlmError::Transport(err.to_string())
            }
        }
    }
}

fn classify_status_code(
    status: u16,
    retry_after_header: Option<&str>,
    body: String,
    now: SystemTime,
) -> LlmError {
    let retry_after =
        retry_after_header.map_or(DEFAULT_RETRY_AFTER, |raw| parse_retry_after(raw, now));
    classify_status_code_with_retry_after(status, retry_after, body)
}

fn classify_status_code_with_retry_after(
    status: u16,
    retry_after: std::time::Duration,
    body: String,
) -> LlmError {
    match status {
        401 | 403 => LlmError::AuthFailed { reason: body },
        429 => LlmError::RateLimited { retry_after },
        other => LlmError::ProviderHttp {
            status: other,
            body,
        },
    }
}

fn current_system_time() -> SystemTime {
    let now = SystemTime::now;
    now()
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

    let chat_messages: Vec<ChatMessage> = messages.into_iter().flat_map(to_chat_messages).collect();

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

fn to_chat_messages(msg: Message) -> Vec<ChatMessage> {
    let role = chat_role(msg.role);
    if msg.tool_call_id.is_some() || !msg.tool_calls.is_empty() {
        return vec![chat_message_with_cache(
            role,
            build_message_content(&msg),
            CacheControl::None,
        )];
    }

    let mut groups: Vec<(CacheControl, Vec<ContentPart>)> = Vec::new();
    for part in &msg.content {
        let cache = *part.cache();
        let content = to_genai_content_part(part);
        if let Some((last_cache, parts)) = groups.last_mut()
            && *last_cache == cache
        {
            parts.push(content);
            continue;
        }
        groups.push((cache, vec![content]));
    }

    groups
        .into_iter()
        .map(|(cache, parts)| {
            chat_message_with_cache(role.clone(), GenAiMessageContent::from_parts(parts), cache)
        })
        .collect()
}

fn chat_role(role: Role) -> ChatRole {
    match role {
        Role::User => ChatRole::User,
        Role::Assistant => ChatRole::Assistant,
        Role::Tool => ChatRole::Tool,
    }
}

fn chat_message_with_cache(
    role: ChatRole,
    content: GenAiMessageContent,
    cache: CacheControl,
) -> ChatMessage {
    let mut chat_msg = ChatMessage::new(role, content);
    if let Some(cache) = cache_control_to_genai(&cache) {
        let opts = MessageOptions::default().with_cache_control(cache);
        chat_msg = chat_msg.with_options(opts);
    }
    chat_msg
}

fn build_message_content(msg: &Message) -> GenAiMessageContent {
    if let Some(call_id) = msg.tool_call_id.as_ref() {
        let response = GenAiToolResponse::new(call_id.as_str().to_owned(), msg.text_content());
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
        MessageContent::Text { text, .. } => ContentPart::Text(text.clone()),
        MessageContent::Binary { binary, .. } => {
            let encoded = base64::engine::general_purpose::STANDARD.encode(binary.bytes.as_ref());
            ContentPart::from_binary_base64(
                binary.mime_type.as_str(),
                encoded,
                Some(
                    binary.name.clone().unwrap_or_else(|| {
                        synthesized_filename_for_mime(binary.mime_type.as_str())
                    }),
                ),
            )
        }
    }
}

fn synthesized_filename_for_mime(mime_type: &str) -> String {
    match mime_type {
        "application/pdf" => "document.pdf".to_string(),
        "image/png" => "image.png".to_string(),
        "image/jpeg" => "image.jpg".to_string(),
        "image/webp" => "image.webp".to_string(),
        other => {
            let sanitized = other
                .split_once('/')
                .map_or("bin", |(_, subtype)| subtype)
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect::<String>();
            let extension = sanitized.trim_matches('-');
            if extension.is_empty() {
                "attachment.bin".to_string()
            } else {
                format!("attachment.{extension}")
            }
        }
    }
}

fn validate_binary_payloads_for_schema(
    schema: SchemaKind,
    req: &CompletionRequest,
) -> Result<(), LlmError> {
    validate_binary_payloads(req)?;
    for message in &req.messages {
        for part in &message.content {
            if let MessageContent::Binary { binary, .. } = part
                && !schema_supports_binary_mime(schema, binary.mime_type.as_str())
            {
                return Err(LlmError::UnsupportedCapability {
                    provider: schema,
                    capability: LlmCapability::MultimodalBinary {
                        mime_type: binary.mime_type.clone(),
                    },
                });
            }
        }
    }
    Ok(())
}

fn schema_supports_binary_mime(schema: SchemaKind, mime_type: &str) -> bool {
    match schema {
        SchemaKind::Anthropic => matches!(
            mime_type,
            "application/pdf" | "image/png" | "image/jpeg" | "image/webp"
        ),
        SchemaKind::OpenAi | SchemaKind::Gemini => true,
        #[cfg(feature = "openai-compat")]
        SchemaKind::OpenAiCompat => false,
    }
}

pub(crate) fn validate_binary_payloads(req: &CompletionRequest) -> Result<(), LlmError> {
    for message in &req.messages {
        for part in &message.content {
            if let MessageContent::Binary { binary, .. } = part
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
        call_id: call.call_id.as_str().to_owned(),
        fn_name: call.name.clone(),
        fn_arguments: call.args.clone(),
        thought_signatures: None,
    }
}

fn chat_tool_call_to_request(call: &GenAiToolCall) -> Result<ToolUseRequest, LlmError> {
    Ok(ToolUseRequest {
        call_id: ToolCallId::parse(call.call_id.clone())
            .map_err(|err| LlmError::MalformedJson(err.to_string()))?,
        name: call.fn_name.clone(),
        args: call.fn_arguments.clone(),
    })
}

pub(crate) fn cache_control_to_genai(cache: &CacheControl) -> Option<GenAiCacheControl> {
    match cache {
        CacheControl::None => None,
        CacheControl::Ephemeral(CacheTtl::Minutes5) => Some(GenAiCacheControl::Ephemeral5m),
        CacheControl::Ephemeral(CacheTtl::Hours1) => Some(GenAiCacheControl::Ephemeral1h),
        CacheControl::Ephemeral(CacheTtl::Hours24) => Some(GenAiCacheControl::Ephemeral24h),
    }
}

pub(crate) fn chat_response_to_completion(
    resp: ChatResponse,
) -> Result<CompletionResponse, LlmError> {
    let usage = usage_to_token_usage(&resp.usage);
    let tool_calls: Vec<ToolUseRequest> = resp
        .tool_calls()
        .into_iter()
        .map(chat_tool_call_to_request)
        .collect::<Result<_, _>>()?;
    let text = resp.into_first_text().unwrap_or_default();
    Ok(CompletionResponse {
        text,
        usage,
        tool_calls,
    })
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
    use crate::client::LlmClientExt;
    use crate::model_id::{AnthropicModel, GeminiModel, OpenAiModel};
    use genai::ModelIden;
    use genai::chat::{PromptTokensDetails, Usage as GenAiUsage};
    use std::sync::{Arc, Mutex};
    use tokio::sync::Barrier;
    use wiremock::matchers::{body_partial_json, body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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

    fn openai_responses_payload(input: u32, output: u32, text: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "resp-test",
            "status": "completed",
            "model": "gpt-5.5",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": text,
                }]
            }],
            "usage": {
                "input_tokens": input,
                "output_tokens": output,
                "total_tokens": input + output
            }
        })
    }

    async fn single_received_json(server: &MockServer) -> serde_json::Value {
        let requests = server
            .received_requests()
            .await
            .expect("request recording enabled");
        assert_eq!(requests.len(), 1, "expected exactly one provider request");
        serde_json::from_slice(&requests[0].body).expect("request body is JSON")
    }

    /// Attaching two sinks via repeated `.with_event_sink(...)` builder
    /// calls accumulates them into a chain; a successful `complete`
    /// call emits token usage into both sinks without requiring callers
    /// to install an explicit envelope builder.
    #[tokio::test]
    async fn client_with_event_sink_attaches_chain_and_receives_usage_events() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(openai_responses_payload(10, 5, "ok")),
            )
            .expect(1)
            .mount(&server)
            .await;

        let sink_a = RecordingSink::default();
        let sink_b = RecordingSink::default();
        let recorded_a = sink_a.events.clone();
        let recorded_b = sink_b.events.clone();
        let client = OpenAiClient::new(test_api_key())
            .with_mock_endpoint(format!("{}/", server.uri()))
            .with_event_sink(sink_a)
            .with_event_sink(sink_b);

        let req = CompletionRequest::new(ModelId::OpenAi(OpenAiModel::Gpt55)).user("hi");
        let response = client
            .complete(req)
            .await
            .expect("mock completion succeeds");
        assert_eq!(response.usage.input, 10);
        assert_eq!(response.usage.output, 5);

        let events_a = recorded_a.lock().expect("sink_a mutex");
        let events_b = recorded_b.lock().expect("sink_b mutex");
        assert_eq!(events_a.len(), 1, "sink_a sees the event");
        assert_eq!(events_b.len(), 1, "sink_b sees the event");
        for events in [&events_a, &events_b] {
            match &events[0] {
                AgentEvent::DriverEvent {
                    driver_kind,
                    payload,
                    ..
                } => {
                    assert_eq!(*driver_kind, DriverKind::TokenUsage);
                    assert_eq!(payload["model"], "gpt-5.5");
                    assert_eq!(payload["input"], 10);
                    assert_eq!(payload["output"], 5);
                }
                other => panic!("expected DriverEvent, got {other:?}"),
            }
        }
    }

    const CONCURRENT_COMPLETIONS: usize = 8;

    async fn run_concurrent_completions<C, F>(client: Arc<C>, request: F)
    where
        C: LlmClient + 'static,
        F: Fn() -> CompletionRequest + Send + Sync + 'static,
    {
        let barrier = Arc::new(Barrier::new(CONCURRENT_COMPLETIONS));
        let request = Arc::new(request);
        let tasks = (0..CONCURRENT_COMPLETIONS)
            .map(|_| {
                let barrier = barrier.clone();
                let client = client.clone();
                let request = request.clone();
                tokio::spawn(async move {
                    barrier.wait().await;
                    client.complete(request()).await
                })
            })
            .collect::<Vec<_>>();

        for task in tasks {
            task.await
                .expect("completion task joins")
                .expect("mock completion succeeds");
        }
    }

    fn assert_concurrent_usage_events(events: &Arc<Mutex<Vec<AgentEvent>>>, model: &str) {
        let events = events.lock().expect("recorded events");
        assert_eq!(events.len(), CONCURRENT_COMPLETIONS);
        let mut sequences = Vec::with_capacity(events.len());
        for event in events.iter() {
            match event {
                AgentEvent::DriverEvent {
                    envelope,
                    driver_kind,
                    payload,
                    ..
                } => {
                    assert_eq!(*driver_kind, DriverKind::TokenUsage);
                    assert_eq!(payload["model"], model);
                    sequences.push(envelope.seq);
                }
                other => panic!("expected DriverEvent, got {other:?}"),
            }
        }
        sequences.sort_unstable();
        let event_count = u64::try_from(CONCURRENT_COMPLETIONS).expect("event count fits u64");
        assert_eq!(sequences, (0..event_count).collect::<Vec<_>>());
    }

    /// Simultaneous `complete` calls on each shared genai-backed Client
    /// preserve every usage event and allocate each event a distinct
    /// sequence number through the mutex-protected sink chain.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn genai_clients_emit_all_usage_events_under_concurrent_completions() {
        let server = MockServer::start().await;
        let delay = std::time::Duration::from_millis(20);
        let expected_calls =
            u64::try_from(CONCURRENT_COMPLETIONS).expect("completion count fits u64");
        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(ResponseTemplate::new(200).set_delay(delay).set_body_json(
                serde_json::json!({
                    "id": "msg_test",
                    "type": "message",
                    "role": "assistant",
                    "model": "claude-sonnet-4-6",
                    "content": [{"type": "text", "text": "ok"}],
                    "stop_reason": "end_turn",
                    "stop_sequence": null,
                    "usage": {"input_tokens": 2, "output_tokens": 1}
                }),
            ))
            .expect(expected_calls)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(delay)
                    .set_body_json(openai_responses_payload(2, 1, "ok")),
            )
            .expect(expected_calls)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.1-pro:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_delay(delay).set_body_json(
                serde_json::json!({
                    "candidates": [{
                        "content": {"parts": [{"text": "ok"}]},
                        "finishReason": "STOP"
                    }],
                    "usageMetadata": {
                        "promptTokenCount": 2,
                        "candidatesTokenCount": 1,
                        "totalTokenCount": 3
                    }
                }),
            ))
            .expect(expected_calls)
            .mount(&server)
            .await;

        let endpoint = format!("{}/", server.uri());

        let anthropic_sink = RecordingSink::default();
        let anthropic_events = anthropic_sink.events.clone();
        let anthropic = Arc::new(
            AnthropicClient::new(test_api_key())
                .with_mock_endpoint(endpoint.clone())
                .with_event_sink(anthropic_sink),
        );
        run_concurrent_completions(anthropic, || {
            CompletionRequest::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46)).user("hi")
        })
        .await;
        assert_concurrent_usage_events(&anthropic_events, "claude-sonnet-4-6");

        let openai_sink = RecordingSink::default();
        let openai_events = openai_sink.events.clone();
        let openai = Arc::new(
            OpenAiClient::new(test_api_key())
                .with_mock_endpoint(endpoint.clone())
                .with_event_sink(openai_sink),
        );
        run_concurrent_completions(openai, || {
            CompletionRequest::new(ModelId::OpenAi(OpenAiModel::Gpt55)).user("hi")
        })
        .await;
        assert_concurrent_usage_events(&openai_events, "gpt-5.5");

        let gemini_sink = RecordingSink::default();
        let gemini_events = gemini_sink.events.clone();
        let gemini = Arc::new(
            GeminiClient::new(test_api_key())
                .with_mock_endpoint(endpoint)
                .with_event_sink(gemini_sink),
        );
        run_concurrent_completions(gemini, || {
            CompletionRequest::new(ModelId::Gemini(GeminiModel::Gemini31Pro)).user("hi")
        })
        .await;
        assert_concurrent_usage_events(&gemini_events, "gemini-3.1-pro");
    }

    /// Calling `complete` with a `ModelId` whose `schema()` does not
    /// match the Client's `SCHEMA` returns `LlmError::IncompatibleModel`
    /// synchronously without issuing a network call. The Client checks
    /// schema compatibility before invoking `exec_chat`; the
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

    /// Cache markers land on the matching content part in the lowered
    /// `ChatRequest`, so the Anthropic adapter places the cache
    /// breakpoint at the per-content-block position the consumer
    /// chose. Adjacent uncached parts carry no `MessageOptions` so the
    /// wire payload reflects the per-block precision the typed surface
    /// promises.
    #[test]
    fn message_text_cached_marks_per_block_in_anthropic_request() {
        let req = CompletionRequest::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .system("be terse")
            .user("hi")
            .user_cached("doc", CacheControl::Ephemeral(CacheTtl::Hours1))
            .user_binary(crate::request::MimeType::APPLICATION_PDF, vec![1_u8, 2, 3])
            .max_tokens(512);

        let (chat_req, options) = to_genai_chat_request(req);

        assert_eq!(chat_req.system.as_deref(), Some("be terse"));
        assert_eq!(chat_req.messages.len(), 3);
        assert_eq!(chat_req.messages[0].role, ChatRole::User);
        assert!(chat_req.messages[0].options.is_none());
        assert_eq!(chat_req.messages[1].role, ChatRole::User);
        let cache = chat_req.messages[1]
            .options
            .as_ref()
            .and_then(|o| o.cache_control.as_ref())
            .expect("cached text part carries cache_control");
        assert_eq!(cache, &GenAiCacheControl::Ephemeral1h);
        assert_eq!(chat_req.messages[2].role, ChatRole::User);
        assert!(
            chat_req.messages[2].options.is_none(),
            "uncached binary part must not inherit text cache marker",
        );
        let parts: Vec<&ContentPart> = (&chat_req.messages[2].content).into_iter().collect();
        assert!(matches!(parts.as_slice(), [ContentPart::Binary(_)]));
        assert_eq!(options.max_tokens, Some(512));
    }

    /// A cache-marked OpenAI request exercises the live provider
    /// serialization path and succeeds while omitting the unsupported
    /// per-message cache marker from the Responses API payload.
    #[tokio::test]
    async fn cache_marker_no_ops_on_openai_provider() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(openai_responses_payload(2, 1, "ok")),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client =
            OpenAiClient::new(test_api_key()).with_mock_endpoint(format!("{}/", server.uri()));
        let req = CompletionRequest::new(ModelId::OpenAi(OpenAiModel::Gpt55))
            .user("hi")
            .user_cached("doc", CacheControl::Ephemeral(CacheTtl::Minutes5));

        let response = client.complete(req).await.expect("OpenAI request succeeds");
        assert_eq!(response.text, "ok");
        let body = single_received_json(&server).await;
        let body_text = serde_json::to_string(&body).expect("body serializes");
        assert!(
            !body_text.contains("cache"),
            "OpenAI payload must no-op per-message cache markers: {body_text}",
        );
    }

    #[tokio::test]
    async fn empty_binary_payload_returns_incompatible_request() {
        let anthropic_server = MockServer::start().await;
        let anthropic = AnthropicClient::new(test_api_key())
            .with_mock_endpoint(format!("{}/", anthropic_server.uri()));
        assert_empty_binary_rejected(
            &anthropic,
            CompletionRequest::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
                .user("see attached")
                .user_binary(crate::request::MimeType::APPLICATION_PDF, Vec::<u8>::new()),
        )
        .await;
        assert!(
            anthropic_server
                .received_requests()
                .await
                .expect("request recording enabled")
                .is_empty(),
            "Anthropic validation must happen before network I/O",
        );

        let openai_server = MockServer::start().await;
        let openai = OpenAiClient::new(test_api_key())
            .with_mock_endpoint(format!("{}/", openai_server.uri()));
        assert_empty_binary_rejected(
            &openai,
            CompletionRequest::new(ModelId::OpenAi(OpenAiModel::Gpt55))
                .user("see attached")
                .user_binary(crate::request::MimeType::APPLICATION_PDF, Vec::<u8>::new()),
        )
        .await;
        assert!(
            openai_server
                .received_requests()
                .await
                .expect("request recording enabled")
                .is_empty(),
            "OpenAI validation must happen before network I/O",
        );

        let gemini_server = MockServer::start().await;
        let gemini = GeminiClient::new(test_api_key())
            .with_mock_endpoint(format!("{}/", gemini_server.uri()));
        assert_empty_binary_rejected(
            &gemini,
            CompletionRequest::new(ModelId::Gemini(GeminiModel::Gemini31Pro))
                .user("see attached")
                .user_binary(crate::request::MimeType::APPLICATION_PDF, Vec::<u8>::new()),
        )
        .await;
        assert!(
            gemini_server
                .received_requests()
                .await
                .expect("request recording enabled")
                .is_empty(),
            "Gemini validation must happen before network I/O",
        );
    }

    async fn assert_empty_binary_rejected<C>(client: &C, req: CompletionRequest)
    where
        C: LlmClient,
    {
        match client.complete(req).await {
            Err(LlmError::IncompatibleRequest { reason }) => {
                assert!(reason.contains("empty binary payload"));
                assert!(reason.contains("application/pdf"));
            }
            other => panic!("expected IncompatibleRequest, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn openai_multimodal_serializes_pdf_input_file() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(openai_responses_payload(12, 4, "ok")),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client =
            OpenAiClient::new(test_api_key()).with_mock_endpoint(format!("{}/", server.uri()));
        let req = CompletionRequest::new(ModelId::OpenAi(OpenAiModel::Gpt55))
            .user("summarize")
            .user_binary_named(
                crate::request::MimeType::APPLICATION_PDF,
                vec![0x25_u8, 0x50, 0x44, 0x46],
                "report.pdf",
            );

        let response = client.complete(req).await.expect("OpenAI request succeeds");
        assert_eq!(response.text, "ok");
        let body = single_received_json(&server).await;
        assert_eq!(body["model"], "gpt-5.5");
        let content = body["input"][0]["content"]
            .as_array()
            .expect("multipart user content array");
        assert!(
            content
                .iter()
                .any(|part| { part["type"] == "input_text" && part["text"] == "summarize" })
        );
        let file = content
            .iter()
            .find(|part| part["type"] == "input_file")
            .expect("PDF is serialized as input_file");
        assert_eq!(file["filename"], "report.pdf");
        assert_eq!(file["file_data"], "data:application/pdf;base64,JVBERg==");
    }

    #[tokio::test]
    async fn provider_filename_synthesized_when_binary_name_absent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(openai_responses_payload(6, 2, "ok")),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client =
            OpenAiClient::new(test_api_key()).with_mock_endpoint(format!("{}/", server.uri()));
        let req = CompletionRequest::new(ModelId::OpenAi(OpenAiModel::Gpt55))
            .user("summarize")
            .user_binary(crate::request::MimeType::APPLICATION_PDF, vec![1_u8, 2, 3]);

        client.complete(req).await.expect("OpenAI request succeeds");
        let body = single_received_json(&server).await;
        let content = body["input"][0]["content"]
            .as_array()
            .expect("multipart user content array");
        let file = content
            .iter()
            .find(|part| part["type"] == "input_file")
            .expect("unnamed PDF is serialized as input_file");
        assert_eq!(file["filename"], "document.pdf");
    }

    #[tokio::test]
    async fn unsupported_multimodal_request_returns_typed_error_not_panic() {
        let server = MockServer::start().await;
        let client =
            AnthropicClient::new(test_api_key()).with_mock_endpoint(format!("{}/", server.uri()));
        let unsupported = crate::request::MimeType::parse("text/plain").expect("valid MIME type");
        let req = CompletionRequest::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .user("read this")
            .user_binary(unsupported.clone(), b"hello".to_vec());

        match client.complete(req).await {
            Err(LlmError::UnsupportedCapability {
                provider,
                capability: LlmCapability::MultimodalBinary { mime_type },
            }) => {
                assert_eq!(provider, SchemaKind::Anthropic);
                assert_eq!(mime_type, unsupported);
            }
            other => panic!("expected UnsupportedCapability, got {other:?}"),
        }
        assert!(
            server
                .received_requests()
                .await
                .expect("request recording enabled")
                .is_empty(),
            "unsupported MIME must fail before network I/O",
        );
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

        let completion = chat_response_to_completion(resp).expect("response converts");
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

    /// `complete_structured::<T>` drives the concrete Anthropic,
    /// OpenAI, and Gemini Clients through their generated provider
    /// requests. Each mock asserts the provider-specific structured
    /// output field is present, then returns that provider's native
    /// response shape carrying a JSON string that the public typed
    /// method deserializes into `T`.
    #[tokio::test]
    async fn complete_structured_returns_typed_t_across_providers() {
        #[derive(Debug, PartialEq, serde::Deserialize, schemars::JsonSchema)]
        struct AnswerShape {
            title: String,
            count: u32,
        }

        let anthropic_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/messages"))
            .and(header("x-api-key", "test-key"))
            .and(body_partial_json(serde_json::json!({
                "output_config": { "format": { "type": "json_schema" } }
            })))
            .and(body_string_contains("\"title\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "msg_test",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-6",
                "content": [{
                    "type": "text",
                    "text": "{\"title\":\"anthropic\",\"count\":11}"
                }],
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": { "input_tokens": 8, "output_tokens": 4 }
            })))
            .expect(1)
            .mount(&anthropic_server)
            .await;

        let anthropic = AnthropicClient::new(test_api_key())
            .with_mock_endpoint(format!("{}/", anthropic_server.uri()));
        let anthropic_value: AnswerShape = anthropic
            .complete_structured(
                CompletionRequest::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
                    .user("structured please"),
            )
            .await
            .expect("anthropic structured response parses");
        assert_eq!(
            anthropic_value,
            AnswerShape {
                title: "anthropic".into(),
                count: 11,
            }
        );

        let openai_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer test-key"))
            .and(body_partial_json(serde_json::json!({
                "text": { "format": { "type": "json_schema" } }
            })))
            .and(body_string_contains("\"title\""))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(openai_responses_payload(
                    9,
                    5,
                    "{\"title\":\"openai\",\"count\":22}",
                )),
            )
            .expect(1)
            .mount(&openai_server)
            .await;

        let openai = OpenAiClient::new(test_api_key())
            .with_mock_endpoint(format!("{}/", openai_server.uri()));
        let openai_value: AnswerShape = openai
            .complete_structured(
                CompletionRequest::new(ModelId::OpenAi(OpenAiModel::Gpt55))
                    .user("structured please"),
            )
            .await
            .expect("openai structured response parses");
        assert_eq!(
            openai_value,
            AnswerShape {
                title: "openai".into(),
                count: 22,
            }
        );

        let gemini_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.1-pro:generateContent"))
            .and(header("x-goog-api-key", "test-key"))
            .and(body_partial_json(serde_json::json!({
                "generationConfig": { "responseMimeType": "application/json" }
            })))
            .and(body_string_contains("responseJsonSchema"))
            .and(body_string_contains("\"title\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "candidates": [{
                    "content": {
                        "parts": [{
                            "text": "{\"title\":\"gemini\",\"count\":33}"
                        }]
                    },
                    "finishReason": "STOP"
                }],
                "usageMetadata": {
                    "promptTokenCount": 7,
                    "candidatesTokenCount": 3,
                    "totalTokenCount": 10
                }
            })))
            .expect(1)
            .mount(&gemini_server)
            .await;

        let gemini = GeminiClient::new(test_api_key())
            .with_mock_endpoint(format!("{}/", gemini_server.uri()));
        let gemini_value: AnswerShape = gemini
            .complete_structured(
                CompletionRequest::new(ModelId::Gemini(GeminiModel::Gemini31Pro))
                    .user("structured please"),
            )
            .await
            .expect("gemini structured response parses");
        assert_eq!(
            gemini_value,
            AnswerShape {
                title: "gemini".into(),
                count: 33,
            }
        );
    }

    /// Every successful `complete*` call emits a
    /// `DriverKind::TokenUsage` driver event into the configured sink
    /// chain. The event's payload carries the four-field `TokenUsage`
    /// plus the model identifier so SaaS billing pipelines see cache
    /// hits and compute their own per-tenant cost from the raw counts.
    #[tokio::test]
    async fn complete_emits_token_usage_driver_event() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(openai_responses_payload(
                    1_000,
                    250,
                    "the answer",
                )),
            )
            .expect(1)
            .mount(&server)
            .await;

        let sink = RecordingSink::default();
        let recorded = sink.events.clone();
        let client = OpenAiClient::new(test_api_key())
            .with_mock_endpoint(format!("{}/", server.uri()))
            .with_event_sink(sink);
        let req = CompletionRequest::new(ModelId::OpenAi(OpenAiModel::Gpt55)).user("hi");
        let response = client
            .complete(req)
            .await
            .expect("mock completion succeeds");
        assert_eq!(response.text, "the answer");

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
                assert_eq!(payload["model"], "gpt-5.5");
                assert_eq!(payload["input"], 1_000);
                assert_eq!(payload["output"], 250);
                assert_eq!(payload["cache_read"], 0);
                assert_eq!(payload["cache_write"], 0);
                let obj = payload.as_object().expect("payload is JSON object");
                let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
                keys.sort_unstable();
                assert_eq!(
                    keys,
                    vec!["cache_read", "cache_write", "input", "model", "output"],
                    "payload pins the spec'd field set with no extras",
                );
                assert!(
                    summary.contains("gpt-5.5"),
                    "summary names the model: {summary}"
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

    /// Each Client owns the credential supplied to its constructor.
    /// Two OpenAI Clients pointed at the same mock endpoint emit
    /// different Authorization headers, proving per-tenant credentials
    /// are Client-local rather than process-global.
    #[tokio::test]
    async fn per_client_api_key_reaches_provider_auth_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer tenant-a"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(openai_responses_payload(1, 1, "a")),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer tenant-b"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(openai_responses_payload(1, 1, "b")),
            )
            .expect(1)
            .mount(&server)
            .await;

        let endpoint = format!("{}/", server.uri());
        let client_a =
            OpenAiClient::new(ApiKey::new("tenant-a".to_string()).expect("non-empty tenant key"))
                .with_mock_endpoint(endpoint.clone());
        let client_b =
            OpenAiClient::new(ApiKey::new("tenant-b".to_string()).expect("non-empty tenant key"))
                .with_mock_endpoint(endpoint);

        let req_a = CompletionRequest::new(ModelId::OpenAi(OpenAiModel::Gpt55)).user("hi");
        let req_b = CompletionRequest::new(ModelId::OpenAi(OpenAiModel::Gpt55)).user("hi");
        assert_eq!(
            client_a
                .complete(req_a)
                .await
                .expect("tenant a succeeds")
                .text,
            "a"
        );
        assert_eq!(
            client_b
                .complete(req_b)
                .await
                .expect("tenant b succeeds")
                .text,
            "b"
        );
    }
}

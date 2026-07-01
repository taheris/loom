//! OpenAI Chat-Completions-shaped HTTP adapter.
//!
//! [`OpenAiCompatClient`] routes requests at a consumer-supplied
//! [`url::Url`] and serializes them in the OpenAI Chat-Completions wire
//! format. The schema family covers local runners (vLLM, llama.cpp, LM
//! Studio, Ollama's `/v1`), proxies (LiteLLM), and commercial
//! OpenAI-compatible providers. The adapter does not retry, throttle,
//! or fall back: it classifies HTTP / network outcomes into
//! [`crate::client::LlmError`] variants and hands retry policy to the
//! consumer via [`crate::client::LlmError::retry_advice`].
//!
//! Gated behind the `openai-compat` Cargo feature. The `--no-default-
//! features` build excludes this module entirely; the default build
//! ships only the three genai-backed Clients per the spec's
//! [Feature Flags](../../../../specs/llm.md#feature-flags) section.

use std::sync::Mutex;
use std::time::SystemTime;

use loom_events::{AgentEvent, EnvelopeBuilder, EventSink};
use serde::{Deserialize, Serialize};
use url::Url;

use super::multi_provider::{
    debug_per_schema_client, emit_event_to_chain, emit_usage_to_chain, push_sink,
    set_envelope_builder,
};
use crate::api_key::ApiKey;
use crate::client::{
    BoxFuture, CompletionResponse, DEFAULT_RETRY_AFTER, LlmCapability, LlmClient, LlmError,
    ToolUseRequest, parse_retry_after,
};
use crate::model_id::{ModelId, SchemaKind};
use crate::request::{CompletionRequest, Message, MessageContent, Role};
use crate::usage::TokenUsage;

/// Client targeting [`SchemaKind::OpenAiCompat`]. Carries a
/// [`reqwest::Client`] sized for repeated calls, the configured
/// `base_url`, and an optional bearer credential. Constructed via
/// [`OpenAiCompatClient::new`]; sinks attach through
/// [`OpenAiCompatClient::with_event_sink`].
pub struct OpenAiCompatClient {
    http: reqwest::Client,
    base_url: Url,
    api_key: Option<ApiKey>,
    sinks: Mutex<Vec<Box<dyn EventSink>>>,
    envelope_builder: Mutex<Option<EnvelopeBuilder>>,
}

impl OpenAiCompatClient {
    /// Wire-format discriminator this Client targets.
    pub const SCHEMA: SchemaKind = SchemaKind::OpenAiCompat;

    /// Construct a Client routed at `base_url`. The URL is the root the
    /// provider exposes the OpenAI Chat-Completions surface under (e.g.
    /// `http://localhost:11434/v1` for Ollama); the adapter appends
    /// `/chat/completions` per OpenAI's published path layout.
    pub fn new(base_url: Url, api_key: Option<ApiKey>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url,
            api_key,
            sinks: Mutex::new(Vec::new()),
            envelope_builder: Mutex::new(super::multi_provider::default_envelope_builder()),
        }
    }

    /// Attach an [`EventSink`] to this Client's chain. Each call
    /// appends; multiple calls compose. Every successful `complete*`
    /// call fans a `DriverKind::TokenUsage` event into every attached
    /// sink in registration order.
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

    /// Borrow the configured base URL.
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Borrow the optional credential the Client was constructed with.
    /// Callers SHOULD NOT log or emit this value; per RS-15 the wrapped
    /// string is meant for wire-level auth resolvers only.
    pub fn api_key(&self) -> Option<&ApiKey> {
        self.api_key.as_ref()
    }

    fn emit_usage(&self, model: &ModelId, usage: &TokenUsage) {
        emit_usage_to_chain(&self.envelope_builder, &self.sinks, model, usage);
    }

    fn chat_completions_url(&self) -> Result<Url, LlmError> {
        let base = self.base_url.as_str();
        let normalized = if base.ends_with('/') {
            format!("{base}chat/completions")
        } else {
            format!("{base}/chat/completions")
        };
        Url::parse(&normalized).map_err(|err| LlmError::Transport(err.to_string()))
    }
}

impl std::fmt::Debug for OpenAiCompatClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        debug_per_schema_client(f, "OpenAiCompatClient", Self::SCHEMA, &self.sinks)
    }
}

impl LlmClient for OpenAiCompatClient {
    fn schema(&self) -> SchemaKind {
        Self::SCHEMA
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
        if let Err(err) = super::multi_provider::validate_binary_payloads(&req) {
            return Box::pin(async move { Err(err) });
        }
        if let Some(capability) = first_binary_capability(&req) {
            return Box::pin(async move {
                Err(LlmError::UnsupportedCapability {
                    provider: Self::SCHEMA,
                    capability,
                })
            });
        }
        Box::pin(async move {
            let endpoint = self.chat_completions_url()?;
            let body = to_chat_completions_body(&req);
            let mut request = self.http.post(endpoint).json(&body);
            if let Some(key) = self.api_key.as_ref() {
                request = request.bearer_auth(key.expose());
            }
            let response = request.send().await.map_err(reqwest_error_to_llm)?;
            let status = response.status();
            let retry_after_header = match response.headers().get(reqwest::header::RETRY_AFTER) {
                Some(value) => match value.to_str() {
                    Ok(raw) => Some(raw.to_string()),
                    Err(_invalid_header) => None,
                },
                None => None,
            };
            let body_bytes = response.bytes().await.map_err(reqwest_error_to_llm)?;
            classify_status(
                status,
                retry_after_header.as_deref(),
                &body_bytes,
                current_system_time(),
            )?;
            let parsed: ChatCompletionResponse = serde_json::from_slice(&body_bytes)
                .map_err(|err| LlmError::MalformedJson(err.to_string()))?;
            let completion = chat_completion_to_response(parsed);
            self.emit_usage(&model, &completion.usage);
            Ok(completion)
        })
    }

    fn complete_structured_raw<'a>(
        &'a self,
        req: CompletionRequest,
        _schema: serde_json::Value,
        _type_name: String,
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
        Box::pin(async move {
            let response = self.complete(req).await?;
            Ok(response.text)
        })
    }
}

#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatCompletionMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

#[derive(Debug, Serialize)]
struct ChatCompletionMessage {
    role: &'static str,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    #[serde(default)]
    choices: Vec<ChatCompletionChoice>,
    #[serde(default)]
    usage: Option<ChatCompletionUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionChoice {
    #[serde(default)]
    message: Option<ChatCompletionResponseMessage>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponseMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionUsage {
    #[serde(default)]
    prompt_tokens: Option<u32>,
    #[serde(default)]
    completion_tokens: Option<u32>,
}

fn to_chat_completions_body(req: &CompletionRequest) -> ChatCompletionRequest {
    let mut messages: Vec<ChatCompletionMessage> = Vec::with_capacity(req.messages.len() + 1);
    if let Some(system) = req.system.as_ref() {
        messages.push(ChatCompletionMessage {
            role: "system",
            content: system.clone(),
            tool_call_id: None,
        });
    }
    for message in &req.messages {
        messages.push(message_to_wire(message));
    }
    ChatCompletionRequest {
        model: req.model.as_wire(),
        messages,
        max_tokens: req.max_tokens,
    }
}

fn message_to_wire(message: &Message) -> ChatCompletionMessage {
    let role = match message.role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    ChatCompletionMessage {
        role,
        content: message.text_content(),
        tool_call_id: message
            .tool_call_id
            .as_ref()
            .map(|call_id| call_id.as_str().to_owned()),
    }
}

fn first_binary_capability(req: &CompletionRequest) -> Option<LlmCapability> {
    req.messages
        .iter()
        .flat_map(|message| message.content.iter())
        .find_map(|part| match part {
            MessageContent::Binary { binary, .. } => Some(LlmCapability::MultimodalBinary {
                mime_type: binary.mime_type.clone(),
            }),
            MessageContent::Text { .. } => None,
        })
}

fn chat_completion_to_response(parsed: ChatCompletionResponse) -> CompletionResponse {
    let text = parsed
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message)
        .and_then(|m| m.content)
        .unwrap_or_default();
    let usage = parsed
        .usage
        .map_or_else(TokenUsage::default, |u| TokenUsage {
            input: u.prompt_tokens.unwrap_or(0),
            output: u.completion_tokens.unwrap_or(0),
            cache_read: 0,
            cache_write: 0,
        });
    CompletionResponse {
        text,
        usage,
        tool_calls: Vec::<ToolUseRequest>::new(),
    }
}

fn classify_status(
    status: reqwest::StatusCode,
    retry_after_header: Option<&str>,
    body: &[u8],
    now: SystemTime,
) -> Result<(), LlmError> {
    if status.is_success() {
        return Ok(());
    }
    let body_text = String::from_utf8_lossy(body).into_owned();
    match status.as_u16() {
        401 | 403 => Err(LlmError::AuthFailed { reason: body_text }),
        429 => {
            let retry_after =
                retry_after_header.map_or(DEFAULT_RETRY_AFTER, |raw| parse_retry_after(raw, now));
            Err(LlmError::RateLimited { retry_after })
        }
        other => Err(LlmError::ProviderHttp {
            status: other,
            body: body_text,
        }),
    }
}

fn current_system_time() -> SystemTime {
    let now = SystemTime::now;
    now()
}

fn reqwest_error_to_llm(err: reqwest::Error) -> LlmError {
    if err.is_timeout() {
        return LlmError::Timeout;
    }
    if err.is_decode() {
        return LlmError::MalformedJson(err.to_string());
    }
    if let Some(status) = err.status() {
        return classify_reqwest_status_error(status, err.to_string());
    }
    if err.is_connect()
        || err.is_request()
        || err.is_body()
        || err.is_redirect()
        || err.is_builder()
    {
        return LlmError::Transport(err.to_string());
    }
    LlmError::Transport(err.to_string())
}

fn classify_reqwest_status_error(status: reqwest::StatusCode, body: String) -> LlmError {
    match status.as_u16() {
        401 | 403 => LlmError::AuthFailed { reason: body },
        429 => LlmError::RateLimited {
            retry_after: DEFAULT_RETRY_AFTER,
        },
        other => LlmError::ProviderHttp {
            status: other,
            body,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::RetryAdvice;
    use crate::model_id::AnthropicModel;
    use loom_events::event::Source;
    use loom_events::identifier::BeadId;
    use std::sync::Arc;
    use std::time::Duration;
    use wiremock::matchers::{header, header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_api_key() -> ApiKey {
        ApiKey::new("sk-test".to_string()).expect("non-empty key")
    }

    fn mock_base_url(server: &MockServer) -> Url {
        let raw = format!("{}/v1", server.uri());
        Url::parse(&raw).expect("mock base URL parses")
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

    /// Issuing a `complete` call against an `OpenAiCompat` `ModelId`
    /// posts an OpenAI Chat-Completions-shaped JSON body to
    /// `<base_url>/chat/completions` with the model name, the messages
    /// array (including the system prefix when set), `max_tokens` when
    /// supplied, and the Bearer credential when configured. The
    /// wiremock matcher pins each of those wire-shape promises.
    #[tokio::test]
    async fn openai_compat_client_sends_chat_completions_shape_to_configured_url() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer sk-test"))
            .and(header_exists("content-type"))
            .and(wiremock::matchers::body_json(serde_json::json!({
                "model": "llama-3.1-70b",
                "messages": [
                    {"role": "system", "content": "be terse"},
                    {"role": "user", "content": "hi"},
                ],
                "max_tokens": 256,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "chatcmpl-1",
                "object": "chat.completion",
                "created": 0,
                "model": "llama-3.1-70b",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "hello"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = OpenAiCompatClient::new(mock_base_url(&server), Some(test_api_key()));
        let req = CompletionRequest::new(ModelId::OpenAiCompat("llama-3.1-70b".to_string()))
            .system("be terse")
            .user("hi")
            .max_tokens(256);
        let response = client.complete(req).await.expect("happy path succeeds");
        assert_eq!(response.text, "hello");
        assert_eq!(response.usage.input, 3);
        assert_eq!(response.usage.output, 2);
    }

    /// `OpenAiCompatClient::complete` returns
    /// [`LlmError::IncompatibleModel`] synchronously when handed a
    /// `ModelId` from a different schema. The check sits at the top of
    /// the impl ahead of any HTTP setup, so no wiremock mock is
    /// required — if a network call leaked through, the test would
    /// fail with a connection error against the bound-but-unmocked
    /// port.
    #[tokio::test]
    async fn openai_compat_client_rejects_non_compat_modelids() {
        let server = MockServer::start().await;
        let client = OpenAiCompatClient::new(mock_base_url(&server), Some(test_api_key()));
        let req = CompletionRequest::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46));
        match client.complete(req).await {
            Err(LlmError::IncompatibleModel { model, expected }) => {
                assert_eq!(model, ModelId::Anthropic(AnthropicModel::ClaudeSonnet46));
                assert_eq!(expected, SchemaKind::OpenAiCompat);
            }
            other => panic!("expected IncompatibleModel, got {other:?}"),
        }
        assert!(
            server
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty(),
            "synchronous fast-path must not issue a network call",
        );
    }

    #[tokio::test]
    async fn openai_compat_multimodal_returns_unsupported_without_network() {
        let server = MockServer::start().await;
        let client = OpenAiCompatClient::new(mock_base_url(&server), Some(test_api_key()));
        let req = CompletionRequest::new(ModelId::OpenAiCompat("llama-3.1-70b".to_string()))
            .user("see attached")
            .user_binary(crate::request::MimeType::IMAGE_PNG, vec![1_u8]);

        match client.complete(req).await {
            Err(LlmError::UnsupportedCapability {
                provider,
                capability: LlmCapability::MultimodalBinary { mime_type },
            }) => {
                assert_eq!(provider, SchemaKind::OpenAiCompat);
                assert_eq!(mime_type, crate::request::MimeType::IMAGE_PNG);
            }
            other => panic!("expected UnsupportedCapability, got {other:?}"),
        }
        assert!(
            server
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty(),
            "unsupported multimodal request must not issue a network call",
        );
    }

    async fn run_status_class_case(
        template: ResponseTemplate,
    ) -> Result<CompletionResponse, LlmError> {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(template)
            .mount(&server)
            .await;
        let client = OpenAiCompatClient::new(mock_base_url(&server), Some(test_api_key()));
        let req =
            CompletionRequest::new(ModelId::OpenAiCompat("llama-3.1-70b".to_string())).user("hi");
        client.complete(req).await
    }

    /// Exercises five status-class outcomes against a single
    /// `OpenAiCompatClient`, each behind its own scoped wiremock mock.
    /// The contract pins both the typed [`LlmError`] variant and the
    /// classification the [`RetryAdvice`] table promises consumers.
    #[tokio::test]
    async fn openai_compat_wiremock_contract_covers_status_classes() {
        let success_payload = serde_json::json!({
            "id": "chatcmpl-ok",
            "object": "chat.completion",
            "created": 0,
            "model": "llama-3.1-70b",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "ok"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        });

        let response =
            run_status_class_case(ResponseTemplate::new(200).set_body_json(success_payload))
                .await
                .expect("200 path returns Ok");
        assert_eq!(response.text, "ok");

        match run_status_class_case(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .await
        {
            Err(err @ LlmError::AuthFailed { .. }) => {
                assert_eq!(err.retry_advice(), RetryAdvice::NonRetryable);
            }
            other => panic!("expected AuthFailed, got {other:?}"),
        }

        match run_status_class_case(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "30")
                .set_body_string("slow down"),
        )
        .await
        {
            Err(err @ LlmError::RateLimited { .. }) => {
                assert_eq!(
                    err.retry_advice(),
                    RetryAdvice::RetryAfter(Duration::from_secs(30)),
                );
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }

        match run_status_class_case(ResponseTemplate::new(500).set_body_string("boom")).await {
            Err(LlmError::ProviderHttp { status, .. }) => {
                assert_eq!(status, 500);
                assert_eq!(
                    LlmError::ProviderHttp {
                        status,
                        body: String::new(),
                    }
                    .retry_advice(),
                    RetryAdvice::Retryable,
                );
            }
            other => panic!("expected ProviderHttp, got {other:?}"),
        }

        match run_status_class_case(ResponseTemplate::new(200).set_body_string("not json")).await {
            Err(err @ LlmError::MalformedJson(_)) => {
                assert_eq!(err.retry_advice(), RetryAdvice::Retryable);
            }
            other => panic!("expected MalformedJson, got {other:?}"),
        }
    }

    /// Recording sink used to confirm `complete*` calls emit a
    /// `DriverKind::TokenUsage` event into the attached chain.
    #[derive(Clone, Default)]
    struct RecordingSink {
        events: Arc<std::sync::Mutex<Vec<loom_events::AgentEvent>>>,
    }

    impl EventSink for RecordingSink {
        fn emit(&mut self, event: &loom_events::AgentEvent) {
            self.events
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(event.clone());
        }
    }

    /// A successful `complete` call against the openai-compat adapter
    /// fans a `DriverKind::TokenUsage` event into the attached sink
    /// chain with the prompt/completion token counts pulled from the
    /// provider's `usage` block.
    #[tokio::test]
    async fn openai_compat_complete_emits_token_usage_event() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "chatcmpl-1",
                "object": "chat.completion",
                "created": 0,
                "model": "llama-3.1-70b",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "ok"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 7, "completion_tokens": 11, "total_tokens": 18}
            })))
            .mount(&server)
            .await;

        let sink = RecordingSink::default();
        let recorded = sink.events.clone();
        let client = OpenAiCompatClient::new(mock_base_url(&server), Some(test_api_key()))
            .with_envelope_builder(test_envelope_builder())
            .with_event_sink(sink);
        let req =
            CompletionRequest::new(ModelId::OpenAiCompat("llama-3.1-70b".to_string())).user("hi");
        let response = client.complete(req).await.expect("call succeeds");
        assert_eq!(response.usage.input, 7);
        assert_eq!(response.usage.output, 11);

        let events = recorded.lock().expect("recorded events");
        assert_eq!(events.len(), 1, "exactly one driver event emitted");
        match &events[0] {
            loom_events::AgentEvent::DriverEvent {
                driver_kind,
                payload,
                ..
            } => {
                assert_eq!(*driver_kind, loom_events::DriverKind::TokenUsage);
                assert_eq!(payload["model"], "llama-3.1-70b");
                assert_eq!(payload["input"], 7);
                assert_eq!(payload["output"], 11);
            }
            other => panic!("expected DriverEvent, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use base64::Engine;
    use tracing::instrument::WithSubscriber;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::super::tests::openai_responses_payload;
    use super::super::{AnthropicClient, GeminiClient, OpenAiClient, to_genai_chat_request};
    use crate::api_key::ApiKey;
    use crate::client::{LlmCapability, LlmClient, LlmClientExt, LlmError};
    use crate::model_id::{AnthropicModel, GeminiModel, ModelId, OpenAiModel, SchemaKind};
    use crate::request::{CompletionRequest, MimeType};
    use loom_events::{AgentEvent, DriverKind, EventSink};

    const SYSTEM: &str = "fixture-sensitive-system";
    const PROMPT: &str = "fixture-sensitive-prompt";
    const RESPONSE: &str = "fixture-sensitive-response";
    const PAYLOAD: &[u8] = b"%PDF-fixture-sensitive-binary\0\xff";
    const CREDENTIAL: &str = "fixture-sensitive-credential";
    const NAME: &str = "safe-document.pdf";

    fn encoded_payload() -> String {
        base64::engine::general_purpose::STANDARD.encode(PAYLOAD)
    }

    fn contains_sensitive_data(log: &str) -> bool {
        [
            SYSTEM,
            PROMPT,
            RESPONSE,
            "fixture-sensitive-binary",
            CREDENTIAL,
        ]
        .iter()
        .any(|canary| log.contains(canary))
            || log.contains(&encoded_payload())
            || log.contains(&format!("{PAYLOAD:?}"))
    }

    struct Capture {
        file: tempfile::NamedTempFile,
        dispatch: tracing::Dispatch,
    }

    impl Capture {
        fn new() -> Self {
            let file = tempfile::NamedTempFile::new().expect("capture file");
            let writer = file.reopen().expect("capture writer");
            let subscriber = tracing_subscriber::fmt()
                .with_ansi(false)
                .without_time()
                .with_max_level(tracing::Level::DEBUG)
                .with_writer(move || writer.try_clone().expect("capture writer clone"))
                .finish();
            Self {
                file,
                dispatch: tracing::Dispatch::new(subscriber),
            }
        }

        fn read(&self) -> String {
            std::fs::read_to_string(self.file.path()).expect("captured UTF-8 logs")
        }
    }

    #[derive(Clone, Default)]
    struct RecordingSink(Arc<Mutex<Vec<AgentEvent>>>);

    impl EventSink for RecordingSink {
        fn emit(&mut self, event: &AgentEvent) {
            self.0.lock().expect("recorded events").push(event.clone());
        }
    }

    fn models() -> Vec<ModelId> {
        let models = vec![
            ModelId::Anthropic(AnthropicModel::ClaudeSonnet46),
            ModelId::OpenAi(OpenAiModel::Gpt55),
            ModelId::Gemini(GeminiModel::Gemini31Pro),
        ];
        #[cfg(feature = "openai-compat")]
        let models = {
            let mut models = models;
            models.push(ModelId::OpenAiCompat("fixture-model".parse().unwrap()));
            models
        };
        models
    }

    fn endpoint_path(schema: SchemaKind) -> &'static str {
        match schema {
            SchemaKind::Anthropic => "/messages",
            SchemaKind::OpenAi => "/responses",
            SchemaKind::Gemini => "/models/gemini-3.1-pro:generateContent",
            #[cfg(feature = "openai-compat")]
            SchemaKind::OpenAiCompat => "/chat/completions",
        }
    }

    fn client(model: &ModelId, server: &MockServer, sink: RecordingSink) -> Box<dyn LlmClient> {
        let key = ApiKey::new(CREDENTIAL.to_owned()).unwrap();
        tracing::debug!(?key, "credential diagnostics");
        let endpoint = format!("{}/", server.uri());
        match model.schema() {
            SchemaKind::Anthropic => {
                let client = AnthropicClient::new(key)
                    .with_mock_endpoint(endpoint)
                    .with_event_sink(sink);
                tracing::debug!(?client, "client diagnostics");
                Box::new(client)
            }
            SchemaKind::OpenAi => {
                let client = OpenAiClient::new(key)
                    .with_mock_endpoint(endpoint)
                    .with_event_sink(sink);
                tracing::debug!(?client, "client diagnostics");
                Box::new(client)
            }
            SchemaKind::Gemini => {
                let client = GeminiClient::new(key)
                    .with_mock_endpoint(endpoint)
                    .with_event_sink(sink);
                tracing::debug!(?client, "client diagnostics");
                Box::new(client)
            }
            #[cfg(feature = "openai-compat")]
            SchemaKind::OpenAiCompat => {
                let client =
                    crate::client::OpenAiCompatClient::new(endpoint.parse().unwrap(), Some(key))
                        .with_event_sink(sink);
                tracing::debug!(?client, "client diagnostics");
                Box::new(client)
            }
        }
    }

    fn carries_binary(schema: SchemaKind) -> bool {
        match schema {
            SchemaKind::Anthropic | SchemaKind::OpenAi | SchemaKind::Gemini => true,
            #[cfg(feature = "openai-compat")]
            SchemaKind::OpenAiCompat => false,
        }
    }

    fn request(model: ModelId) -> CompletionRequest {
        let req = CompletionRequest::new(model).system(SYSTEM).user(PROMPT);
        if carries_binary(req.model.schema()) {
            req.user_binary_named(MimeType::APPLICATION_PDF, PAYLOAD.to_vec(), NAME)
        } else {
            req
        }
    }

    fn response_body(schema: SchemaKind, text: &str) -> serde_json::Value {
        match schema {
            SchemaKind::Anthropic => serde_json::json!({
                "id": "msg_fixture", "type": "message", "role": "assistant",
                "model": "claude-sonnet-4-6",
                "content": [{"type": "text", "text": text}],
                "stop_reason": "end_turn", "usage": {"input_tokens": 4, "output_tokens": 2}
            }),
            SchemaKind::OpenAi => openai_responses_payload(4, 2, text),
            SchemaKind::Gemini => serde_json::json!({
                "candidates": [{"content": {"parts": [{"text": text}]}, "finishReason": "STOP"}],
                "usageMetadata": {"promptTokenCount": 4, "candidatesTokenCount": 2}
            }),
            #[cfg(feature = "openai-compat")]
            SchemaKind::OpenAiCompat => serde_json::json!({
                "choices": [{"message": {"role": "assistant", "content": text}}],
                "usage": {"prompt_tokens": 4, "completion_tokens": 2}
            }),
        }
    }

    #[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
    struct Answer {
        answer: String,
    }

    #[tokio::test]
    async fn multimodal_completion_logs_redact_payloads_and_preserve_safe_diagnostics() {
        for model in models() {
            let server = MockServer::start().await;
            let text = serde_json::json!({"answer": RESPONSE}).to_string();
            Mock::given(method("POST"))
                .and(path(endpoint_path(model.schema())))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(response_body(model.schema(), &text)),
                )
                .expect(2)
                .mount(&server)
                .await;
            let capture = Capture::new();
            let sink = RecordingSink::default();
            async {
                let client = client(&model, &server, sink.clone());
                let req = request(model.clone());
                tracing::info!(?req, "request diagnostics");
                let response = client.complete(req.clone()).await.unwrap();
                assert_eq!(response.text, text);
                tracing::debug!(?response, "response diagnostics");
                let answer = client.complete_structured::<Answer>(req).await.unwrap();
                assert_eq!(answer.answer, RESPONSE);
            }
            .with_subscriber(capture.dispatch.clone())
            .await;
            let requests = server.received_requests().await.unwrap();
            assert_eq!(requests.len(), 2);
            for req in requests {
                let body = String::from_utf8(req.body).unwrap();
                assert!(body.contains(PROMPT));
                if carries_binary(model.schema()) {
                    assert!(body.contains(&encoded_payload()));
                }
                assert!(
                    req.headers
                        .values()
                        .any(|value| value.to_str().unwrap().contains(CREDENTIAL))
                );
            }
            let events = sink.0.lock().unwrap();
            assert_eq!(events.len(), 2);
            for event in events.iter() {
                assert!(
                    matches!(event, AgentEvent::DriverEvent { driver_kind: DriverKind::TokenUsage, payload, .. } if payload["input"] == 4 && payload["output"] == 2)
                );
                assert!(!contains_sensitive_data(
                    &serde_json::to_string(event).unwrap()
                ));
            }
            drop(events);
            let log = capture.read();
            assert!(!contains_sensitive_data(&log), "{log}");
            assert!(log.contains("request diagnostics") && log.contains("response diagnostics"));
            assert!(log.contains("[REDACTED]"));
            if carries_binary(model.schema()) {
                assert!(log.contains("application/pdf") && log.contains(NAME));
                assert!(log.contains(&format!("byte_len: {}", PAYLOAD.len())));
            }
            assert!(log.contains("text_char_len") && log.contains("TokenUsage"));
        }
    }

    #[tokio::test]
    async fn provider_failure_logs_redact_echoed_content_in_typed_errors() {
        for model in models() {
            for status in [401, 503] {
                let server = MockServer::start().await;
                let echo = format!(
                    "{SYSTEM} {PROMPT} {RESPONSE} {CREDENTIAL} {}",
                    encoded_payload()
                );
                Mock::given(method("POST"))
                    .and(path(endpoint_path(model.schema())))
                    .respond_with(
                        ResponseTemplate::new(status)
                            .set_body_json(serde_json::json!({"error": echo})),
                    )
                    .expect(1)
                    .mount(&server)
                    .await;
                let capture = Capture::new();
                async {
                    let client = client(&model, &server, RecordingSink::default());
                    let error = client.complete(request(model.clone())).await.unwrap_err();
                    match &error {
                        LlmError::AuthFailed { reason } if status == 401 => {
                            assert!(reason.contains(CREDENTIAL));
                        }
                        LlmError::ProviderHttp {
                            status: actual,
                            body,
                        } if status == 503 => {
                            assert_eq!(*actual, status);
                            assert!(body.contains(RESPONSE));
                        }
                        other => panic!("unexpected typed error: {other:?}"),
                    }
                    tracing::error!(?error, "completion failed");
                }
                .with_subscriber(capture.dispatch.clone())
                .await;
                let log = capture.read();
                assert!(log.contains("completion failed"));
                assert!(log.contains(if status == 401 {
                    "AuthFailed"
                } else {
                    "ProviderHttp"
                }));
                assert!(!contains_sensitive_data(&log), "{log}");
            }
        }
    }

    #[tokio::test]
    async fn structured_response_error_logs_redact_sensitive_invalid_values() {
        for model in models() {
            for malformed in [true, false] {
                let server = MockServer::start().await;
                let text = if malformed {
                    RESPONSE.to_owned()
                } else {
                    serde_json::json!({"answer": {"sensitive": RESPONSE}}).to_string()
                };
                Mock::given(method("POST"))
                    .and(path(endpoint_path(model.schema())))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_json(response_body(model.schema(), &text)),
                    )
                    .expect(1)
                    .mount(&server)
                    .await;
                let capture = Capture::new();
                async {
                    let client = client(&model, &server, RecordingSink::default());
                    let error = client
                        .complete_structured::<Answer>(request(model.clone()))
                        .await
                        .unwrap_err();
                    assert!(matches!(
                        (&error, malformed),
                        (LlmError::MalformedJson(_), true) | (LlmError::SchemaViolation(_), false)
                    ));
                    tracing::error!(?error, "structured completion failed");
                }
                .with_subscriber(capture.dispatch.clone())
                .await;
                let log = capture.read();
                assert!(log.contains("structured completion failed"));
                assert!(!contains_sensitive_data(&log), "{log}");
            }
        }
    }

    #[tokio::test]
    async fn multimodal_validation_error_logs_preserve_safe_mime_without_network() {
        for model in models() {
            let server = MockServer::start().await;
            let capture = Capture::new();
            async {
                let client = client(&model, &server, RecordingSink::default());
                let empty = CompletionRequest::new(model.clone())
                    .user(PROMPT)
                    .user_binary_named(MimeType::APPLICATION_PDF, Vec::<u8>::new(), NAME);
                let error = client.complete(empty).await.unwrap_err();
                assert!(matches!(error, LlmError::IncompatibleRequest { .. }));
                tracing::error!(?error, "invalid binary request");
                if model.schema() == SchemaKind::Anthropic || !carries_binary(model.schema()) {
                    let req = CompletionRequest::new(model.clone())
                        .user(PROMPT)
                        .user_binary_named("text/plain".parse().unwrap(), PAYLOAD.to_vec(), NAME);
                    let error = client.complete(req).await.unwrap_err();
                    assert!(matches!(error, LlmError::UnsupportedCapability { .. }));
                    tracing::error!(?error, "unsupported binary request");
                }
            }
            .with_subscriber(capture.dispatch.clone())
            .await;
            assert!(server.received_requests().await.unwrap().is_empty());
            let log = capture.read();
            assert!(log.contains("IncompatibleRequest"));
            if model.schema() == SchemaKind::Anthropic || !carries_binary(model.schema()) {
                assert!(log.contains("UnsupportedCapability") && log.contains("text/plain"));
            }
            assert!(!contains_sensitive_data(&log), "{log}");
        }
    }

    #[test]
    fn typed_error_debug_logs_redact_all_opaque_diagnostics() {
        let echo = format!(
            "{SYSTEM} {PROMPT} {RESPONSE} {CREDENTIAL} {}",
            encoded_payload()
        );
        let errors = [
            LlmError::Timeout,
            LlmError::RateLimited {
                retry_after: std::time::Duration::from_secs(7),
            },
            LlmError::IncompatibleModel {
                model: ModelId::OpenAi(OpenAiModel::Gpt55),
                expected: SchemaKind::Anthropic,
            },
            LlmError::Transport(echo.clone()),
            LlmError::AuthFailed {
                reason: echo.clone(),
            },
            LlmError::ProviderHttp {
                status: 503,
                body: echo.clone(),
            },
            LlmError::MalformedJson(echo.clone()),
            LlmError::SchemaViolation(echo.clone()),
            LlmError::IncompatibleRequest {
                reason: echo.clone(),
            },
            LlmError::Provider { message: echo },
            LlmError::UnsupportedCapability {
                provider: SchemaKind::Anthropic,
                capability: LlmCapability::MultimodalBinary {
                    mime_type: MimeType::APPLICATION_PDF,
                },
            },
        ];
        let capture = Capture::new();
        tracing::dispatcher::with_default(&capture.dispatch, || {
            for error in errors {
                tracing::error!(?error, "typed failure diagnostics");
            }
        });
        let log = capture.read();
        assert_eq!(log.matches("typed failure diagnostics").count(), 11);
        assert!(log.contains("status: 503") && log.contains("application/pdf"));
        assert!(!contains_sensitive_data(&log), "{log}");
    }

    #[test]
    fn sensitive_log_checker_rejects_intentionally_leaking_fixtures() {
        let req = request(ModelId::OpenAi(OpenAiModel::Gpt55));
        let (wire, _) = to_genai_chat_request(req.clone());
        let error = LlmError::ProviderHttp {
            status: 503,
            body: RESPONSE.to_owned(),
        };
        let leaks = [
            SYSTEM.to_owned(),
            PROMPT.to_owned(),
            RESPONSE.to_owned(),
            CREDENTIAL.to_owned(),
            format!("{:?}", bytes::Bytes::copy_from_slice(PAYLOAD)),
            format!("{PAYLOAD:?}"),
            encoded_payload(),
            format!("{wire:?}"),
            error.to_string(),
        ];
        for leak in leaks {
            let capture = Capture::new();
            tracing::dispatcher::with_default(&capture.dispatch, || {
                tracing::info!(payload = leak, "intentionally unsafe fixture");
            });
            let log = capture.read();
            assert!(log.contains("intentionally unsafe fixture"));
            assert!(contains_sensitive_data(&log), "checker missed leak: {log}");
        }
        assert!(!contains_sensitive_data(&format!("{req:?} {error:?}")));
    }
}

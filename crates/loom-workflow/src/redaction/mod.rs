use loom_driver::agent::SpawnConfig;
use loom_events::{AgentEvent, DriverKind, InputRedaction, RedactionClass};

mod sink;
pub use sink::Sink;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedAgentInput {
    pub text: String,
    pub redactions: Option<Vec<InputRedaction>>,
}

struct Secret {
    value: String,
    redaction: InputRedaction,
}

struct Policy {
    secrets: Vec<Secret>,
}

impl Policy {
    fn new(config: &SpawnConfig) -> Self {
        let mut secrets = Vec::new();
        for (name, value) in config.env.iter().chain(&config.launcher_env) {
            if value.is_empty() {
                continue;
            }
            let Some(class) = secret_class(name) else {
                continue;
            };
            let redaction = InputRedaction {
                marker: redaction_marker(name, &class),
                class,
            };
            secrets.push(Secret {
                value: value.clone(),
                redaction: redaction.clone(),
            });
            let encoded = serde_json::Value::String(value.clone()).to_string();
            let escaped = &encoded[1..encoded.len() - 1];
            if escaped != value {
                secrets.push(Secret {
                    value: escaped.to_string(),
                    redaction,
                });
            }
        }
        secrets.sort_by(|a, b| {
            b.value
                .len()
                .cmp(&a.value.len())
                .then(a.redaction.marker.cmp(&b.redaction.marker))
        });
        secrets.dedup_by(|a, b| a.value == b.value);
        Self { secrets }
    }

    fn text(&self, text: &str) -> RedactedAgentInput {
        self.prefix(text, true).0
    }

    fn prefix(&self, text: &str, complete: bool) -> (RedactedAgentInput, usize) {
        let mut output = String::with_capacity(text.len());
        let mut redactions = Vec::new();
        let mut remaining = text;
        while !remaining.is_empty() {
            if let Some(secret) = self.secrets.iter().find(|secret| {
                remaining.starts_with(&secret.value)
                    || (!complete && secret.value.starts_with(remaining))
            }) {
                if remaining.len() < secret.value.len() {
                    break;
                }
                output.push_str(&secret.redaction.marker);
                if !redactions.contains(&secret.redaction) {
                    redactions.push(secret.redaction.clone());
                }
                remaining = &remaining[secret.value.len()..];
            } else if let Some(ch) = remaining.chars().next() {
                output.push(ch);
                remaining = &remaining[ch.len_utf8()..];
            }
        }
        (
            RedactedAgentInput {
                text: output,
                redactions: (!redactions.is_empty()).then_some(redactions),
            },
            text.len() - remaining.len(),
        )
    }

    fn value(&self, value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(text) => *text = self.text(text).text,
            serde_json::Value::Array(values) => {
                for value in values {
                    self.value(value);
                }
            }
            serde_json::Value::Object(values) => {
                *values = std::mem::take(values)
                    .into_iter()
                    .map(|(key, mut value)| {
                        self.value(&mut value);
                        (self.text(&key).text, value)
                    })
                    .collect();
            }
            serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            }
        }
    }

    fn event(&self, event: &mut AgentEvent) {
        match event {
            AgentEvent::AgentInput {
                text, redactions, ..
            } => {
                let input = self.text(text);
                *text = input.text;
                if let Some(markers) = input.redactions {
                    redactions.get_or_insert_with(Vec::new).extend(markers);
                }
            }
            AgentEvent::AgentStart { title: text, .. }
            | AgentEvent::TextDelta { text, .. }
            | AgentEvent::ThinkingDelta { text, .. }
            | AgentEvent::ToolcallDelta { delta: text, .. }
            | AgentEvent::ToolResult { output: text, .. }
            | AgentEvent::ToolProgress { text, .. }
            | AgentEvent::AutoRetry {
                error_message: text,
                ..
            }
            | AgentEvent::Error { message: text, .. } => *text = self.text(text).text,
            AgentEvent::ToolCall { tool, params, .. } => {
                *tool = self.text(tool).text;
                self.value(params);
            }
            AgentEvent::DriverEvent {
                driver_kind,
                summary,
                payload,
                ..
            } => {
                if let DriverKind::Other(kind) = driver_kind {
                    *kind = self.text(kind).text;
                }
                *summary = self.text(summary).text;
                self.value(payload);
            }
            AgentEvent::AgentEnd { .. }
            | AgentEvent::TurnStart { .. }
            | AgentEvent::TurnEnd { .. }
            | AgentEvent::TextEnd { .. }
            | AgentEvent::ThinkingEnd { .. }
            | AgentEvent::SessionComplete { .. }
            | AgentEvent::CompactionStart { .. }
            | AgentEvent::CompactionEnd { .. } => {}
        }
    }
}

pub fn redact_agent_input(text: &str, config: &SpawnConfig) -> RedactedAgentInput {
    Policy::new(config).text(text)
}

fn secret_class(name: &str) -> Option<RedactionClass> {
    let upper = name.to_ascii_uppercase();
    if upper.contains("API_KEY") {
        Some(RedactionClass::ApiKey)
    } else if upper.contains("TOKEN") {
        Some(RedactionClass::Token)
    } else if upper.contains("SECRET")
        || upper.contains("PASSWORD")
        || upper.contains("PRIVATE_KEY")
        || upper.contains("DEPLOY_KEY")
        || upper.contains("SIGNING_KEY")
    {
        Some(RedactionClass::Secret)
    } else {
        None
    }
}

fn redaction_marker(name: &str, class: &RedactionClass) -> String {
    format!("[REDACTED:{}:{name}]", class.as_wire())
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::agent::{ImageSourceKind, RePinContent};
    use std::path::PathBuf;

    fn config_with_env(env: Vec<(String, String)>) -> SpawnConfig {
        SpawnConfig {
            image_ref: "localhost/test".into(),
            image_source: PathBuf::from("/nix/store/test"),
            image_source_kind: Some(ImageSourceKind::NixDescriptor),
            wrix_launcher: None,
            profile_config: None,
            workspace: PathBuf::from("/workspace"),
            env,
            mounts: Vec::new(),
            initial_prompt: String::new(),
            agent_args: Vec::new(),
            repin: RePinContent {
                orientation: String::new(),
                pinned_context: String::new(),
                partial_bodies: Vec::new(),
            },
            skills: None,
            event_metadata: None,
            scratch_dir: PathBuf::new(),
            model_id: None,
            model: None,
            thinking_level: None,
            observers: loom_driver::config::AgentObserversConfig::default(),
            output_limits: None,
            shutdown_grace: None,
            denied_tools: Vec::new(),
            handshake_timeout: None,
            stall_warn_interval: None,
            launcher_env: Vec::new(),
        }
    }

    fn envelope_builder() -> loom_events::EnvelopeBuilder {
        loom_events::EnvelopeBuilder::new(
            loom_events::SessionScope::phase(
                loom_events::identifier::SessionId::new("redaction-test").unwrap(),
                None,
            ),
            loom_events::Source::Agent,
            || 123,
        )
    }

    fn persisted(
        events: Vec<loom_events::ParsedAgentEvent>,
        config: &SpawnConfig,
    ) -> Vec<AgentEvent> {
        let dir = tempfile::tempdir().unwrap();
        let inner = loom_render::LogSink::open_phase_at(
            dir.path(),
            "test",
            None,
            std::time::SystemTime::UNIX_EPOCH,
        )
        .unwrap();
        let path = inner.log_path().to_path_buf();
        let mut sink = Sink::new(inner, config);
        let mut builder = envelope_builder();
        for event in events {
            sink.emit(&AgentEvent::from_parsed(event, builder.build()))
                .unwrap();
        }
        sink.finish(loom_render::BeadOutcome::Done).unwrap();
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn backend_payload_redaction_covers_text_tools_errors_and_driver_data() {
        use loom_events::{DriverEventPayload, ParsedAgentEvent as Parsed};
        let config = config_with_env(vec![("API_KEY".into(), "fixture-secret".into())]);
        let id = loom_events::identifier::ToolCallId::new("call-1").unwrap();
        let secret = "fixture-secret".to_string();
        let events = vec![
            Parsed::TextDelta {
                text: secret.clone(),
            },
            Parsed::ThinkingDelta {
                text: secret.clone(),
            },
            Parsed::ToolcallDelta {
                id: id.clone(),
                delta: secret.clone(),
            },
            Parsed::ToolProgress {
                id: id.clone(),
                text: secret.clone(),
            },
            Parsed::ToolResult {
                id: id.clone(),
                output: secret.clone(),
                is_error: true,
            },
            Parsed::ToolCall {
                id: id.clone(),
                tool: secret.clone(),
                parent_tool_call_id: Some(id),
                params: serde_json::json!({"nested": [{"fixture-secret": "fixture-secret"}], "ok": false}),
            },
            Parsed::AutoRetry {
                attempt: 1,
                max_attempts: 3,
                delay_ms: 100,
                error_message: secret.clone(),
            },
            Parsed::Error {
                message: secret.clone(),
            },
            Parsed::DriverEvent(DriverEventPayload::new(
                DriverKind::Other(secret.clone()),
                secret,
                serde_json::json!({"nested": ["fixture-secret", null, 4]}),
            )),
        ];
        let mut builder = envelope_builder();
        let originals: Vec<_> = events
            .iter()
            .cloned()
            .map(|event| AgentEvent::from_parsed(event, builder.build()))
            .collect();
        let sanitized = persisted(events, &config);
        assert_eq!(sanitized.len(), originals.len());
        for (event, original) in sanitized.iter().zip(&originals) {
            let json = serde_json::to_string(event).unwrap();
            assert!(!json.contains("fixture-secret"), "{json}");
            assert!(json.contains("[REDACTED:api_key:API_KEY]"), "{json}");
            assert_eq!(event.envelope(), original.envelope());
        }
        let call = serde_json::to_value(&sanitized[5]).unwrap();
        assert_eq!(call["params"]["ok"], false);
        assert_eq!(call["parent_tool_call_id"], "call-1");
        let retry = serde_json::to_value(&sanitized[6]).unwrap();
        assert_eq!(retry["attempt"], 1);
        assert_eq!(retry["delay_ms"], 100);
    }

    #[test]
    fn every_utf8_split_of_a_streamed_secret_is_redacted() {
        use loom_events::ParsedAgentEvent as Parsed;
        let secret = "鍵-abc";
        let config = config_with_env(vec![("API_KEY".into(), secret.into())]);
        let id = loom_events::identifier::ToolCallId::new("call-1").unwrap();
        for split in secret.char_indices().map(|(offset, _)| offset).skip(1) {
            for events in [
                vec![
                    Parsed::TextDelta {
                        text: secret[..split].into(),
                    },
                    Parsed::TextDelta {
                        text: secret[split..].into(),
                    },
                ],
                vec![
                    Parsed::ThinkingDelta {
                        text: secret[..split].into(),
                    },
                    Parsed::ThinkingDelta {
                        text: secret[split..].into(),
                    },
                ],
                vec![
                    Parsed::ToolcallDelta {
                        id: id.clone(),
                        delta: secret[..split].into(),
                    },
                    Parsed::ToolcallDelta {
                        id: id.clone(),
                        delta: secret[split..].into(),
                    },
                ],
            ] {
                let events = persisted(events, &config);
                let text: String = events
                    .iter()
                    .map(|event| match event {
                        AgentEvent::TextDelta { text, .. }
                        | AgentEvent::ThinkingDelta { text, .. }
                        | AgentEvent::ToolcallDelta { delta: text, .. } => text.as_str(),
                        other => panic!("unexpected event: {other:?}"),
                    })
                    .collect();
                assert_eq!(text, "[REDACTED:api_key:API_KEY]");
                assert_eq!(events.len(), 2);
                assert_eq!(events[0].envelope().seq, 0);
                assert_eq!(events[1].envelope().seq, 1);
            }
        }
    }

    #[test]
    fn nonsecret_stream_content_survives_prefix_mismatches_and_normal_end() {
        use loom_events::ParsedAgentEvent as Parsed;
        let config = config_with_env(vec![("TOKEN".into(), "private-value".into())]);
        let chunks = ["long harmless text ending with p", "ri", "nt this p"];
        let mut events: Vec<_> = chunks
            .iter()
            .map(|text| Parsed::TextDelta {
                text: (*text).into(),
            })
            .collect();
        events.push(Parsed::TextEnd);
        let events = persisted(events, &config);
        let text: String = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::TextDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, chunks.concat());
        assert!(matches!(events.last(), Some(AgentEvent::TextEnd { .. })));
    }

    #[test]
    fn interrupted_secret_prefix_is_explicitly_redacted_before_later_events() {
        use loom_events::ParsedAgentEvent as Parsed;
        let config = config_with_env(vec![("TOKEN".into(), "private-value".into())]);
        for tail in [
            vec![],
            vec![Parsed::Error {
                message: "interrupted".into(),
            }],
        ] {
            let events = persisted(
                std::iter::once(Parsed::TextDelta {
                    text: "safe private-".into(),
                })
                .chain(tail)
                .collect(),
                &config,
            );
            assert!(
                matches!(&events[0], AgentEvent::TextDelta { text, .. } if text == "safe [REDACTED:token:TOKEN]")
            );
        }
    }

    #[test]
    fn overlapping_secrets_use_longest_match_without_rewriting_markers() {
        let config = config_with_env(vec![
            ("API_KEY".into(), "fixture".into()),
            ("TOKEN".into(), "fixture-long".into()),
            ("PASSWORD".into(), "api_key".into()),
            ("EMPTY_SECRET".into(), String::new()),
        ]);
        assert_eq!(
            redact_agent_input("fixture-long fixture", &config).text,
            "[REDACTED:token:TOKEN] [REDACTED:api_key:API_KEY]"
        );
    }

    #[test]
    fn json_escaped_secrets_are_redacted_in_opaque_tool_output() {
        let config = config_with_env(vec![("PRIVATE_KEY".into(), "fixture\n\"value\\".into())]);
        let text = serde_json::json!({"stdout": "fixture\n\"value\\", "exit_code": 0}).to_string();
        let redacted = redact_agent_input(&text, &config);
        let output: serde_json::Value = serde_json::from_str(&redacted.text).unwrap();
        assert_eq!(output["stdout"], "[REDACTED:secret:PRIVATE_KEY]");
        assert_eq!(output["exit_code"], 0);
    }

    #[test]
    fn redacts_secret_env_values_with_explicit_markers() {
        let cfg = config_with_env(vec![
            ("ANTHROPIC_API_KEY".into(), "sk-secret".into()),
            ("WRIX_AGENT".into(), "pi".into()),
        ]);
        let input = redact_agent_input("key sk-secret and agent pi", &cfg);
        assert_eq!(
            input.text,
            "key [REDACTED:api_key:ANTHROPIC_API_KEY] and agent pi",
        );
        let redactions = input.redactions.expect("redaction marker recorded");
        assert_eq!(redactions.len(), 1);
        assert_eq!(redactions[0].marker, "[REDACTED:api_key:ANTHROPIC_API_KEY]");
        assert_eq!(redactions[0].class, RedactionClass::ApiKey);
    }
}

use loom_driver::agent::SpawnConfig;
use loom_events::{InputRedaction, RedactionClass};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RedactedAgentInput {
    pub text: String,
    pub redactions: Option<Vec<InputRedaction>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SecretValue<'a> {
    name: &'a str,
    value: &'a str,
    class: RedactionClass,
}

pub(crate) fn redact_agent_input(text: &str, config: &SpawnConfig) -> RedactedAgentInput {
    let mut secrets: Vec<SecretValue<'_>> = config
        .env
        .iter()
        .chain(config.launcher_env.iter())
        .filter_map(|(name, value)| secret_value(name, value, text))
        .collect();
    secrets.sort_by(|a, b| b.value.len().cmp(&a.value.len()).then(a.name.cmp(b.name)));

    let mut redacted = text.to_string();
    let mut redactions = Vec::new();
    for secret in secrets {
        if !redacted.contains(secret.value) {
            continue;
        }
        let marker = redaction_marker(secret.name, &secret.class);
        redacted = redacted.replace(secret.value, &marker);
        if !redactions
            .iter()
            .any(|r: &InputRedaction| r.marker == marker)
        {
            redactions.push(InputRedaction {
                marker,
                class: secret.class,
            });
        }
    }

    RedactedAgentInput {
        text: redacted,
        redactions: (!redactions.is_empty()).then_some(redactions),
    }
}

fn secret_value<'a>(name: &'a str, value: &'a str, text: &str) -> Option<SecretValue<'a>> {
    if value.is_empty() || !text.contains(value) {
        return None;
    }
    secret_class(name).map(|class| SecretValue { name, value, class })
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
            scratch_dir: PathBuf::new(),
            model_id: None,
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

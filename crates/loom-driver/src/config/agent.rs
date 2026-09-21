use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::PhaseKey;
use crate::agent::{AgentKind, ModelSelection, OutputLimits, SpawnConfig, ThinkingLevel};
use crate::identifier::{ModelName, ProfileName, ProviderName};

/// `[phase.<name>]` table from `<workspace>/loom.toml`. Each per-phase
/// block deserializes into one of these; `[phase.default]` is the fallback
/// applied to any field a per-phase table does not set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PhaseConfig {
    /// Profile name (`base`, `rust`, `python`, …) used to select the
    /// container image. Resolves through the same chain as the agent
    /// fields when unset.
    pub profile: Option<ProfileName>,
    /// Agent-related fields. `agent.backend` / `agent.provider` /
    /// `agent.model_id` flatten naturally as dotted keys in TOML.
    pub agent: PhaseAgentConfig,
}

/// Agent fields nested under `[phase.<name>]`. Captured separately so
/// `agent.backend = "..."` style keys parse natively.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PhaseAgentConfig {
    pub backend: Option<AgentKind>,
    pub provider: Option<ProviderName>,
    pub model_id: Option<ModelName>,
    /// Reasoning effort parsed before phase fallback is applied.
    pub thinking_level: Option<ThinkingLevel>,
}

/// Workflow phase that resolves an [`AgentSelection`] from config.
///
/// `[phase.<phase>]` table keys in TOML correspond to the active workflow
/// phases. Unknown phase keys are rejected rather than selecting defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Phase {
    #[serde(rename = "plan")]
    Plan,
    #[serde(rename = "todo")]
    Todo,
    #[serde(rename = "loop")]
    Loop,
    #[serde(rename = "gate.review")]
    Review,
    #[serde(rename = "inbox")]
    Inbox,
}

impl Phase {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Phase::Plan => "plan",
            Phase::Todo => "todo",
            Phase::Loop => "loop",
            Phase::Review => "gate.review",
            Phase::Inbox => "inbox",
        }
    }
}

/// Phase-table key used as the resolver fallback. Every per-phase field
/// chain ends with `[phase.default]` before falling through to the
/// built-in defaults.
pub const DEFAULT_PHASE_KEY: &str = "default";

/// Built-in profile when neither `[phase.<name>]` nor `[phase.default]`
/// declares one.
pub const BUILT_IN_PROFILE: &str = "base";

/// Built-in backend name when neither `[phase.<name>]` nor
/// `[phase.default]` declares one.
pub const BUILT_IN_BACKEND: &str = "claude";

/// Claude-backend-specific runtime settings surfaced through
/// [`AgentSelection::claude_settings`] when the resolved backend is
/// [`AgentKind::Claude`]. Pi has no analog (no host-side permission flow).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeSettings {
    /// Tool names denied at host-side `control_request` time. Sourced from
    /// `[security] denied_tools`.
    pub denied_tools: Vec<String>,
    /// Seconds to wait for clean exit after `result` before SIGTERM.
    /// Sourced from `[claude] post_result_grace_secs`.
    pub post_result_grace_secs: u32,
}

/// Per-phase selection resolved by [`super::LoomConfig::agent_for`].
///
/// `profile` carries the profile name after walking
/// `[phase.<name>]` → `[phase.default]` → built-in. `kind` is the resolved
/// backend. `provider` / `model_id` hold the per-phase model override for
/// the pi backend (`set_model { provider, modelId }`). `claude_settings`
/// is populated only when `kind == Claude` so call sites can wire the
/// post-result grace period and denied-tools list without a second
/// config lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSelection {
    pub profile: ProfileName,
    pub backend: BackendSettings,
    pub provider: Option<ProviderName>,
    pub model_id: Option<ModelName>,
    /// Reasoning-effort hint forwarded to pi via `set_thinking_level` when
    /// the resolved backend is [`AgentKind::Pi`]. Claude has no analog;
    /// resolver carries the value through regardless so a non-pi phase that
    /// later switches backends still has the typed value at hand.
    pub thinking_level: Option<ThinkingLevel>,
}

/// Runtime and runtime-only settings form one exclusive choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendSettings {
    Pi,
    Claude(ClaudeSettings),
    Direct,
}

impl AgentSelection {
    pub const fn kind(&self) -> AgentKind {
        match self.backend {
            BackendSettings::Pi => AgentKind::Pi,
            BackendSettings::Claude(_) => AgentKind::Claude,
            BackendSettings::Direct => AgentKind::Direct,
        }
    }
    pub const fn claude_settings(&self) -> Option<&ClaudeSettings> {
        match &self.backend {
            BackendSettings::Claude(settings) => Some(settings),
            _ => None,
        }
    }
    /// Install resolved per-phase agent settings onto a spawn config.
    pub fn apply_to_spawn_config(
        &self,
        spawn: &mut SpawnConfig,
        direct_output_limits: OutputLimits,
    ) {
        spawn.model_id = None;
        spawn.model = None;
        spawn.thinking_level = None;
        spawn.output_limits = None;
        spawn.denied_tools.clear();

        match self.kind() {
            AgentKind::Pi => {
                if let (Some(provider), Some(model_id)) = (&self.provider, &self.model_id) {
                    spawn.model = Some(ModelSelection {
                        provider: provider.to_string(),
                        model_id: model_id.to_string(),
                    });
                }
                spawn.thinking_level = self.thinking_level;
            }
            AgentKind::Claude => {
                if let Some(settings) = self.claude_settings() {
                    spawn.denied_tools.clone_from(&settings.denied_tools);
                }
            }
            AgentKind::Direct => {
                spawn.model_id = self.model_id.as_ref().map(ToString::to_string);
                spawn.output_limits = Some(direct_output_limits);
            }
        }

        self.apply_api_key_allowlist(spawn, lookup_env_var);
    }

    fn apply_api_key_allowlist<F>(&self, spawn: &mut SpawnConfig, mut lookup: F)
    where
        F: FnMut(&str) -> Option<String>,
    {
        for var in self.required_api_key_vars() {
            if let Some(value) = lookup(&var) {
                upsert_env(&mut spawn.env, &var, value);
                info!(env_var = %var, "agent spawn env allowlist includes provider API key");
            }
        }
    }

    fn required_api_key_vars(&self) -> Vec<String> {
        let mut vars = Vec::new();
        match self.kind() {
            AgentKind::Pi => {
                if let Some(provider) = &self.provider {
                    push_provider_api_key_var(&mut vars, provider.as_str());
                }
                if let Some(model_id) = &self.model_id {
                    push_model_api_key_var(&mut vars, model_id.as_str());
                }
            }
            AgentKind::Direct => match self.model_id.as_ref().map(ModelName::as_str) {
                Some(model_id) => push_model_api_key_var(&mut vars, model_id),
                None => push_unique(&mut vars, ANTHROPIC_API_KEY_ENV),
            },
            AgentKind::Claude => {}
        }
        vars
    }
}

const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
const OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";
const GEMINI_API_KEY_ENV: &str = "GEMINI_API_KEY";
const GOOGLE_API_KEY_ENV: &str = "GOOGLE_API_KEY";

fn push_model_api_key_var(vars: &mut Vec<String>, model_id: &str) {
    let lower = model_id.to_ascii_lowercase();
    if lower.starts_with("claude") {
        push_unique(vars, ANTHROPIC_API_KEY_ENV);
    } else if lower.starts_with("gpt") || lower.starts_with("o1") || lower.starts_with("o3") {
        push_unique(vars, OPENAI_API_KEY_ENV);
    } else if lower.starts_with("gemini") {
        push_unique(vars, GEMINI_API_KEY_ENV);
    }
}

fn push_provider_api_key_var(vars: &mut Vec<String>, provider: &str) {
    match provider.to_ascii_lowercase().as_str() {
        "anthropic" | "claude" => push_unique(vars, ANTHROPIC_API_KEY_ENV),
        "openai" => push_unique(vars, OPENAI_API_KEY_ENV),
        "google" => {
            push_unique(vars, GOOGLE_API_KEY_ENV);
            push_unique(vars, GEMINI_API_KEY_ENV);
        }
        "gemini" => push_unique(vars, GEMINI_API_KEY_ENV),
        other => {
            if let Some(var) = provider_api_key_env(other) {
                push_unique_owned(vars, var);
            }
        }
    }
}

fn provider_api_key_env(provider: &str) -> Option<String> {
    let mut name = String::new();
    for ch in provider.chars() {
        if ch.is_ascii_alphanumeric() {
            name.push(ch.to_ascii_uppercase());
        } else if !name.ends_with('_') {
            name.push('_');
        }
    }
    let name = name.trim_matches('_');
    if name.is_empty() {
        None
    } else {
        Some(format!("{name}_API_KEY"))
    }
}

fn push_unique(vars: &mut Vec<String>, var: &str) {
    push_unique_owned(vars, var.to_string());
}

fn push_unique_owned(vars: &mut Vec<String>, var: String) {
    if !vars.iter().any(|existing| existing == &var) {
        vars.push(var);
    }
}

fn lookup_env_var(var: &str) -> Option<String> {
    match std::env::var(var) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            warn!(env_var = %var, "agent spawn env skipped non-unicode provider API key");
            None
        }
    }
}

fn upsert_env(env: &mut Vec<(String, String)>, key: &str, value: String) {
    if let Some((_, existing)) = env.iter_mut().find(|(existing, _)| existing == key) {
        *existing = value;
    } else {
        env.push((key.to_string(), value));
    }
}

/// Resolve a single optional phase field via the
/// `[phase.<name>]` → `[phase.default]` chain. Returns `None` only when
/// neither the named phase nor `default` populates the field.
pub(super) fn lookup_phase_field<'a, T, F>(
    phase: &'a BTreeMap<PhaseKey, PhaseConfig>,
    name: Phase,
    f: F,
) -> Option<&'a T>
where
    F: Fn(&'a PhaseConfig) -> &'a Option<T>,
{
    phase
        .get(&PhaseKey::Named(name))
        .and_then(|p| f(p).as_ref())
        .or_else(|| phase.get(&PhaseKey::Default).and_then(|p| f(p).as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{ImageSourceKind, RePinContent};
    use std::path::PathBuf;

    fn selection(
        kind: AgentKind,
        provider: Option<&str>,
        model_id: Option<&str>,
    ) -> AgentSelection {
        AgentSelection {
            profile: ProfileName::new("base").unwrap(),
            backend: super::super::LoomConfig::default().backend_settings(kind),
            provider: provider.map(|value| value.parse().unwrap()),
            model_id: model_id.map(|value| value.parse().unwrap()),
            thinking_level: None,
        }
    }

    fn spawn_config() -> SpawnConfig {
        SpawnConfig {
            image_ref: "localhost/wrix:tag".into(),
            image_source: PathBuf::from("/nix/store/wrix-image"),
            image_source_kind: Some(ImageSourceKind::NixDescriptor),
            wrix_launcher: None,
            profile_config: None,
            workspace: PathBuf::from("/workspace"),
            env: vec![("WRIX_AGENT".into(), "direct".into())],
            mounts: Vec::new(),
            initial_prompt: "prompt".into(),
            agent_args: Vec::new(),
            repin: RePinContent {
                orientation: String::new(),
                pinned_context: String::new(),
                partial_bodies: Vec::new(),
            },
            skills: None,
            event_metadata: None,
            scratch_dir: PathBuf::from("/workspace/.loom/scratch/k"),
            model_id: None,
            model: None,
            thinking_level: None,
            observers: crate::config::AgentObserversConfig::default(),
            output_limits: None,
            shutdown_grace: None,
            denied_tools: Vec::new(),
            handshake_timeout: None,
            stall_warn_interval: None,
            launcher_env: Vec::new(),
        }
    }

    #[test]
    fn phase_round_trips_through_serde() {
        for (phase, expected) in [
            (Phase::Plan, "plan"),
            (Phase::Todo, "todo"),
            (Phase::Loop, "loop"),
            (Phase::Review, "gate.review"),
            (Phase::Inbox, "inbox"),
        ] {
            assert_eq!(
                serde_json::to_string(&phase).unwrap(),
                format!("\"{expected}\"")
            );
            let back: Phase = serde_json::from_str(&format!("\"{expected}\"")).unwrap();
            assert_eq!(back, phase);
            assert_eq!(phase.as_str(), expected);
        }
    }

    #[test]
    fn phase_backend_field_accepts_claude_pi_and_direct() {
        for (name, expected) in [
            ("claude", AgentKind::Claude),
            ("pi", AgentKind::Pi),
            ("direct", AgentKind::Direct),
        ] {
            let agent: PhaseAgentConfig =
                serde_json::from_value(serde_json::json!({"backend": name})).unwrap();
            assert_eq!(agent.backend, Some(expected));
        }
    }

    #[test]
    fn phase_backend_field_rejects_unknown() {
        assert!(
            serde_json::from_value::<PhaseAgentConfig>(serde_json::json!({"backend": "gpt"}))
                .is_err()
        );
    }

    #[test]
    fn phase_thinking_field_accepts_every_documented_level() {
        for (token, expected) in [
            ("off", ThinkingLevel::Off),
            ("minimal", ThinkingLevel::Minimal),
            ("low", ThinkingLevel::Low),
            ("medium", ThinkingLevel::Medium),
            ("high", ThinkingLevel::High),
            ("xhigh", ThinkingLevel::Xhigh),
        ] {
            let agent: PhaseAgentConfig =
                serde_json::from_value(serde_json::json!({"thinking_level": token})).unwrap();
            assert_eq!(agent.thinking_level, Some(expected));
        }
    }

    #[test]
    fn phase_thinking_field_rejects_unknown() {
        assert!(
            serde_json::from_value::<PhaseAgentConfig>(
                serde_json::json!({"thinking_level": "ultra"})
            )
            .is_err()
        );
    }

    #[test]
    fn lookup_phase_field_prefers_named_over_default() {
        let mut phase = BTreeMap::new();
        phase.insert(
            PhaseKey::Default,
            PhaseConfig {
                profile: Some(ProfileName::base()),
                ..PhaseConfig::default()
            },
        );
        phase.insert(
            PhaseKey::Named(Phase::Todo),
            PhaseConfig {
                profile: Some(ProfileName::rust()),
                ..PhaseConfig::default()
            },
        );
        let resolved = lookup_phase_field(&phase, Phase::Todo, |p| &p.profile).unwrap();
        assert_eq!(resolved.as_str(), "rust");
    }

    #[test]
    fn lookup_phase_field_falls_back_to_default_when_named_unset() {
        let mut phase = BTreeMap::new();
        phase.insert(
            PhaseKey::Default,
            PhaseConfig {
                profile: Some(ProfileName::base()),
                ..PhaseConfig::default()
            },
        );
        phase.insert(PhaseKey::Named(Phase::Todo), PhaseConfig::default());
        let resolved = lookup_phase_field(&phase, Phase::Todo, |p| &p.profile).unwrap();
        assert_eq!(resolved.as_str(), "base");
    }

    #[test]
    fn lookup_phase_field_returns_none_when_neither_set() {
        let phase: BTreeMap<PhaseKey, PhaseConfig> = BTreeMap::new();
        assert!(lookup_phase_field(&phase, Phase::Todo, |p| &p.profile).is_none());
    }

    #[test]
    fn direct_anthropic_model_allows_anthropic_api_key() {
        let mut spawn = spawn_config();
        selection(AgentKind::Direct, None, Some("claude-sonnet-4-6"))
            .apply_api_key_allowlist(&mut spawn, |var| {
                (var == "ANTHROPIC_API_KEY").then(|| "anthropic-value".to_string())
            });
        assert!(
            spawn
                .env
                .iter()
                .any(|(key, value)| key == "ANTHROPIC_API_KEY" && value == "anthropic-value"),
            "ANTHROPIC_API_KEY must reach direct Anthropic sessions: {:?}",
            spawn.env,
        );
    }

    #[test]
    fn direct_default_model_allows_anthropic_api_key() {
        let mut spawn = spawn_config();
        selection(AgentKind::Direct, None, None).apply_api_key_allowlist(&mut spawn, |var| {
            (var == "ANTHROPIC_API_KEY").then(|| "default-anthropic-value".to_string())
        });
        assert!(
            spawn.env.iter().any(
                |(key, value)| key == "ANTHROPIC_API_KEY" && value == "default-anthropic-value"
            ),
            "Direct's default Anthropic model needs ANTHROPIC_API_KEY: {:?}",
            spawn.env,
        );
    }

    #[test]
    fn pi_provider_specific_key_is_allowlisted_when_present() {
        let mut spawn = spawn_config();
        selection(AgentKind::Pi, Some("deepseek"), Some("deepseek-v3"))
            .apply_api_key_allowlist(&mut spawn, |var| {
                (var == "DEEPSEEK_API_KEY").then(|| "deepseek-value".to_string())
            });
        assert!(
            spawn
                .env
                .iter()
                .any(|(key, value)| key == "DEEPSEEK_API_KEY" && value == "deepseek-value"),
            "provider-derived API key env var missing: {:?}",
            spawn.env,
        );
    }

    #[test]
    fn api_key_allowlist_updates_existing_env_entry_without_duplicate() {
        let mut spawn = spawn_config();
        spawn
            .env
            .push(("OPENAI_API_KEY".to_string(), "old".to_string()));
        selection(AgentKind::Direct, None, Some("gpt-5.5"))
            .apply_api_key_allowlist(&mut spawn, |var| {
                (var == "OPENAI_API_KEY").then(|| "new".to_string())
            });
        let entries = spawn
            .env
            .iter()
            .filter(|(key, _)| key == "OPENAI_API_KEY")
            .collect::<Vec<_>>();
        assert_eq!(
            entries,
            vec![&("OPENAI_API_KEY".to_string(), "new".to_string())]
        );
    }
}

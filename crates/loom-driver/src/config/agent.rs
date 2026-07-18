use std::collections::BTreeMap;

use displaydoc::Display;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{info, warn};

use crate::agent::{AgentKind, ModelSelection, OutputLimits, SpawnConfig, ThinkingLevel};
use crate::identifier::ProfileName;

/// `[phase.<name>]` table from `<workspace>/loom.toml`. Each per-phase
/// block deserializes into one of these; `[phase.default]` is the fallback
/// applied to any field a per-phase table does not set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct PhaseConfig {
    /// Profile name (`base`, `rust`, `python`, …) used to select the
    /// container image. Resolves through the same chain as the agent
    /// fields when unset.
    pub profile: Option<String>,
    /// Agent-related fields. `agent.backend` / `agent.provider` /
    /// `agent.model_id` flatten naturally as dotted keys in TOML.
    pub agent: PhaseAgentConfig,
}

/// Agent fields nested under `[phase.<name>]`. Captured separately so
/// `agent.backend = "..."` style keys parse natively.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct PhaseAgentConfig {
    pub backend: Option<String>,
    pub provider: Option<String>,
    pub model_id: Option<String>,
    /// Reasoning-effort hint forwarded to pi via `set_thinking_level`. Stays
    /// stringly-typed at the TOML layer so `agent.thinking_level = "off"`
    /// parses natively; [`super::LoomConfig::agent_for`] converts the value
    /// to [`ThinkingLevel`] and surfaces typos as
    /// [`AgentSelectionError::UnknownThinkingLevel`].
    pub thinking_level: Option<String>,
}

/// Workflow phase that resolves an [`AgentSelection`] from config.
///
/// `[phase.<phase>]` table keys in TOML correspond to the active workflow
/// phases. `loom loop` resolves `[phase.loop]`; LLM review resolves
/// `[phase.gate.review]`. The `BTreeMap` that backs `[phase.*]` remains
/// string-keyed so unknown TOML keys parse without error and the resolver's
/// `[phase.default]` fallback is just another lookup against the same map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    pub fn as_str(&self) -> &'static str {
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
    pub kind: AgentKind,
    pub provider: Option<String>,
    pub model_id: Option<String>,
    /// Reasoning-effort hint forwarded to pi via `set_thinking_level` when
    /// the resolved backend is [`AgentKind::Pi`]. Claude has no analog;
    /// resolver carries the value through regardless so a non-pi phase that
    /// later switches backends still has the typed value at hand.
    pub thinking_level: Option<ThinkingLevel>,
    pub claude_settings: Option<ClaudeSettings>,
}

impl AgentSelection {
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

        match self.kind {
            AgentKind::Pi => {
                if let (Some(provider), Some(model_id)) = (&self.provider, &self.model_id) {
                    spawn.model = Some(ModelSelection {
                        provider: provider.clone(),
                        model_id: model_id.clone(),
                    });
                }
                spawn.thinking_level = self.thinking_level;
            }
            AgentKind::Claude => {
                if let Some(settings) = &self.claude_settings {
                    spawn.denied_tools = settings.denied_tools.clone();
                }
            }
            AgentKind::Direct => {
                spawn.model_id = self.model_id.clone();
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
        match self.kind {
            AgentKind::Pi => {
                if let Some(provider) = &self.provider {
                    push_provider_api_key_var(&mut vars, provider);
                }
                if let Some(model_id) = &self.model_id {
                    push_model_api_key_var(&mut vars, model_id);
                }
            }
            AgentKind::Direct => match self.model_id.as_deref() {
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

#[derive(Debug, Display, Error, PartialEq, Eq)]
pub enum AgentSelectionError {
    /// unknown agent backend `{name}` in config (expected `claude`, `pi`, or `direct`)
    UnknownBackend { name: String },
    /// unknown agent.thinking_level `{name}` in config (expected one of `off`, `minimal`, `low`, `medium`, `high`, `xhigh`)
    UnknownThinkingLevel { name: String },
}

/// Convert a backend name string (from `[phase.<name>] agent.backend` or
/// `[phase.default] agent.backend`) into the typed [`AgentKind`].
pub fn parse_backend_name(name: &str) -> Result<AgentKind, AgentSelectionError> {
    name.parse()
        .map_err(|_| AgentSelectionError::UnknownBackend {
            name: name.to_string(),
        })
}

/// Convert a `agent.thinking_level` TOML string into the typed
/// [`ThinkingLevel`]. The accepted vocabulary matches `specs/agent.md`'s
/// Pi command table; typos surface as
/// [`AgentSelectionError::UnknownThinkingLevel`] rather than silently
/// dropping the override.
pub fn parse_thinking_level_name(name: &str) -> Result<ThinkingLevel, AgentSelectionError> {
    match name {
        "off" => Ok(ThinkingLevel::Off),
        "minimal" => Ok(ThinkingLevel::Minimal),
        "low" => Ok(ThinkingLevel::Low),
        "medium" => Ok(ThinkingLevel::Medium),
        "high" => Ok(ThinkingLevel::High),
        "xhigh" => Ok(ThinkingLevel::Xhigh),
        other => Err(AgentSelectionError::UnknownThinkingLevel {
            name: other.to_string(),
        }),
    }
}

/// Resolve a single optional phase field via the
/// `[phase.<name>]` → `[phase.default]` chain. Returns `None` only when
/// neither the named phase nor `default` populates the field.
pub(super) fn lookup_phase_field<'a, T, F>(
    phase: &'a BTreeMap<String, PhaseConfig>,
    name: &str,
    f: F,
) -> Option<&'a T>
where
    F: Fn(&'a PhaseConfig) -> &'a Option<T>,
{
    phase
        .get(name)
        .and_then(|p| f(p).as_ref())
        .or_else(|| phase.get(DEFAULT_PHASE_KEY).and_then(|p| f(p).as_ref()))
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
            profile: ProfileName::new("base"),
            kind,
            provider: provider.map(str::to_string),
            model_id: model_id.map(str::to_string),
            thinking_level: None,
            claude_settings: None,
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
            observers: Default::default(),
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
    fn parse_backend_name_accepts_claude_pi_and_direct() {
        assert_eq!(parse_backend_name("claude").unwrap(), AgentKind::Claude);
        assert_eq!(parse_backend_name("pi").unwrap(), AgentKind::Pi);
        assert_eq!(parse_backend_name("direct").unwrap(), AgentKind::Direct);
    }

    #[test]
    fn parse_backend_name_rejects_unknown() {
        match parse_backend_name("gpt") {
            Err(AgentSelectionError::UnknownBackend { name }) => assert_eq!(name, "gpt"),
            other => panic!("expected UnknownBackend, got {other:?}"),
        }
    }

    #[test]
    fn parse_thinking_level_name_accepts_every_documented_level() {
        for (token, expected) in [
            ("off", ThinkingLevel::Off),
            ("minimal", ThinkingLevel::Minimal),
            ("low", ThinkingLevel::Low),
            ("medium", ThinkingLevel::Medium),
            ("high", ThinkingLevel::High),
            ("xhigh", ThinkingLevel::Xhigh),
        ] {
            assert_eq!(parse_thinking_level_name(token).unwrap(), expected);
        }
    }

    #[test]
    fn parse_thinking_level_name_rejects_unknown() {
        match parse_thinking_level_name("ultra") {
            Err(AgentSelectionError::UnknownThinkingLevel { name }) => assert_eq!(name, "ultra"),
            other => panic!("expected UnknownThinkingLevel, got {other:?}"),
        }
    }

    #[test]
    fn lookup_phase_field_prefers_named_over_default() {
        let mut phase = BTreeMap::new();
        phase.insert(
            DEFAULT_PHASE_KEY.to_string(),
            PhaseConfig {
                profile: Some("base".to_string()),
                ..PhaseConfig::default()
            },
        );
        phase.insert(
            "todo".to_string(),
            PhaseConfig {
                profile: Some("rust".to_string()),
                ..PhaseConfig::default()
            },
        );
        let resolved = lookup_phase_field(&phase, "todo", |p| &p.profile).unwrap();
        assert_eq!(resolved, "rust");
    }

    #[test]
    fn lookup_phase_field_falls_back_to_default_when_named_unset() {
        let mut phase = BTreeMap::new();
        phase.insert(
            DEFAULT_PHASE_KEY.to_string(),
            PhaseConfig {
                profile: Some("base".to_string()),
                ..PhaseConfig::default()
            },
        );
        phase.insert("todo".to_string(), PhaseConfig::default());
        let resolved = lookup_phase_field(&phase, "todo", |p| &p.profile).unwrap();
        assert_eq!(resolved, "base");
    }

    #[test]
    fn lookup_phase_field_returns_none_when_neither_set() {
        let phase: BTreeMap<String, PhaseConfig> = BTreeMap::new();
        assert!(lookup_phase_field(&phase, "todo", |p| &p.profile).is_none());
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

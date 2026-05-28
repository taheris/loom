//! Loom configuration loaded from `<workspace>/config.toml`.
//!
//! Parsed natively via the `toml` crate into a typed [`LoomConfig`]. Every
//! field carries `#[serde(default)]` so a missing or empty file yields
//! sensible defaults — users can run Loom with no config at all.
//!
//! Per-phase agent and profile selection lives in `[phase.<name>]` tables
//! with `[phase.default]` as the fallback (see
//! `specs/harness.md` § Configuration). Resolution for any phase
//! field walks `[phase.<name>]` → `[phase.default]` → built-in defaults.
//!
//! The on-disk path is resolved via [`LoomConfig::resolve_path`]: when
//! `LOOM_CONFIG` is set, its value is the path (absolute or
//! cwd-relative); otherwise the path is `<workspace>/config.toml`.

mod agent;
mod agent_observer;
mod beads;
mod claude;
mod error;
mod logs;
mod loop_config;
mod runner;
mod security;

pub use agent::{
    AgentSelection, AgentSelectionError, BUILT_IN_BACKEND, BUILT_IN_PROFILE, ClaudeSettings,
    DEFAULT_PHASE_KEY, Phase, PhaseAgentConfig, PhaseConfig, parse_backend_name,
    parse_thinking_level_name,
};
pub use agent_observer::{AgentObserversConfig, DoomLoopConfig, DuplicateResultConfig};
pub use beads::BeadsConfig;
pub use claude::ClaudeConfig;
pub use error::LoomConfigError;
pub use logs::LogsConfig;
pub use loop_config::LoopConfig;
pub use runner::{Parser, RunnerConfig, RunnerEntry, RunnerTier};
pub use security::SecurityConfig;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Env-var name that overrides the default config-file lookup. When set,
/// its value is taken as the config path (absolute or cwd-relative) and
/// the `<workspace>/config.toml` default is not consulted.
pub const CONFIG_PATH_ENV: &str = "LOOM_CONFIG";

/// Default config-file location, relative to the workspace root. Used
/// when [`CONFIG_PATH_ENV`] is unset.
pub const DEFAULT_CONFIG_FILENAME: &str = "config.toml";

use crate::agent::AgentKind;
use crate::identifier::ProfileName;
use agent::lookup_phase_field;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct LoomConfig {
    pub pinned_context: String,
    /// Workspace-relative path to the style-rules document. Pinned in the
    /// `run` and `review` phases via `partial/style_rules.md` (see
    /// `specs/templates.md` § Style-Rules Partial).
    pub style_rules: String,
    /// Workspace-relative path to the spec-authoring conventions document.
    /// Pinned in the `plan_new` and `plan_update` phases via
    /// `partial/spec_conventions.md` (see `specs/templates.md`
    /// § Spec-Conventions Partial).
    pub spec_conventions: String,
    pub beads: BeadsConfig,
    #[serde(rename = "loop")]
    pub loop_: LoopConfig,
    pub logs: LogsConfig,
    /// `[phase.<name>]` tables keyed by phase name. The literal key
    /// `default` is the fallback applied by [`LoomConfig::agent_for`] to
    /// any field a per-phase table does not declare.
    pub phase: BTreeMap<String, PhaseConfig>,
    pub claude: ClaudeConfig,
    pub security: SecurityConfig,
    /// `[runner.<tier>.<name>]` blocks per `specs/gate.md` § Runners.
    /// The runtime dispatcher reads this map; an empty table parses as
    /// `RunnerConfig::default()` so consumers without runner overrides
    /// fall back to toolchain detection.
    pub runner: RunnerConfig,
    /// `[agent]` block — observer-composition knobs (`[agent.doom_loop]`
    /// / `[agent.duplicate_result]`). Workflow's Pi/Claude dispatch
    /// reads this to materialise the default observer chain.
    pub agent: AgentObserversConfig,
}

impl Default for LoomConfig {
    fn default() -> Self {
        Self {
            pinned_context: "docs/README.md".to_string(),
            style_rules: "docs/style-rules.md".to_string(),
            spec_conventions: "docs/spec-conventions.md".to_string(),
            beads: BeadsConfig::default(),
            loop_: LoopConfig::default(),
            logs: LogsConfig::default(),
            phase: BTreeMap::new(),
            claude: ClaudeConfig::default(),
            security: SecurityConfig::default(),
            runner: RunnerConfig::default(),
            agent: AgentObserversConfig::default(),
        }
    }
}

impl LoomConfig {
    /// Parse a `LoomConfig` from a TOML string. An empty string yields the
    /// full default config.
    ///
    /// Pin-path fields (`pinned_context`, `style_rules`, `spec_conventions`)
    /// are rejected when empty: blanking a config does not disable the
    /// pin — to genuinely drop a pin, remove the corresponding
    /// `{% include %}` from the relevant template.
    pub fn from_toml_str(src: &str) -> Result<Self, LoomConfigError> {
        let cfg: Self = toml::from_str(src)?;
        for (field, value) in [
            ("pinned_context", &cfg.pinned_context),
            ("style_rules", &cfg.style_rules),
            ("spec_conventions", &cfg.spec_conventions),
        ] {
            if value.is_empty() {
                return Err(LoomConfigError::EmptyPath { field });
            }
        }
        Ok(cfg)
    }

    /// Resolve the [`AgentSelection`] for `phase`. Each field walks the
    /// `[phase.<name>]` → `[phase.default]` → built-in chain; when the
    /// resolved backend is [`crate::agent::AgentKind::Claude`] the
    /// claude-specific settings are pulled from `[claude]` and `[security]`
    /// so call sites receive everything in one struct.
    ///
    /// Returns [`AgentSelectionError::UnknownBackend`] when the backend name
    /// (per-phase or default) does not match `claude` or `pi` — surfacing
    /// the validation lazily lets the TOML parser stay schema-free for
    /// unknown `[phase.<phase>]` keys.
    pub fn agent_for(&self, phase: Phase) -> Result<AgentSelection, AgentSelectionError> {
        let key = phase.as_str();
        let profile_str = lookup_phase_field(&self.phase, key, |p| &p.profile)
            .map(String::as_str)
            .unwrap_or(BUILT_IN_PROFILE);
        let backend_str = lookup_phase_field(&self.phase, key, |p| &p.agent.backend)
            .map(String::as_str)
            .unwrap_or(BUILT_IN_BACKEND);
        let kind = parse_backend_name(backend_str)?;
        let provider = lookup_phase_field(&self.phase, key, |p| &p.agent.provider).cloned();
        let model_id = lookup_phase_field(&self.phase, key, |p| &p.agent.model_id).cloned();
        let thinking_level = lookup_phase_field(&self.phase, key, |p| &p.agent.thinking_level)
            .map(String::as_str)
            .map(parse_thinking_level_name)
            .transpose()?;
        let claude_settings = match kind {
            AgentKind::Claude => Some(ClaudeSettings {
                denied_tools: self.security.denied_tools.clone(),
                post_result_grace_secs: self.claude.post_result_grace_secs,
            }),
            AgentKind::Pi => None,
        };
        Ok(AgentSelection {
            profile: ProfileName::new(profile_str),
            kind,
            provider,
            model_id,
            thinking_level,
            claude_settings,
        })
    }

    /// Resolve the on-disk config path for `workspace`. Returns the value
    /// of [`CONFIG_PATH_ENV`] when set; otherwise returns
    /// `<workspace>/config.toml`. The path is not required to exist —
    /// [`Self::load`] tolerates a missing file.
    pub fn resolve_path(workspace: &Path) -> PathBuf {
        if let Some(override_path) = std::env::var_os(CONFIG_PATH_ENV) {
            PathBuf::from(override_path)
        } else {
            workspace.join(DEFAULT_CONFIG_FILENAME)
        }
    }

    /// Load a config from disk. A missing file yields the default config so
    /// the file is optional.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, LoomConfigError> {
        let path = path.as_ref();
        match std::fs::read_to_string(path) {
            Ok(s) => Self::from_toml_str(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(LoomConfigError::Read {
                path: path.to_path_buf(),
                source,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    /// The example TOML reproduced verbatim from the Configuration section
    /// of `specs/harness.md`. Any drift between the parser and the
    /// spec example surfaces here. Note the example explicitly writes the
    /// built-in defaults under `[phase.default]`; we do not assert
    /// `cfg == LoomConfig::default()` because the populated map and
    /// `BTreeMap::new()` are not structurally equal — instead the test
    /// asserts that `agent_for` resolves the same values either way.
    const SPEC_EXAMPLE: &str = r#"pinned_context = "docs/README.md"

[beads]
priority = 2
default_type = "task"

[loop]
# Molecule-level: bounds `loom loop`'s outer loop on fix-up beads. Each
# full molecule pass — initial pass plus every verdict-gate-produced
# fix-up pass — consumes one slot.
max_iterations = 10
# In-session: per-bead retry-with-`previous_failure` budget inside one
# `process_one_bead` call. Independent of `max_iterations`.
max_retries = 2

[logs]
# Delete log files under .wrapix/loom/logs/ older than this many days on
# `loom loop` startup. 0 disables sweeping (keep forever).
retention_days = 14

# Per-phase config. Resolution for any field: [phase.<name>] →
# [phase.default] → built-in. `loom loop` reads its profile from the
# bead's `profile:X` label first, then [phase.run] / [phase.default];
# the `--profile` CLI flag overrides everything.
[phase.default]
profile = "base"
agent.backend = "claude"

# [phase.todo]
# profile = "rust"
# agent.backend = "pi"
# agent.provider = "deepseek"
# agent.model_id = "deepseek-v3"
#
# [phase.check]
# agent.backend = "claude"

[claude]
# Agent-runtime settings, applied wherever claude is selected. Seconds to
# wait for clean exit after `result` before SIGTERM (shutdown watchdog).
post_result_grace_secs = 5

[security]
# Tool names to deny when claude sends control_request. Claude-only —
# pi has no host-side permission flow (tools execute internally, no
# control_request analog). Empty by default; the container sandbox is
# the trust boundary.
# denied_tools = ["SomeNewHostTool"]
"#;

    #[test]
    fn empty_string_yields_defaults() -> Result<()> {
        let cfg = LoomConfig::from_toml_str("")?;
        assert_eq!(cfg, LoomConfig::default());
        Ok(())
    }

    /// The three pin-path fields default to the bundled documents — see
    /// `specs/templates.md` § Configuration. A user transitioning
    /// without writing a config file should still get pins pointing at the
    /// canonical paths.
    #[test]
    fn pin_paths_default_to_bundled_docs() {
        let cfg = LoomConfig::default();
        assert_eq!(cfg.pinned_context, "docs/README.md");
        assert_eq!(cfg.style_rules, "docs/style-rules.md");
        assert_eq!(cfg.spec_conventions, "docs/spec-conventions.md");
    }

    /// Empty values for any pin-path field are rejected at parse time —
    /// blanking the value does not disable the pin (the template would still
    /// render an empty path). The error names the offending field so the
    /// user can find it in their config.
    #[test]
    fn empty_pin_path_returns_empty_path_error() {
        for (toml_field, expected_field) in [
            ("pinned_context", "pinned_context"),
            ("style_rules", "style_rules"),
            ("spec_conventions", "spec_conventions"),
        ] {
            let src = format!("{toml_field} = \"\"\n");
            match LoomConfig::from_toml_str(&src) {
                Err(LoomConfigError::EmptyPath { field }) => {
                    assert_eq!(
                        field, expected_field,
                        "wrong field reported for {toml_field}"
                    );
                }
                other => panic!(
                    "expected EmptyPath {{ field: {expected_field} }} for empty {toml_field}, got {other:?}"
                ),
            }
        }
    }

    /// The spec example writes the built-in defaults explicitly under
    /// `[phase.default]`; both that form and the empty config resolve to
    /// the same values via `agent_for` for every phase.
    #[test]
    fn spec_example_resolves_to_built_in_defaults() -> Result<()> {
        let from_spec = LoomConfig::from_toml_str(SPEC_EXAMPLE)?;
        let empty = LoomConfig::default();
        for phase in [
            Phase::Plan,
            Phase::Todo,
            Phase::Run,
            Phase::Check,
            Phase::Review,
            Phase::Msg,
        ] {
            let from_spec_sel = from_spec.agent_for(phase).expect("agent_for");
            let empty_sel = empty.agent_for(phase).expect("agent_for");
            assert_eq!(from_spec_sel, empty_sel, "phase={phase:?}");
            assert_eq!(from_spec_sel.profile.as_str(), BUILT_IN_PROFILE);
            assert_eq!(from_spec_sel.kind, AgentKind::Claude);
        }
        // Non-phase fields round-trip identically.
        assert_eq!(from_spec.pinned_context, empty.pinned_context);
        assert_eq!(from_spec.beads, empty.beads);
        assert_eq!(from_spec.loop_, empty.loop_);
        assert_eq!(from_spec.logs, empty.logs);
        assert_eq!(from_spec.claude, empty.claude);
        assert_eq!(from_spec.security, empty.security);
        assert_eq!(from_spec.agent, empty.agent);
        Ok(())
    }

    /// `[agent.doom_loop]` / `[agent.duplicate_result]` round-trip through
    /// the full `LoomConfig` parser the way the spec's Configuration block
    /// promises. Disabling either observer in TOML lands on the typed
    /// `enabled = false` so the workflow's chain composition skips it.
    #[test]
    fn agent_observer_block_round_trips_via_loom_config() -> Result<()> {
        let src = r#"
[agent.doom_loop]
enabled = false
window = 8
threshold = 4
stage_2_after_stage_1 = 2

[agent.duplicate_result]
enabled = false
min_bytes = 1024
"#;
        let cfg = LoomConfig::from_toml_str(src)?;
        assert!(!cfg.agent.doom_loop.enabled);
        assert_eq!(cfg.agent.doom_loop.window, 8);
        assert_eq!(cfg.agent.doom_loop.threshold, 4);
        assert_eq!(cfg.agent.doom_loop.stage_2_after_stage_1, 2);
        assert!(!cfg.agent.duplicate_result.enabled);
        assert_eq!(cfg.agent.duplicate_result.min_bytes, 1024);
        Ok(())
    }

    #[test]
    fn partial_file_fills_remaining_with_defaults() -> Result<()> {
        let src = r#"
pinned_context = "AGENTS.md"

[loop]
max_retries = 5
"#;
        let cfg = LoomConfig::from_toml_str(src)?;
        assert_eq!(cfg.pinned_context, "AGENTS.md");
        assert_eq!(cfg.loop_.max_retries, 5);
        // Other [loop] fields fall back to defaults.
        assert_eq!(cfg.loop_.max_iterations, 10);
        // Whole sections that are absent stay at defaults.
        assert_eq!(cfg.beads, BeadsConfig::default());
        assert!(cfg.phase.is_empty());
        assert_eq!(cfg.claude, ClaudeConfig::default());
        assert_eq!(cfg.security, SecurityConfig::default());
        assert!(cfg.runner.0.is_empty());
        Ok(())
    }

    #[test]
    fn phase_tables_collect_into_map() -> Result<()> {
        let src = r#"
[phase.default]
profile = "base"
agent.backend = "pi"

[phase.todo]
profile = "rust"
agent.backend = "pi"
agent.provider = "deepseek"
agent.model_id = "deepseek-v3"

[phase.check]
agent.backend = "claude"
"#;
        let cfg = LoomConfig::from_toml_str(src)?;
        assert_eq!(cfg.phase.len(), 3);

        let default = &cfg.phase[DEFAULT_PHASE_KEY];
        assert_eq!(default.profile.as_deref(), Some("base"));
        assert_eq!(default.agent.backend.as_deref(), Some("pi"));

        let todo = &cfg.phase["todo"];
        assert_eq!(todo.profile.as_deref(), Some("rust"));
        assert_eq!(todo.agent.backend.as_deref(), Some("pi"));
        assert_eq!(todo.agent.provider.as_deref(), Some("deepseek"));
        assert_eq!(todo.agent.model_id.as_deref(), Some("deepseek-v3"));

        let check = &cfg.phase["check"];
        assert!(check.profile.is_none());
        assert_eq!(check.agent.backend.as_deref(), Some("claude"));
        assert!(check.agent.provider.is_none());
        Ok(())
    }

    #[test]
    fn security_denied_tools_parses_list() -> Result<()> {
        let src = r#"
[security]
denied_tools = ["WebFetch", "DangerousTool"]
"#;
        let cfg = LoomConfig::from_toml_str(src)?;
        assert_eq!(cfg.security.denied_tools, vec!["WebFetch", "DangerousTool"]);
        Ok(())
    }

    #[test]
    fn load_missing_file_yields_defaults() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let cfg = LoomConfig::load(dir.path().join("does-not-exist.toml"))?;
        assert_eq!(cfg, LoomConfig::default());
        Ok(())
    }

    #[test]
    fn load_reads_file_from_disk() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "pinned_context = \"AGENTS.md\"\n")?;
        let cfg = LoomConfig::load(&path)?;
        assert_eq!(cfg.pinned_context, "AGENTS.md");
        Ok(())
    }

    #[test]
    fn invalid_toml_returns_parse_error() {
        let result = LoomConfig::from_toml_str("not = = valid");
        assert!(matches!(result, Err(LoomConfigError::Parse(_))));
    }

    /// `[phase.default] agent.backend = "claude"` with `[phase.todo]
    /// agent.backend = "pi"` → `agent_for(Todo)` returns `Pi`,
    /// `agent_for(Run)` inherits `Claude` from default.
    #[test]
    fn agent_for_per_phase_resolves_override_and_default() -> Result<()> {
        let src = r#"
[phase.default]
profile = "base"
agent.backend = "claude"

[phase.todo]
profile = "rust"
agent.backend = "pi"
agent.provider = "deepseek"
agent.model_id = "deepseek-v3"
"#;
        let cfg = LoomConfig::from_toml_str(src)?;

        let todo = cfg.agent_for(Phase::Todo).expect("agent_for todo");
        assert_eq!(todo.profile.as_str(), "rust");
        assert_eq!(todo.kind, AgentKind::Pi);
        assert_eq!(todo.provider.as_deref(), Some("deepseek"));
        assert_eq!(todo.model_id.as_deref(), Some("deepseek-v3"));
        assert!(todo.claude_settings.is_none());

        let run = cfg.agent_for(Phase::Run).expect("agent_for run");
        assert_eq!(run.profile.as_str(), "base");
        assert_eq!(run.kind, AgentKind::Claude);
        assert!(run.provider.is_none());
        let claude = run.claude_settings.expect("claude_settings");
        assert_eq!(claude.post_result_grace_secs, 5);
        assert!(claude.denied_tools.is_empty());

        Ok(())
    }

    /// `[phase.<name>] agent.thinking_level = "high"` resolves into a typed
    /// [`ThinkingLevel`] on the resolved [`AgentSelection`]. The fallback
    /// chain walks `[phase.<name>]` → `[phase.default]` like every other
    /// field — assert both legs.
    #[test]
    fn agent_for_resolves_thinking_level_through_named_and_default_phases() -> Result<()> {
        use crate::agent::ThinkingLevel;

        let src = r#"
[phase.default]
agent.backend = "pi"
agent.thinking_level = "medium"

[phase.todo]
agent.backend = "pi"
agent.thinking_level = "high"
"#;
        let cfg = LoomConfig::from_toml_str(src)?;
        let todo = cfg.agent_for(Phase::Todo).expect("agent_for todo");
        assert_eq!(todo.thinking_level, Some(ThinkingLevel::High));

        let run = cfg.agent_for(Phase::Run).expect("agent_for run");
        assert_eq!(run.thinking_level, Some(ThinkingLevel::Medium));
        Ok(())
    }

    /// Typos in `agent.thinking_level` surface lazily as
    /// `UnknownThinkingLevel` from `agent_for`, mirroring the
    /// `UnknownBackend` error path so misconfigurations are caught at
    /// resolve time with a precise message.
    #[test]
    fn agent_for_unknown_thinking_level_surfaces_typed_error() -> Result<()> {
        let src = r#"
[phase.default]
agent.backend = "pi"
agent.thinking_level = "ultra"
"#;
        let cfg = LoomConfig::from_toml_str(src)?;
        match cfg.agent_for(Phase::Run) {
            Err(AgentSelectionError::UnknownThinkingLevel { name }) => {
                assert_eq!(name, "ultra");
            }
            other => panic!("expected UnknownThinkingLevel, got {other:?}"),
        }
        Ok(())
    }

    /// Empty config (no `[phase]` tables at all) resolves every phase to
    /// `claude` with the built-in `base` profile — the documented defaults.
    #[test]
    fn agent_for_default_is_claude_when_config_empty() -> Result<()> {
        let cfg = LoomConfig::default();
        for phase in [
            Phase::Plan,
            Phase::Todo,
            Phase::Run,
            Phase::Check,
            Phase::Review,
            Phase::Msg,
        ] {
            let sel = cfg.agent_for(phase).expect("agent_for");
            assert_eq!(sel.kind, AgentKind::Claude, "phase={phase:?}");
            assert_eq!(sel.profile.as_str(), BUILT_IN_PROFILE, "phase={phase:?}");
            assert!(sel.claude_settings.is_some());
        }
        Ok(())
    }

    /// `[phase.default]` without `agent.backend` still resolves to the
    /// built-in `claude` backend; the per-field fallback chain reaches
    /// past the partially-populated default into the built-in.
    #[test]
    fn agent_for_falls_through_partial_default_to_built_in() -> Result<()> {
        let src = r#"
[phase.default]
profile = "base"
"#;
        let cfg = LoomConfig::from_toml_str(src)?;
        let sel = cfg.agent_for(Phase::Run).expect("agent_for");
        assert_eq!(sel.kind, AgentKind::Claude);
        assert_eq!(sel.profile.as_str(), "base");
        Ok(())
    }

    /// Unknown backend name in TOML surfaces as `UnknownBackend` — not a
    /// parse error — so the message is precise about the offending value.
    #[test]
    fn agent_for_unknown_backend_in_default_returns_error() -> Result<()> {
        let src = r#"
[phase.default]
agent.backend = "gpt"
"#;
        let cfg = LoomConfig::from_toml_str(src)?;
        match cfg.agent_for(Phase::Run) {
            Err(AgentSelectionError::UnknownBackend { name }) => assert_eq!(name, "gpt"),
            other => panic!("expected UnknownBackend, got {other:?}"),
        }
        Ok(())
    }

    /// Unknown backend in a per-phase override surfaces only when that phase
    /// is queried — other phases still resolve.
    #[test]
    fn agent_for_unknown_backend_in_phase_override_isolated_to_that_phase() -> Result<()> {
        let src = r#"
[phase.default]
agent.backend = "claude"

[phase.todo]
agent.backend = "ollama"
"#;
        let cfg = LoomConfig::from_toml_str(src)?;
        match cfg.agent_for(Phase::Todo) {
            Err(AgentSelectionError::UnknownBackend { name }) => assert_eq!(name, "ollama"),
            other => panic!("expected UnknownBackend, got {other:?}"),
        }
        // Other phases unaffected.
        let run = cfg.agent_for(Phase::Run).expect("run unaffected");
        assert_eq!(run.kind, AgentKind::Claude);
        Ok(())
    }

    /// Claude-specific settings (`[claude]` + `[security]`) flow through
    /// `agent_for` when the resolved backend is claude.
    #[test]
    fn agent_for_threads_claude_specific_settings_when_kind_is_claude() -> Result<()> {
        let src = r#"
[claude]
post_result_grace_secs = 12

[security]
denied_tools = ["WebFetch", "Other"]
"#;
        let cfg = LoomConfig::from_toml_str(src)?;
        let sel = cfg.agent_for(Phase::Run).expect("agent_for");
        let claude = sel.claude_settings.expect("claude_settings present");
        assert_eq!(claude.post_result_grace_secs, 12);
        assert_eq!(claude.denied_tools, vec!["WebFetch", "Other"]);
        Ok(())
    }

    /// A per-phase `profile` override wins over `[phase.default].profile`.
    #[test]
    fn agent_for_resolves_profile_per_phase() -> Result<()> {
        let src = r#"
[phase.default]
profile = "base"

[phase.todo]
profile = "rust"
"#;
        let cfg = LoomConfig::from_toml_str(src)?;
        assert_eq!(
            cfg.agent_for(Phase::Todo).expect("todo").profile.as_str(),
            "rust"
        );
        assert_eq!(
            cfg.agent_for(Phase::Run).expect("run").profile.as_str(),
            "base"
        );
        Ok(())
    }
}

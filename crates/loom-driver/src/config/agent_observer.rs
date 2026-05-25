//! `[agent]` TOML block — observer-composition knobs surfaced through
//! `LoomConfig`. The shape mirrors `llm`'s `DoomLoopConfig` /
//! `DuplicateResultConfig` so the workflow can construct each observer
//! directly from the corresponding sub-block. The defaults match
//! `specs/llm.md` § Configuration.
//!
//! `loom-driver` cannot import `llm` (the dep graph in
//! `specs/harness.md` § Dependency Graph forbids it), so the
//! configs are defined here as plain serde shapes; the workflow layer
//! consumes them when composing the observer chain.

use serde::Deserialize;

/// `[agent]` table aggregating the per-observer sub-blocks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AgentObserversConfig {
    pub doom_loop: DoomLoopConfig,
    pub duplicate_result: DuplicateResultConfig,
}

/// `[agent.doom_loop]` — sliding-window detection knobs for the
/// doom-loop observer. Mirrors `loom_llm::observer::DoomLoopConfig`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct DoomLoopConfig {
    pub enabled: bool,
    pub window: u32,
    pub threshold: u32,
    pub stage_2_after_stage_1: u32,
}

impl Default for DoomLoopConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            window: 5,
            threshold: 3,
            stage_2_after_stage_1: 3,
        }
    }
}

/// `[agent.duplicate_result]` — duplicate-detection knobs for the
/// duplicate-result observer. Mirrors
/// `loom_llm::observer::DuplicateResultConfig`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct DuplicateResultConfig {
    pub enabled: bool,
    pub min_bytes: u32,
}

impl Default for DuplicateResultConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_bytes: 256,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec_configuration_block() {
        let cfg = AgentObserversConfig::default();
        assert!(cfg.doom_loop.enabled);
        assert_eq!(cfg.doom_loop.window, 5);
        assert_eq!(cfg.doom_loop.threshold, 3);
        assert_eq!(cfg.doom_loop.stage_2_after_stage_1, 3);
        assert!(cfg.duplicate_result.enabled);
        assert_eq!(cfg.duplicate_result.min_bytes, 256);
    }

    #[test]
    fn deserializes_spec_example_toml() {
        let src = r#"
[doom_loop]
enabled = true
window = 5
threshold = 3
stage_2_after_stage_1 = 3

[duplicate_result]
enabled = true
min_bytes = 256
"#;
        let cfg: AgentObserversConfig = toml::from_str(src).expect("parse");
        assert_eq!(cfg, AgentObserversConfig::default());
    }

    #[test]
    fn missing_subblock_falls_back_to_default() {
        let src = "";
        let cfg: AgentObserversConfig = toml::from_str(src).expect("parse");
        assert_eq!(cfg, AgentObserversConfig::default());
    }

    #[test]
    fn enabled_false_is_respected() {
        let src = r#"
[doom_loop]
enabled = false

[duplicate_result]
enabled = false
"#;
        let cfg: AgentObserversConfig = toml::from_str(src).expect("parse");
        assert!(!cfg.doom_loop.enabled);
        assert!(!cfg.duplicate_result.enabled);
    }
}

//! Raw phase-table syntax resolves once to closed workflow keys.
use super::{Phase, PhaseConfig};
use serde::{Deserialize, Deserializer};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PhaseKey {
    Default,
    Named(Phase),
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawPhases {
    default: Option<PhaseConfig>,
    plan: Option<PhaseConfig>,
    todo: Option<PhaseConfig>,
    #[serde(rename = "loop")]
    loop_: Option<PhaseConfig>,
    inbox: Option<PhaseConfig>,
    #[serde(rename = "gate.review")]
    review: Option<PhaseConfig>,
    gate: Option<RawGate>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGate {
    review: PhaseConfig,
}

pub(super) fn deserialize<'de, D: Deserializer<'de>>(
    de: D,
) -> Result<BTreeMap<PhaseKey, PhaseConfig>, D::Error> {
    let raw = RawPhases::deserialize(de)?;
    if raw.review.is_some() && raw.gate.is_some() {
        return Err(serde::de::Error::custom(
            "phase.gate.review is declared both as a dotted key and a nested table",
        ));
    }
    Ok([
        (PhaseKey::Default, raw.default),
        (PhaseKey::Named(Phase::Plan), raw.plan),
        (PhaseKey::Named(Phase::Todo), raw.todo),
        (PhaseKey::Named(Phase::Loop), raw.loop_),
        (PhaseKey::Named(Phase::Inbox), raw.inbox),
        (
            PhaseKey::Named(Phase::Review),
            raw.review.or_else(|| raw.gate.map(|gate| gate.review)),
        ),
    ]
    .into_iter()
    .filter_map(|(key, value)| value.map(|value| (key, value)))
    .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agent::{AgentKind, ThinkingLevel},
        config::LoomConfig,
    };

    #[test]
    fn direct_serde_rejects_phase_typos_and_invalid_known_values() {
        for json in [
            r#"{"phase":{"looop":{}}}"#,
            r#"{"phase":{"gate":{"revieew":{}}}}"#,
            r#"{"phase":{"loop":{"profile":"Not Valid"}}}"#,
            r#"{"phase":{"loop":{"agent":{"backend":"typo"}}}}"#,
            r#"{"phase":{"loop":{"agent":{"thinking_level":"ultra"}}}}"#,
            r#"{"phase":{"loop":{"agent":{"provider":""}}}}"#,
            r#"{"phase":{"loop":{"agent":{"model_id":" "}}}}"#,
        ] {
            assert!(serde_json::from_str::<LoomConfig>(json).is_err(), "{json}");
        }
    }

    #[test]
    fn nested_and_literal_review_tables_have_identical_typed_fallback() {
        let nested = LoomConfig::from_toml_str("[phase.default]\nprofile='rust'\nagent.backend='pi'\n[phase.gate.review]\nagent.thinking_level='high'\n").unwrap();
        let literal = LoomConfig::from_toml_str("[phase.default]\nprofile='rust'\nagent.backend='pi'\n[phase.'gate.review']\nagent.thinking_level='high'\n").unwrap();
        assert_eq!(nested, literal);
        let selection = nested.agent_for(Phase::Review);
        assert_eq!(selection.kind(), AgentKind::Pi);
        assert_eq!(selection.profile.as_str(), "rust");
        assert_eq!(selection.thinking_level, Some(ThinkingLevel::High));
        assert!(LoomConfig::from_toml_str("[phase.gate.review]\n[phase.'gate.review']\n").is_err());
    }
}

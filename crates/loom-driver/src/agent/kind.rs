use std::fmt;
use std::str::FromStr;

use displaydoc::Display;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Closed-set runtime selected for an agent-backed phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentRuntime {
    Pi,
    Claude,
    Direct,
}

impl AgentRuntime {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pi => "pi",
            Self::Claude => "claude",
            Self::Direct => "direct",
        }
    }
}

impl fmt::Display for AgentRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentRuntime {
    type Err = ParseAgentRuntimeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pi" => Ok(Self::Pi),
            "claude" => Ok(Self::Claude),
            "direct" => Ok(Self::Direct),
            other => Err(ParseAgentRuntimeError {
                name: other.to_string(),
            }),
        }
    }
}

/// unknown agent runtime `{name}` (expected `pi`, `claude`, or `direct`)
#[derive(Debug, Display, Error, PartialEq, Eq)]
pub struct ParseAgentRuntimeError {
    pub name: String,
}

pub type AgentKind = AgentRuntime;

#[cfg(test)]
mod tests {
    use super::AgentRuntime;
    use anyhow::Result;

    #[test]
    fn agent_runtime_parse_serde_rejects_unknown_values() -> Result<()> {
        for (runtime, expected) in runtime_wire_values() {
            assert_eq!(runtime.as_str(), expected);
            assert_eq!(runtime.to_string(), expected);
            assert_eq!(serde_json::to_string(&runtime)?, format!("\"{expected}\""));
            let parsed: AgentRuntime = expected.parse()?;
            assert_eq!(parsed, runtime);
            let back: AgentRuntime = serde_json::from_str(&format!("\"{expected}\""))?;
            assert_eq!(back, runtime);
        }

        let parse_err = "gpt".parse::<AgentRuntime>().expect_err("unknown runtime");
        assert_eq!(parse_err.name, "gpt");
        let serde_err = serde_json::from_str::<AgentRuntime>("\"gpt\"").unwrap_err();
        assert!(
            serde_err.to_string().contains("unknown variant"),
            "{serde_err}"
        );
        Ok(())
    }

    #[test]
    fn agent_runtime_name_maps_to_wrix_agent_values() {
        for (runtime, expected) in runtime_wire_values() {
            assert_eq!(runtime.as_str(), expected);
        }
    }

    fn runtime_wire_values() -> [(AgentRuntime, &'static str); 3] {
        [
            (AgentRuntime::Pi, "pi"),
            (AgentRuntime::Claude, "claude"),
            (AgentRuntime::Direct, "direct"),
        ]
    }
}

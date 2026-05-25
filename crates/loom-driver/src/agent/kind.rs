use serde::{Deserialize, Serialize};

/// Selector for which agent backend should drive a phase.
///
/// Per spec NF-7 this is an enum, not a newtype: the variants are a closed
/// set known at compile time and dispatch is via `match`, not parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Pi,
    Claude,
}

#[cfg(test)]
mod tests {
    use super::AgentKind;
    use anyhow::Result;

    #[test]
    fn serde_uses_lowercase_variant_names() -> Result<()> {
        assert_eq!(serde_json::to_string(&AgentKind::Pi)?, "\"pi\"");
        assert_eq!(serde_json::to_string(&AgentKind::Claude)?, "\"claude\"");
        let back: AgentKind = serde_json::from_str("\"claude\"")?;
        assert_eq!(back, AgentKind::Claude);
        Ok(())
    }
}

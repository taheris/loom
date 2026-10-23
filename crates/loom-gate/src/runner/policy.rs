//! Skip permission is compiled configuration, not a producer verdict or runner command.

use loom_driver::config::{LoomConfig, RunnerEntry, SkipPolicy};
use regex::Regex;

use super::RunnerError;
use super::result::{Token, permits_skip};
use crate::Tier;
use crate::dispatch::VerifierVerdict;

#[derive(Debug)]
struct Rule {
    tier: Tier,
    pattern: Option<Regex>,
    policy: SkipPolicy,
    capabilities: Vec<Token>,
}

/// Permission rules cannot execute commands, discover identities or grant coverage.
#[derive(Debug, Default)]
pub struct Policies {
    rules: Vec<Rule>,
}

impl Policies {
    /// Compile permission only; command, input-query and target templates are never retained.
    ///
    /// # Errors
    /// Rejects invalid matching expressions and capability tokens before execution.
    pub fn compile(config: &LoomConfig) -> Result<Self, RunnerError> {
        let mut rules = Vec::new();
        for tier in [Tier::Check, Tier::Test, Tier::System] {
            let Some(block) = config.runner.tier(tier.as_wire()) else {
                continue;
            };
            for (name, entry) in &block.runners {
                rules.push(Self::rule(tier, name, entry)?);
            }
            if let Some(entry) = block.default_runner() {
                rules.push(Self::rule(tier, "default", &entry)?);
            }
        }
        Ok(Self { rules })
    }

    fn rule(tier: Tier, name: &str, entry: &RunnerEntry) -> Result<Rule, RunnerError> {
        let pattern = entry
            .match_regex
            .as_ref()
            .map(|pattern| {
                Regex::new(pattern).map_err(|source| RunnerError::InvalidMatch {
                    name: name.to_owned(),
                    pattern: pattern.clone(),
                    source,
                })
            })
            .transpose()?;
        let capabilities = entry
            .skip_capabilities
            .iter()
            .cloned()
            .map(Token::try_from)
            .collect::<Result<_, _>>()
            .map_err(|detail| RunnerError::InvalidOutput {
                parser: "skip-policy",
                detail,
            })?;
        Ok(Rule {
            tier,
            pattern,
            policy: entry.skip_policy,
            capabilities,
        })
    }

    /// Apply a rule only after the caller has resolved the exact checked identity.
    pub fn permits(&self, tier: Tier, target: &str, verdict: &VerifierVerdict) -> bool {
        verdict.skipped
            && verdict.producer_error.is_none()
            && self
                .rules
                .iter()
                .find(|rule| {
                    rule.tier == tier
                        && rule
                            .pattern
                            .as_ref()
                            .is_none_or(|pattern| pattern.is_match(target))
                })
                .is_some_and(|rule| {
                    permits_skip(
                        rule.policy,
                        &rule.capabilities,
                        verdict.execution.as_ref(),
                        verdict.skip_reason.as_ref(),
                    )
                })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Verdict;
    use crate::runner::result::{Execution, SkipReason, current_platform};

    #[test]
    fn permission_rules_do_not_need_or_retain_executable_commands() {
        let config = LoomConfig::from_toml_str("[runner.system.native]\nmatch='^nix report .*::probe$'\nskip_policy='sandbox-capability'\nskip_capabilities=['kvm']\n").unwrap();
        let policies = Policies::compile(&config).unwrap();
        let mut verdict = VerifierVerdict::from_outcome(Verdict::Skipped, "missing kvm".into());
        verdict.execution = Some(Execution {
            platform: Token::try_from(current_platform()).unwrap(),
            platforms: vec![],
            capabilities: vec![Token::try_from("kvm".to_owned()).unwrap()],
        });
        verdict.skip_reason = Some(SkipReason::MissingCapability {
            capability: Token::try_from("kvm".to_owned()).unwrap(),
            reason: "not available".into(),
        });
        assert!(policies.permits(Tier::System, "nix report .#checks.x.probe::probe", &verdict));
        assert!(!policies.permits(Tier::Check, "nix report .#checks.x.probe::probe", &verdict));
        assert!(!policies.permits(Tier::System, "unregistered-shaped-name", &verdict));
        verdict.producer_error = Some("contradictory status".into());
        assert!(!policies.permits(Tier::System, "nix report .#checks.x.probe::probe", &verdict));
    }
}

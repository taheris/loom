//! Checked verifier result records and sandbox-capability policy.

use loom_driver::config::SkipPolicy;
use serde::{Deserialize, Serialize};

use crate::cache::Verdict;

/// Nonempty platform/capability token from an open producer vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Token(String);

impl TryFrom<String> for Token {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty()
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(format!("invalid platform/capability token `{value}`"));
        }
        Ok(Self(value))
    }
}

impl From<Token> for String {
    fn from(value: Token) -> Self {
        value.0
    }
}

/// Execution platform and declared target requirements, not proof of execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Execution {
    pub platform: Token,
    pub platforms: Vec<Token>,
    pub capabilities: Vec<Token>,
}

/// Explicit prerequisite preventing execution; ordinary failures have no skip reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SkipReason {
    ForeignPlatform { reason: String },
    MissingCapability { capability: Token, reason: String },
}

impl SkipReason {
    const fn reason(&self) -> &str {
        match self {
            Self::ForeignPlatform { reason } | Self::MissingCapability { reason, .. } => {
                reason.as_str()
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Outcome {
    Passed,
    Failed,
    Skipped,
}

impl Outcome {
    const fn verdict(self) -> Verdict {
        match self {
            Self::Passed => Verdict::Pass,
            Self::Failed => Verdict::Fail,
            Self::Skipped => Verdict::Skipped,
        }
    }
}

/// Legacy flags remain accepted, but cannot conflict with an explicit outcome.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Wire {
    pub target: String,
    #[serde(default, deserialize_with = "present_value")]
    outcome: Option<Outcome>,
    #[serde(default, deserialize_with = "present_value")]
    pass: Option<bool>,
    #[serde(default, deserialize_with = "present_value")]
    skipped: Option<bool>,
    evidence: String,
    #[serde(default, deserialize_with = "present_value")]
    execution: Option<Execution>,
    #[serde(default, deserialize_with = "present_value")]
    skip_reason: Option<SkipReason>,
}

fn present_value<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

impl Wire {
    pub(super) fn resolve(self) -> Result<super::ParsedVerdict, String> {
        if self.target.trim().is_empty() || self.target.chars().any(char::is_control) {
            return Err("target must be nonblank and contain no control characters".into());
        }
        let legacy = self.pass.map(|pass| {
            if self.skipped == Some(true) {
                Verdict::Skipped
            } else if pass {
                Verdict::Pass
            } else {
                Verdict::Fail
            }
        });
        let outcome = self
            .outcome
            .map(Outcome::verdict)
            .or(legacy)
            .ok_or_else(|| "expected outcome or legacy pass".to_string())?;
        if self
            .pass
            .is_some_and(|pass| pass != (outcome == Verdict::Pass))
            || self
                .skipped
                .is_some_and(|skipped| skipped != (outcome == Verdict::Skipped))
        {
            return Err("contradictory outcome/pass/skipped fields".into());
        }
        if self.outcome.is_some() && self.execution.is_none() {
            return Err("explicit outcomes require execution metadata".into());
        }
        if self.skip_reason.is_some() && outcome != Verdict::Skipped {
            return Err("only skipped results may carry skip_reason".into());
        }
        if self.outcome.is_some() && outcome == Verdict::Skipped && self.skip_reason.is_none() {
            return Err("skipped outcomes require skip_reason".into());
        }
        if self
            .skip_reason
            .as_ref()
            .is_some_and(|reason| reason.reason().trim().is_empty())
        {
            return Err("skip reason must be nonblank".into());
        }
        if let Some(execution) = &self.execution {
            for tokens in [&execution.platforms, &execution.capabilities] {
                let unique = tokens
                    .iter()
                    .map(|token| &token.0)
                    .collect::<std::collections::HashSet<_>>();
                if unique.len() != tokens.len() {
                    return Err("duplicate execution requirement".into());
                }
            }
        }
        Ok(super::ParsedVerdict {
            target: self.target,
            outcome,
            evidence: self.evidence,
            execution: self.execution,
            skip_reason: self.skip_reason,
        })
    }
}

/// Canonical Nix-style platform of the verifier process Loom actually launched.
pub fn current_platform() -> String {
    let os = if std::env::consts::OS == "macos" {
        "darwin"
    } else {
        std::env::consts::OS
    };
    format!("{}-{os}", std::env::consts::ARCH)
}

/// Policy permits only a declared foreign platform or an allowlisted missing capability.
pub fn permits_skip(
    policy: SkipPolicy,
    allowed_capabilities: &[Token],
    execution: Option<&Execution>,
    reason: Option<&SkipReason>,
) -> bool {
    if policy != SkipPolicy::SandboxCapability {
        return false;
    }
    let Some(execution) = execution.filter(|execution| execution.platform.0 == current_platform())
    else {
        return false;
    };
    let applicable =
        execution.platforms.is_empty() || execution.platforms.contains(&execution.platform);
    match reason {
        Some(SkipReason::ForeignPlatform { .. }) => !applicable,
        Some(SkipReason::MissingCapability { capability, .. }) => {
            applicable
                && execution.capabilities.contains(capability)
                && allowed_capabilities.contains(capability)
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(record: serde_json::Value) -> Result<super::super::ParsedVerdict, String> {
        serde_json::from_value::<Wire>(record)
            .map_err(|error| error.to_string())?
            .resolve()
    }

    #[test]
    fn legacy_false_with_skipped_evidence_is_failure() {
        let result =
            parse(serde_json::json!({"target":"a","pass":false,"evidence":"skipped missing KVM"}))
                .unwrap();
        assert_eq!(result.outcome, Verdict::Fail);
    }

    #[test]
    fn optional_result_fields_reject_present_null_values() {
        for field in ["outcome", "pass", "skipped", "execution", "skip_reason"] {
            let mut record = serde_json::json!({"target":"a","pass":true,"evidence":"ok"});
            record[field] = serde_json::Value::Null;
            assert!(
                parse(record).is_err(),
                "null field {field} must not become omission"
            );
        }
    }

    #[test]
    fn explicit_skipped_outcome_is_consistent_with_legacy_false_flag() {
        let record = serde_json::json!({"target":"a","outcome":"skipped","pass":false,"evidence":"not executed","execution":{"platform":current_platform(),"platforms":[],"capabilities":["kvm"]},"skip_reason":{"kind":"missing-capability","capability":"kvm","reason":"device unavailable"}});
        assert_eq!(parse(record).unwrap().outcome, Verdict::Skipped);
    }

    #[test]
    fn result_schema_rejects_conflicting_or_incomplete_outcomes() {
        for record in [
            serde_json::json!({"target":"a","pass":true,"skipped":true,"evidence":"not run"}),
            serde_json::json!({"target":"a","outcome":"passed","evidence":"ok"}),
            serde_json::json!({"target":"a","pass":false,"evidence":"bad","skip_reason":{"kind":"foreign-platform","reason":"linux"}}),
            serde_json::json!({"target":"a","pass":"yes","evidence":"bad"}),
        ] {
            assert!(parse(record.clone()).is_err(), "{record}");
        }
    }

    #[test]
    fn sandbox_policy_permits_only_declared_platform_or_capability_gaps() {
        let platform = Token::try_from(current_platform()).unwrap();
        let kvm = Token::try_from("kvm".to_string()).unwrap();
        let mut execution = Execution {
            platform,
            platforms: vec![Token::try_from("foreign-platform".to_string()).unwrap()],
            capabilities: vec![kvm.clone()],
        };
        let foreign = SkipReason::ForeignPlatform {
            reason: "not applicable".into(),
        };
        let missing = SkipReason::MissingCapability {
            capability: kvm.clone(),
            reason: "no device".into(),
        };
        assert!(permits_skip(
            SkipPolicy::SandboxCapability,
            &[],
            Some(&execution),
            Some(&foreign)
        ));
        assert!(!permits_skip(
            SkipPolicy::Deny,
            std::slice::from_ref(&kvm),
            Some(&execution),
            Some(&foreign)
        ));
        assert!(!permits_skip(
            SkipPolicy::SandboxCapability,
            std::slice::from_ref(&kvm),
            Some(&execution),
            Some(&missing)
        ));
        execution.platforms.clear();
        assert!(!permits_skip(
            SkipPolicy::SandboxCapability,
            &[],
            Some(&execution),
            Some(&missing)
        ));
        assert!(permits_skip(
            SkipPolicy::SandboxCapability,
            &[kvm],
            Some(&execution),
            Some(&missing)
        ));
        assert!(!permits_skip(
            SkipPolicy::SandboxCapability,
            &[],
            Some(&execution),
            Some(&foreign)
        ));
    }

    proptest::proptest! {
        #![proptest_config(loom_test_support::proptest_config())]
        #[test]
        fn json_result_parser_never_panics_on_arbitrary_input(input in ".{0,2048}") {
            let _result = super::super::parse_json_lines(&input);
        }

        #[test]
        fn json_result_records_preserve_generated_identity_and_evidence(target in "[a-z][a-z0-9_-]{0,32}", evidence in ".{0,512}") {
            let record = serde_json::json!({"target":target,"outcome":"passed","evidence":evidence,"execution":{"platform":current_platform(),"platforms":[],"capabilities":[]}});
            let parsed = parse(record).unwrap();
            proptest::prop_assert_eq!(parsed.target, target);
            proptest::prop_assert_eq!(parsed.evidence, evidence);
            proptest::prop_assert_eq!(parsed.outcome, Verdict::Pass);
        }
    }
}

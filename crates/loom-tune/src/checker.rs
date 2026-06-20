use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use displaydoc::Display;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::target;

/// Internal checker id in `kind.domain.name` form.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CheckerId {
    value: String,
    kind: Kind,
    domain: Domain,
}

impl CheckerId {
    pub fn new(value: impl Into<String>) -> Result<Self, ParseCheckerIdError> {
        value.into().parse()
    }

    pub fn as_str(&self) -> &str {
        &self.value
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn domain(&self) -> Domain {
        self.domain
    }
}

impl fmt::Display for CheckerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.value)
    }
}

impl PartialOrd for CheckerId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CheckerId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.value.cmp(&other.value)
    }
}

impl FromStr for CheckerId {
    type Err = ParseCheckerIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut parts = value.split('.');
        let kind_raw = parts
            .next()
            .ok_or_else(|| ParseCheckerIdError::InvalidShape {
                value: value.to_owned(),
            })?;
        let domain_raw = parts
            .next()
            .ok_or_else(|| ParseCheckerIdError::InvalidShape {
                value: value.to_owned(),
            })?;
        let name = parts
            .next()
            .ok_or_else(|| ParseCheckerIdError::InvalidShape {
                value: value.to_owned(),
            })?;
        if parts.next().is_some() {
            return Err(ParseCheckerIdError::InvalidShape {
                value: value.to_owned(),
            });
        }
        for segment in [kind_raw, domain_raw, name] {
            if !valid_segment(segment) {
                return Err(ParseCheckerIdError::InvalidSegment {
                    value: value.to_owned(),
                    segment: segment.to_owned(),
                });
            }
        }
        let kind =
            Kind::from_segment(kind_raw).ok_or_else(|| ParseCheckerIdError::UnknownKind {
                value: value.to_owned(),
                kind: kind_raw.to_owned(),
            })?;
        let domain =
            Domain::from_segment(domain_raw).ok_or_else(|| ParseCheckerIdError::UnknownDomain {
                value: value.to_owned(),
                domain: domain_raw.to_owned(),
            })?;
        Ok(Self {
            value: value.to_owned(),
            kind,
            domain,
        })
    }
}

impl Serialize for CheckerId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.value)
    }
}

impl<'de> Deserialize<'de> for CheckerId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

fn valid_segment(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && !value.ends_with('-')
        && !value.contains("--")
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Checker id parse failures.
#[derive(Debug, Clone, PartialEq, Eq, Display, Error)]
pub enum ParseCheckerIdError {
    /// invalid checker id `{value}`: expected exactly three dotted segments
    InvalidShape { value: String },
    /// invalid checker id `{value}` segment `{segment}`: expected lowercase kebab-case
    InvalidSegment { value: String, segment: String },
    /// invalid checker id `{value}` kind `{kind}`: expected `preflight` or `behavior`
    UnknownKind { value: String, kind: String },
    /// invalid checker id `{value}` domain `{domain}` is not in the v1 domain set
    UnknownDomain { value: String, domain: String },
}

/// Top-level checker class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Preflight,
    Behavior,
}

pub type CheckerKind = Kind;

impl Kind {
    fn from_segment(value: &str) -> Option<Self> {
        match value {
            "preflight" => Some(Self::Preflight),
            "behavior" => Some(Self::Behavior),
            _ => None,
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Preflight => f.write_str("preflight"),
            Self::Behavior => f.write_str("behavior"),
        }
    }
}

/// V1 checker domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    Skill,
    Template,
    Review,
    Todo,
    Loop,
    Inbox,
    Tune,
    Agent,
    Gate,
}

impl Domain {
    fn from_segment(value: &str) -> Option<Self> {
        match value {
            "skill" => Some(Self::Skill),
            "template" => Some(Self::Template),
            "review" => Some(Self::Review),
            "todo" => Some(Self::Todo),
            "loop" => Some(Self::Loop),
            "inbox" => Some(Self::Inbox),
            "tune" => Some(Self::Tune),
            "agent" => Some(Self::Agent),
            "gate" => Some(Self::Gate),
            _ => None,
        }
    }
}

/// Checker lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Active,
    Retired,
}

/// Tune run level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Fast,
    Run,
    Full,
}

/// Declared case role.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum CaseRole {
    #[default]
    Regression,
}

/// Built-in checker execution cost class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Cost {
    Static,
    AgentReplay,
}

/// Checker-specific case schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseSchema {
    ReviewFindingRecall,
    TodoDecomposition,
    LoopVerifyAfterEdit,
    LoopScopeDiscipline,
    InboxResolutionPath,
    TuneApplyHandoff,
    AgentContextBeforeEdit,
}

/// Machine-readable checker metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub id: CheckerId,
    pub title: String,
    pub summary: String,
    pub status: Status,
    pub target_kinds: Vec<target::Kind>,
    pub levels: Vec<Level>,
    pub cost: Cost,
    pub mandatory: bool,
    pub case_roles: Vec<CaseRole>,
    pub implementation: String,
    pub soft_regression_epsilon: String,
    pub schema: Option<CaseSchema>,
    pub retirement: Option<String>,
}

impl Metadata {
    pub fn supports_level(&self, level: Level) -> bool {
        self.levels.contains(&level)
    }

    pub fn supports_role(&self, role: CaseRole) -> bool {
        self.case_roles.contains(&role)
    }
}

/// Internal checker registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    checkers: BTreeMap<CheckerId, Metadata>,
}

impl Registry {
    pub fn builtin() -> Result<Self, RegistryError> {
        Self::new(
            builtin_descriptors()
                .into_iter()
                .map(MetadataDescriptor::into_metadata),
        )
    }

    pub fn new(
        metadata: impl IntoIterator<Item = Result<Metadata, RegistryError>>,
    ) -> Result<Self, RegistryError> {
        let mut checkers = BTreeMap::new();
        for result in metadata {
            let metadata = result?;
            let id = metadata.id.clone();
            if checkers.insert(id.clone(), metadata).is_some() {
                return Err(RegistryError::Duplicate { id });
            }
        }
        Ok(Self { checkers })
    }

    pub fn get(&self, id: &CheckerId) -> Option<&Metadata> {
        self.checkers.get(id)
    }

    pub fn active(&self) -> impl Iterator<Item = &Metadata> {
        self.checkers
            .values()
            .filter(|metadata| metadata.status == Status::Active)
    }

    pub fn metadata_snapshot(&self) -> Vec<Metadata> {
        self.checkers.values().cloned().collect()
    }

    pub fn require_active(&self, id: &CheckerId) -> Result<&Metadata, RegistryError> {
        let metadata = self
            .get(id)
            .ok_or_else(|| RegistryError::Unknown { id: id.clone() })?;
        if metadata.status == Status::Retired {
            return Err(RegistryError::Retired {
                id: id.clone(),
                guidance: metadata.retirement.clone().unwrap_or_default(),
            });
        }
        Ok(metadata)
    }

    pub fn require_case_checker(
        &self,
        id: &CheckerId,
        disabled: &BTreeSet<CheckerId>,
    ) -> Result<&Metadata, RegistryError> {
        let metadata = self.require_active(id)?;
        if metadata.id.kind() != Kind::Behavior {
            return Err(RegistryError::NotBehaviorCase {
                id: id.clone(),
                kind: metadata.id.kind(),
            });
        }
        if disabled.contains(id) {
            return Err(RegistryError::Disabled { id: id.clone() });
        }
        Ok(metadata)
    }

    pub fn validate_disabled(
        &self,
        disabled: &[CheckerId],
    ) -> Result<BTreeSet<CheckerId>, RegistryError> {
        let mut validated = BTreeSet::new();
        for id in disabled {
            let metadata = self.require_active(id)?;
            if metadata.mandatory {
                return Err(RegistryError::MandatoryDisabled { id: id.clone() });
            }
            validated.insert(id.clone());
        }
        Ok(validated)
    }
}

/// Checker registry failures.
#[derive(Debug, Clone, PartialEq, Eq, Display, Error)]
pub enum RegistryError {
    /// checker `{id}` is not registered
    Unknown { id: CheckerId },
    /// checker `{id}` is retired: {guidance}
    Retired { id: CheckerId, guidance: String },
    /// checker `{id}` is mandatory and cannot be disabled
    MandatoryDisabled { id: CheckerId },
    /// checker `{id}` cannot be used in `loom-case` because it is `{kind}`
    NotBehaviorCase { id: CheckerId, kind: Kind },
    /// checker `{id}` is disabled by tune config
    Disabled { id: CheckerId },
    /// duplicate checker id `{id}` in registry
    Duplicate { id: CheckerId },
    /// built-in checker id `{id}` is invalid
    InvalidBuiltinId {
        id: String,
        #[source]
        source: ParseCheckerIdError,
    },
}

#[derive(Debug, Clone, Copy)]
struct MetadataDescriptor {
    id: &'static str,
    title: &'static str,
    summary: &'static str,
    status: Status,
    target_kinds: &'static [target::Kind],
    levels: &'static [Level],
    cost: Cost,
    mandatory: bool,
    case_roles: &'static [CaseRole],
    implementation: &'static str,
    soft_regression_epsilon: &'static str,
    schema: Option<CaseSchema>,
    retirement: Option<&'static str>,
}

impl MetadataDescriptor {
    fn into_metadata(self) -> Result<Metadata, RegistryError> {
        Ok(Metadata {
            id: CheckerId::new(self.id).map_err(|source| RegistryError::InvalidBuiltinId {
                id: self.id.to_owned(),
                source,
            })?,
            title: self.title.to_owned(),
            summary: self.summary.to_owned(),
            status: self.status,
            target_kinds: self.target_kinds.to_vec(),
            levels: self.levels.to_vec(),
            cost: self.cost,
            mandatory: self.mandatory,
            case_roles: self.case_roles.to_vec(),
            implementation: self.implementation.to_owned(),
            soft_regression_epsilon: self.soft_regression_epsilon.to_owned(),
            schema: self.schema,
            retirement: self.retirement.map(ToOwned::to_owned),
        })
    }
}

const ALL_LEVELS: &[Level] = &[Level::Fast, Level::Run, Level::Full];
const BEHAVIOR_LEVELS: &[Level] = &[Level::Run, Level::Full];
const REGRESSION: &[CaseRole] = &[CaseRole::Regression];
const SKILL_TARGET: &[target::Kind] = &[target::Kind::Skill];
const TEMPLATE_TARGET: &[target::Kind] = &[target::Kind::Phase, target::Kind::Partial];
const SKILL_PHASE_TARGET: &[target::Kind] = &[target::Kind::Skill, target::Kind::Phase];

fn builtin_descriptors() -> Vec<MetadataDescriptor> {
    vec![
        preflight(
            "preflight.skill.registry",
            "Skill registry legality",
            "Validates skill parsing, frontmatter, names, duplicates, and overrides.",
            SKILL_TARGET,
            "skill_registry",
        ),
        preflight(
            "preflight.skill.materialization",
            "Skill materialization legality",
            "Validates materialized skill paths and backend registration inputs.",
            SKILL_TARGET,
            "skill_materialization",
        ),
        preflight(
            "preflight.skill.protocol-boundary",
            "Skill protocol boundary",
            "Ensures skills cannot weaken compiled phase protocol or gate contracts.",
            SKILL_TARGET,
            "skill_protocol_boundary",
        ),
        preflight(
            "preflight.template.compile",
            "Template compilation",
            "Compiles candidate phase and partial templates against typed Askama contexts.",
            TEMPLATE_TARGET,
            "template_compile",
        ),
        preflight(
            "preflight.template.conformance",
            "Template conformance",
            "Validates include graph, marker ownership, and protocol surfaces.",
            TEMPLATE_TARGET,
            "template_conformance",
        ),
        preflight(
            "preflight.tune.case-validation",
            "Tuning case validation",
            "Validates tuning documents and declared loom-case blocks.",
            &[
                target::Kind::Skill,
                target::Kind::Phase,
                target::Kind::Partial,
            ],
            "tune_case_validation",
        ),
        behavior(
            "behavior.review.finding-recall",
            "Review finding recall",
            "Runs review on a known diff and scores expected findings.",
            SKILL_PHASE_TARGET,
            "review_finding_recall",
            CaseSchema::ReviewFindingRecall,
        ),
        behavior(
            "behavior.todo.decomposition",
            "Todo decomposition",
            "Runs todo decomposition and scores parseable, scoped LOOM_TODO output.",
            SKILL_PHASE_TARGET,
            "todo_decomposition",
            CaseSchema::TodoDecomposition,
        ),
        behavior(
            "behavior.loop.verify-after-edit",
            "Loop verify after edit",
            "Verifies relevant commands ran after final relevant edits.",
            SKILL_PHASE_TARGET,
            "loop_verify_after_edit",
            CaseSchema::LoopVerifyAfterEdit,
        ),
        behavior(
            "behavior.loop.scope-discipline",
            "Loop scope discipline",
            "Scores solving the requested task without unrelated edits.",
            SKILL_PHASE_TARGET,
            "loop_scope_discipline",
            CaseSchema::LoopScopeDiscipline,
        ),
        behavior(
            "behavior.inbox.resolution-path",
            "Inbox resolution path",
            "Scores chat-based resolution without removed host-side mutation commands.",
            SKILL_PHASE_TARGET,
            "inbox_resolution_path",
            CaseSchema::InboxResolutionPath,
        ),
        behavior(
            "behavior.tune.apply-handoff",
            "Tune apply handoff",
            "Scores LOOM_APPLY handoff without chat-side push or integration mutation.",
            SKILL_PHASE_TARGET,
            "tune_apply_handoff",
            CaseSchema::TuneApplyHandoff,
        ),
        behavior(
            "behavior.agent.context-before-edit",
            "Agent context before edit",
            "Verifies required context was read before the first relevant edit.",
            SKILL_PHASE_TARGET,
            "agent_context_before_edit",
            CaseSchema::AgentContextBeforeEdit,
        ),
        MetadataDescriptor {
            id: "behavior.review.legacy-recall",
            title: "Legacy review recall",
            summary: "Retired review recall checker kept for migration diagnostics.",
            status: Status::Retired,
            target_kinds: SKILL_PHASE_TARGET,
            levels: BEHAVIOR_LEVELS,
            cost: Cost::AgentReplay,
            mandatory: false,
            case_roles: REGRESSION,
            implementation: "legacy_review_recall",
            soft_regression_epsilon: "0.01",
            schema: Some(CaseSchema::ReviewFindingRecall),
            retirement: Some("Use behavior.review.finding-recall."),
        },
    ]
}

fn preflight(
    id: &'static str,
    title: &'static str,
    summary: &'static str,
    target_kinds: &'static [target::Kind],
    implementation: &'static str,
) -> MetadataDescriptor {
    MetadataDescriptor {
        id,
        title,
        summary,
        status: Status::Active,
        target_kinds,
        levels: ALL_LEVELS,
        cost: Cost::Static,
        mandatory: true,
        case_roles: &[],
        implementation,
        soft_regression_epsilon: "0.01",
        schema: None,
        retirement: None,
    }
}

fn behavior(
    id: &'static str,
    title: &'static str,
    summary: &'static str,
    target_kinds: &'static [target::Kind],
    implementation: &'static str,
    schema: CaseSchema,
) -> MetadataDescriptor {
    MetadataDescriptor {
        id,
        title,
        summary,
        status: Status::Active,
        target_kinds,
        levels: BEHAVIOR_LEVELS,
        cost: Cost::AgentReplay,
        mandatory: false,
        case_roles: REGRESSION,
        implementation,
        soft_regression_epsilon: "0.01",
        schema: Some(schema),
        retirement: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checker_id_requires_closed_kind_domain_and_three_segments() {
        let id = CheckerId::new("behavior.skill.no-drift").expect("valid checker id");
        assert_eq!(id.as_str(), "behavior.skill.no-drift");
        assert_eq!(id.kind(), Kind::Behavior);
        assert_eq!(id.domain(), Domain::Skill);
        for input in [
            "behavior",
            "behavior.skill",
            "Behavior.skill.case",
            "a..b",
            "lint.skill.case",
            "behavior.unknown.case",
            "behavior.skill.double--dash",
        ] {
            assert!(CheckerId::new(input).is_err(), "{input}");
        }
    }

    #[test]
    fn builtin_registry_contains_v1_checker_portfolio() {
        let registry = Registry::builtin().expect("registry builds");
        for id in [
            "preflight.skill.registry",
            "preflight.skill.materialization",
            "preflight.skill.protocol-boundary",
            "preflight.template.compile",
            "preflight.template.conformance",
            "preflight.tune.case-validation",
            "behavior.review.finding-recall",
            "behavior.todo.decomposition",
            "behavior.loop.verify-after-edit",
            "behavior.loop.scope-discipline",
            "behavior.inbox.resolution-path",
            "behavior.tune.apply-handoff",
            "behavior.agent.context-before-edit",
        ] {
            let id = CheckerId::new(id).expect("valid id");
            assert_eq!(
                registry.require_active(&id).expect("registered").status,
                Status::Active
            );
        }
    }

    #[test]
    fn disabled_policy_rejects_mandatory_unknown_and_retired_checkers() {
        let registry = Registry::builtin().expect("registry builds");
        let mandatory = CheckerId::new("preflight.skill.registry").expect("valid id");
        assert!(matches!(
            registry.validate_disabled(&[mandatory]),
            Err(RegistryError::MandatoryDisabled { .. })
        ));
        let unknown = CheckerId::new("behavior.skill.unknown").expect("valid id");
        assert!(matches!(
            registry.validate_disabled(&[unknown]),
            Err(RegistryError::Unknown { .. })
        ));
        let retired = CheckerId::new("behavior.review.legacy-recall").expect("valid id");
        assert!(matches!(
            registry.validate_disabled(&[retired]),
            Err(RegistryError::Retired { .. })
        ));
        let optional = CheckerId::new("behavior.review.finding-recall").expect("valid id");
        let disabled = registry
            .validate_disabled(std::slice::from_ref(&optional))
            .expect("optional checker can be disabled");
        assert!(disabled.contains(&optional));
    }
}

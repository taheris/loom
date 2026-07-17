use std::collections::BTreeSet;
use std::fmt;

use displaydoc::Display;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::case::{Case, Id as CaseId, LoadedCases};
use crate::checker::{CheckerId, Kind as CheckerKind, Level, Registry, RegistryError};
use crate::config::TuneConfig;
use crate::evidence::{Item as EvidenceItem, ItemId, Snapshot as EvidenceSnapshot};
use crate::target::{Kind as TargetKind, Target};

/// Inputs that deterministically freeze a checker plan.
pub struct Request<'a> {
    pub targets: Vec<Target>,
    pub level: Level,
    pub cases: &'a LoadedCases,
    pub evidence: &'a EvidenceSnapshot,
    pub config: &'a TuneConfig,
    pub registry: &'a Registry,
    pub seed: u64,
}

/// Frozen checker plan recorded before candidate generation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrozenPlan {
    pub level: Level,
    pub seed: u64,
    pub targets: Vec<Target>,
    pub preflight_checkers: Vec<CheckerId>,
    pub checker_plan: Vec<CheckerId>,
    pub selected_cases: Vec<SelectedCase>,
    pub skipped_cases: Vec<SkippedCase>,
    pub outcome_skeletons: Vec<OutcomeSkeleton>,
    pub diagnostics: Vec<Diagnostic>,
    pub evidence_split: crate::evidence::SplitMetadata,
    pub plan_hash: Hash,
}

impl FrozenPlan {
    pub fn reject_if_changed(&self, rebuilt: &Self) -> Result<(), PlanError> {
        if self.plan_hash == rebuilt.plan_hash {
            Ok(())
        } else {
            Err(PlanError::PlanChanged {
                expected: self.plan_hash.clone(),
                actual: rebuilt.plan_hash.clone(),
            })
        }
    }
}

/// Stable plan hash.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Hash(String);

impl Hash {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Selected behavioral case row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedCase {
    pub case_id: PlannedCaseId,
    pub checker: CheckerId,
    pub pool: Pool,
}

/// Skipped behavioral case row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedCase {
    pub case_id: PlannedCaseId,
    pub checker: CheckerId,
    pub pool: Pool,
    pub reason: SkipReason,
}

/// Planned case id from declared or mined evidence pools.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum PlannedCaseId {
    Declared(CaseId),
    Mined(ItemId),
}

impl fmt::Display for PlannedCaseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Declared(id) => write!(f, "declared:{id}"),
            Self::Mined(id) => write!(f, "mined:{id}"),
        }
    }
}

/// Case pool source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pool {
    DeclaredRegression,
    MinedSelection,
    MinedTrain,
}

/// Case skip reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    TargetMismatch,
    FastLevelSkipsBehavior,
    CapExceeded,
    TrainEvidenceWithheld,
    CheckerDisabled,
    CheckerLevelUnsupported,
}

/// Future checker outcome slot for a selected case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeSkeleton {
    pub case_id: PlannedCaseId,
    pub checker: CheckerId,
    pub pool: Pool,
}

/// Plan diagnostic severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
}

/// Plan diagnostic for bead and manifest consumers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: DiagnosticSeverity,
    pub message: String,
}

/// Build and freeze a deterministic checker plan.
pub fn build(request: Request<'_>) -> Result<FrozenPlan, PlanError> {
    let disabled = request
        .registry
        .validate_disabled(&request.config.checks.disabled)?;
    let mut targets = request.targets;
    targets.sort();
    targets.dedup();
    let target_kinds = targets.iter().map(Target::kind).collect::<BTreeSet<_>>();
    let preflight_checkers = select_preflight(request.registry, &target_kinds);
    let mut state = PlanState::default();
    let declared_context = DeclaredContext {
        targets: &targets,
        level: request.level,
        cap: request.config.checks.max_behavior_cases,
        seed: request.seed,
    };
    let applicable_declared =
        partition_declared(request.cases.cases(), &declared_context, &mut state);
    let remaining_cap = behavior_remaining_cap(
        request.level,
        request.config.checks.max_behavior_cases,
        applicable_declared,
        state.selected_cases.len(),
    );
    let mined_context = MinedContext {
        targets: &targets,
        level: request.level,
        registry: request.registry,
        disabled: &disabled,
        cap: remaining_cap,
        seed: request.seed,
    };
    partition_mined(request.evidence, &mined_context, &mut state)?;

    state
        .selected_cases
        .sort_by(|left, right| left.case_id.cmp(&right.case_id));
    state
        .skipped_cases
        .sort_by(|left, right| left.case_id.cmp(&right.case_id));
    let outcome_skeletons = state
        .selected_cases
        .iter()
        .map(|selected| OutcomeSkeleton {
            case_id: selected.case_id.clone(),
            checker: selected.checker.clone(),
            pool: selected.pool,
        })
        .collect::<Vec<_>>();
    let checker_plan = checker_plan(&preflight_checkers, &state.selected_cases);
    let hashable = HashablePlan {
        level: request.level,
        seed: request.seed,
        targets,
        preflight_checkers,
        checker_plan,
        selected_cases: state.selected_cases,
        skipped_cases: state.skipped_cases,
        outcome_skeletons,
        diagnostics: state.diagnostics,
        evidence_split: request.evidence.metadata.clone(),
    };
    let plan_hash = hash_plan(&hashable, request.registry)?;
    Ok(FrozenPlan {
        level: hashable.level,
        seed: hashable.seed,
        targets: hashable.targets,
        preflight_checkers: hashable.preflight_checkers,
        checker_plan: hashable.checker_plan,
        selected_cases: hashable.selected_cases,
        skipped_cases: hashable.skipped_cases,
        outcome_skeletons: hashable.outcome_skeletons,
        diagnostics: hashable.diagnostics,
        evidence_split: hashable.evidence_split,
        plan_hash,
    })
}

fn select_preflight(registry: &Registry, target_kinds: &BTreeSet<TargetKind>) -> Vec<CheckerId> {
    let mut selected = registry
        .active()
        .filter(|metadata| metadata.id.kind() == CheckerKind::Preflight)
        .filter(|metadata| {
            metadata.id.as_str() == "preflight.tune.case-validation"
                || metadata
                    .target_kinds
                    .iter()
                    .any(|kind| target_kinds.contains(kind))
        })
        .map(|metadata| metadata.id.clone())
        .collect::<Vec<_>>();
    selected.sort();
    selected
}

#[derive(Default)]
struct PlanState {
    selected_cases: Vec<SelectedCase>,
    skipped_cases: Vec<SkippedCase>,
    diagnostics: Vec<Diagnostic>,
}

struct DeclaredContext<'a> {
    targets: &'a [Target],
    level: Level,
    cap: usize,
    seed: u64,
}

fn partition_declared(
    cases: &[Case],
    context: &DeclaredContext<'_>,
    state: &mut PlanState,
) -> usize {
    let mut applicable = Vec::new();
    for case in cases {
        if case
            .targets
            .iter()
            .any(|case_target| case_target.intersects_any(context.targets))
        {
            applicable.push(case);
        } else {
            state
                .skipped_cases
                .push(skip_declared(case, SkipReason::TargetMismatch));
        }
    }
    if context.level == Level::Fast {
        state.skipped_cases.extend(
            applicable
                .iter()
                .map(|case| skip_declared(case, SkipReason::FastLevelSkipsBehavior)),
        );
        return applicable.len();
    }
    let selected_ids = if context.level == Level::Full {
        applicable.iter().map(|case| case.id.clone()).collect()
    } else {
        sample_case_ids(&applicable, context.cap, context.seed)
    };
    for case in applicable {
        if selected_ids.contains(&case.id) {
            state.selected_cases.push(select_declared(case));
        } else {
            state
                .skipped_cases
                .push(skip_declared(case, SkipReason::CapExceeded));
            state.diagnostics.push(Diagnostic {
                severity: DiagnosticSeverity::Warning,
                message: format!(
                    "declared regression case `{}` skipped by max_behavior_cases; use full or raise the cap",
                    case.id
                ),
            });
        }
    }
    selected_ids.len()
}

fn behavior_remaining_cap(
    level: Level,
    cap: usize,
    applicable_declared: usize,
    selected: usize,
) -> usize {
    match level {
        Level::Fast => 0,
        Level::Run => cap.saturating_sub(selected),
        Level::Full => cap.saturating_sub(applicable_declared),
    }
}

struct MinedContext<'a> {
    targets: &'a [Target],
    level: Level,
    registry: &'a Registry,
    disabled: &'a BTreeSet<CheckerId>,
    cap: usize,
    seed: u64,
}

fn partition_mined(
    evidence: &EvidenceSnapshot,
    context: &MinedContext<'_>,
    state: &mut PlanState,
) -> Result<(), PlanError> {
    for item in &evidence.train {
        state.skipped_cases.push(skip_mined(
            item,
            Pool::MinedTrain,
            SkipReason::TrainEvidenceWithheld,
        ));
    }
    if context.level == Level::Fast {
        for item in &evidence.selection {
            state.skipped_cases.push(skip_mined(
                item,
                Pool::MinedSelection,
                SkipReason::FastLevelSkipsBehavior,
            ));
        }
        return Ok(());
    }
    let mut applicable = Vec::new();
    for item in &evidence.selection {
        let metadata = context.registry.require_active(&item.checker)?;
        if context.disabled.contains(&item.checker) {
            state.skipped_cases.push(skip_mined(
                item,
                Pool::MinedSelection,
                SkipReason::CheckerDisabled,
            ));
        } else if !metadata.supports_level(context.level) {
            state.skipped_cases.push(skip_mined(
                item,
                Pool::MinedSelection,
                SkipReason::CheckerLevelUnsupported,
            ));
        } else if !item
            .targets
            .iter()
            .any(|item_target| item_target.intersects_any(context.targets))
        {
            state.skipped_cases.push(skip_mined(
                item,
                Pool::MinedSelection,
                SkipReason::TargetMismatch,
            ));
        } else {
            applicable.push(item);
        }
    }
    let selected_ids = sample_item_ids(&applicable, context.cap, context.seed);
    for item in applicable {
        if selected_ids.contains(&item.id) {
            state.selected_cases.push(SelectedCase {
                case_id: PlannedCaseId::Mined(item.id.clone()),
                checker: item.checker.clone(),
                pool: Pool::MinedSelection,
            });
        } else {
            state.skipped_cases.push(skip_mined(
                item,
                Pool::MinedSelection,
                SkipReason::CapExceeded,
            ));
        }
    }
    Ok(())
}

fn checker_plan(preflight: &[CheckerId], selected: &[SelectedCase]) -> Vec<CheckerId> {
    let mut ids = preflight.iter().cloned().collect::<BTreeSet<_>>();
    ids.extend(selected.iter().map(|case| case.checker.clone()));
    ids.into_iter().collect()
}

fn select_declared(case: &Case) -> SelectedCase {
    SelectedCase {
        case_id: PlannedCaseId::Declared(case.id.clone()),
        checker: case.checker.clone(),
        pool: Pool::DeclaredRegression,
    }
}

fn skip_declared(case: &Case, reason: SkipReason) -> SkippedCase {
    SkippedCase {
        case_id: PlannedCaseId::Declared(case.id.clone()),
        checker: case.checker.clone(),
        pool: Pool::DeclaredRegression,
        reason,
    }
}

fn skip_mined(item: &EvidenceItem, pool: Pool, reason: SkipReason) -> SkippedCase {
    SkippedCase {
        case_id: PlannedCaseId::Mined(item.id.clone()),
        checker: item.checker.clone(),
        pool,
        reason,
    }
}

fn sample_case_ids(cases: &[&Case], cap: usize, seed: u64) -> BTreeSet<CaseId> {
    let mut scored = cases
        .iter()
        .map(|case| {
            (
                sample_key(seed, "declared", case.id.as_str()),
                case.id.clone(),
            )
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    scored.into_iter().take(cap).map(|(_, id)| id).collect()
}

fn sample_item_ids(items: &[&EvidenceItem], cap: usize, seed: u64) -> BTreeSet<ItemId> {
    let mut scored = items
        .iter()
        .map(|item| (sample_key(seed, "mined", item.id.as_str()), item.id.clone()))
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    scored.into_iter().take(cap).map(|(_, id)| id).collect()
}

fn sample_key(seed: u64, pool: &str, id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(seed.to_be_bytes());
    hasher.update(pool.as_bytes());
    hasher.update(id.as_bytes());
    hex(&hasher.finalize())
}

fn hash_plan(plan: &HashablePlan, registry: &Registry) -> Result<Hash, PlanError> {
    let payload = HashPayload {
        plan,
        registry: registry.metadata_snapshot(),
    };
    let bytes = serde_json::to_vec(&payload)?;
    let digest = Sha256::digest(&bytes);
    Ok(Hash(hex(&digest)))
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[derive(Debug, Serialize)]
struct HashPayload<'a> {
    plan: &'a HashablePlan,
    registry: Vec<crate::checker::Metadata>,
}

#[derive(Debug, Serialize)]
struct HashablePlan {
    level: Level,
    seed: u64,
    targets: Vec<Target>,
    preflight_checkers: Vec<CheckerId>,
    checker_plan: Vec<CheckerId>,
    selected_cases: Vec<SelectedCase>,
    skipped_cases: Vec<SkippedCase>,
    outcome_skeletons: Vec<OutcomeSkeleton>,
    diagnostics: Vec<Diagnostic>,
    evidence_split: crate::evidence::SplitMetadata,
}

/// Checker planning failures.
#[derive(Debug, Display, Error)]
pub enum PlanError {
    /// checker registry policy rejected the plan input
    Registry(#[from] RegistryError),
    /// failed to serialize checker plan for hashing
    Serialize(#[from] serde_json::Error),
    /// frozen checker plan changed after candidate generation: expected `{expected}`, got `{actual}`
    PlanChanged { expected: Hash, actual: Hash },
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::case::{
        Case, Expected, Input, LoadedCases, LoadedDocument, PathKind, RepoPath, Source,
    };
    use crate::checker::{CaseRole, CheckerId, Level, Registry};
    use crate::config::{ChecksConfig, EvidenceConfig, SelectionFraction, TuneConfig};
    use crate::evidence::{Item, ItemId, Snapshot, SplitMetadata};
    use crate::target::Target;

    use super::*;

    fn target() -> Target {
        "skill:loom-review-finding-recall".parse().expect("target")
    }

    fn case(id: &str) -> Case {
        Case {
            id: CaseId::new(id).expect("case id"),
            checker: CheckerId::new("behavior.review.finding-recall").expect("checker"),
            targets: vec![target()],
            role: CaseRole::Regression,
            input: Input::ReviewFindingRecall {
                patch: RepoPath {
                    relative: "docs/cases/review.diff".into(),
                    kind: PathKind::File,
                },
            },
            expected: Expected::ReviewFindingRecall(crate::case::ReviewExpected {
                findings: vec![crate::case::ReviewFinding {
                    contains: vec!["missing".to_owned()],
                    file: None,
                }],
                max_extra_findings: None,
            }),
            source: Source {
                path: "docs/tuning.md".into(),
                line: 1,
            },
        }
    }

    fn evidence_snapshot() -> Snapshot {
        Snapshot {
            train: vec![Item::for_test(
                ItemId::new("train-1").expect("id"),
                CheckerId::new("behavior.review.finding-recall").expect("checker"),
                vec![target()],
            )],
            selection: vec![
                Item::for_test(
                    ItemId::new("selection-1").expect("id"),
                    CheckerId::new("behavior.review.finding-recall").expect("checker"),
                    vec![target()],
                ),
                Item::for_test(
                    ItemId::new("selection-2").expect("id"),
                    CheckerId::new("behavior.review.finding-recall").expect("checker"),
                    vec![target()],
                ),
            ],
            metadata: SplitMetadata {
                algorithm: "sha256-salt-v1".to_owned(),
                salt_id: "repo".to_owned(),
                selection_fraction: SelectionFraction::new(0.34).expect("fraction"),
            },
        }
    }

    fn loaded_cases() -> LoadedCases {
        LoadedCases::new(
            vec![case("case-a"), case("case-b"), case("case-c")],
            vec![LoadedDocument {
                path: "docs/tuning.md".into(),
                kind: crate::case::DocumentKind::Repo,
                case_count: 3,
            }],
        )
    }

    fn request<'a>(
        cases: &'a LoadedCases,
        evidence: &'a Snapshot,
        config: &'a TuneConfig,
        registry: &'a Registry,
        seed: u64,
    ) -> Request<'a> {
        Request {
            targets: vec![target()],
            level: Level::Run,
            cases,
            evidence,
            config,
            registry,
            seed,
        }
    }

    #[test]
    fn checker_plan_is_seed_deterministic_and_frozen() {
        let registry = Registry::builtin().expect("registry");
        let cases = loaded_cases();
        let evidence = evidence_snapshot();
        let config = TuneConfig {
            checks: ChecksConfig {
                max_behavior_cases: 2,
                ..ChecksConfig::default()
            },
            evidence: EvidenceConfig::default(),
        };
        let first = build(request(&cases, &evidence, &config, &registry, 42)).expect("plan");
        let second = build(request(&cases, &evidence, &config, &registry, 42)).expect("plan");
        assert_eq!(first.plan_hash, second.plan_hash);
        assert_eq!(first.selected_cases, second.selected_cases);
        let changed = build(request(&cases, &evidence, &config, &registry, 43)).expect("plan");
        assert_ne!(first.plan_hash, changed.plan_hash);
        assert!(matches!(
            first.reject_if_changed(&changed),
            Err(PlanError::PlanChanged { .. })
        ));
    }

    #[test]
    fn fast_level_freezes_preflight_and_skips_behavior() {
        let registry = Registry::builtin().expect("registry");
        let cases = loaded_cases();
        let evidence = evidence_snapshot();
        let config = TuneConfig::default();
        let plan = build(Request {
            targets: vec![target()],
            level: Level::Fast,
            cases: &cases,
            evidence: &evidence,
            config: &config,
            registry: &registry,
            seed: 7,
        })
        .expect("plan");
        assert!(plan.selected_cases.is_empty());
        assert!(
            plan.checker_plan
                .iter()
                .all(|id| id.kind() == CheckerKind::Preflight)
        );
        assert!(
            plan.skipped_cases
                .iter()
                .any(|case| case.reason == SkipReason::FastLevelSkipsBehavior)
        );
    }

    #[test]
    fn disabled_optional_checker_is_omitted_from_mined_evidence() {
        let registry = Registry::builtin().expect("registry");
        let cases = LoadedCases::new(Vec::new(), Vec::new());
        let evidence = evidence_snapshot();
        let config = TuneConfig {
            checks: ChecksConfig {
                disabled: vec![CheckerId::new("behavior.review.finding-recall").expect("id")],
                ..ChecksConfig::default()
            },
            evidence: EvidenceConfig::default(),
        };
        let plan = build(request(&cases, &evidence, &config, &registry, 1)).expect("plan");
        assert!(plan.selected_cases.is_empty());
        assert!(
            plan.skipped_cases
                .iter()
                .any(|case| case.reason == SkipReason::CheckerDisabled)
        );
    }

    #[test]
    fn target_mismatch_cases_are_recorded_as_skipped() {
        let registry = Registry::builtin().expect("registry");
        let cases = LoadedCases::new(vec![case("case-a")], Vec::new());
        let evidence = Snapshot {
            selection: Vec::new(),
            train: Vec::new(),
            metadata: evidence_snapshot().metadata,
        };
        let config = TuneConfig::default();
        let plan = build(Request {
            targets: vec!["phase:todo".parse().expect("target")],
            level: Level::Run,
            cases: &cases,
            evidence: &evidence,
            config: &config,
            registry: &registry,
            seed: 1,
        })
        .expect("plan");
        assert_eq!(plan.selected_cases.len(), 0);
        assert_eq!(plan.skipped_cases[0].reason, SkipReason::TargetMismatch);
    }

    #[test]
    fn full_level_selects_all_declared_before_mined_cap() {
        let registry = Registry::builtin().expect("registry");
        let cases = loaded_cases();
        let evidence = evidence_snapshot();
        let config = TuneConfig {
            checks: ChecksConfig {
                max_behavior_cases: 1,
                ..ChecksConfig::default()
            },
            evidence: EvidenceConfig::default(),
        };
        let plan = build(Request {
            targets: vec![target()],
            level: Level::Full,
            cases: &cases,
            evidence: &evidence,
            config: &config,
            registry: &registry,
            seed: 1,
        })
        .expect("plan");
        let declared = plan
            .selected_cases
            .iter()
            .filter(|case| case.pool == Pool::DeclaredRegression)
            .count();
        assert_eq!(declared, 3);
        assert!(plan.skipped_cases.iter().any(
            |case| case.pool == Pool::MinedSelection && case.reason == SkipReason::CapExceeded
        ));
    }

    #[test]
    fn checker_plan_keeps_preflight_when_no_behavior_is_selected() {
        let registry = Registry::builtin().expect("registry");
        let cases = LoadedCases::new(Vec::new(), Vec::new());
        let evidence = Snapshot {
            train: Vec::new(),
            selection: Vec::new(),
            metadata: evidence_snapshot().metadata,
        };
        let config = TuneConfig::default();
        let plan = build(request(&cases, &evidence, &config, &registry, 1)).expect("plan");
        assert!(!plan.preflight_checkers.is_empty());
        assert_eq!(
            plan.checker_plan.iter().cloned().collect::<BTreeSet<_>>(),
            plan.preflight_checkers
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
        );
    }
}

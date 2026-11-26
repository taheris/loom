//! Selection is a read-only projection; execution always retains the complete admitted closure.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde::Serialize;

use super::Plan;
use crate::annotation::{Annotation, Tier};
use crate::inventory::Name;
use crate::inventory::snapshot::ResourcePath;
use crate::provider::tracked::Observed;
use crate::provider::{Candidate, Digest, Error, Execution, Input, Origin, Unit};
use crate::scope::Resolved;

/// Applicability is supplied by the driver/hook boundary, never a free-form stage string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    Feedback,
    Publication,
    Diagnostic,
}

/// Why an admitted execution is selected or excluded, independently of its analysis inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum Reason {
    WholeTree,
    ExactTarget,
    OwnerLocal,
    RequiredPublicationSuite,
    UnchangedPublicationTestPartition,
    Affected {
        inputs: Vec<Input>,
    },
    Unaffected,
    OtherTarget,
    EmptyFiniteScope,
    OtherLane,
    DeferredUntilPublication,
    /// Foreign runtime coverage is unverified, not a passing or skipped native obligation.
    OutsidePlatformScope {
        system: Name,
    },
}

impl Reason {
    pub const fn selected(&self) -> bool {
        matches!(
            self,
            Self::WholeTree
                | Self::ExactTarget
                | Self::OwnerLocal
                | Self::RequiredPublicationSuite
                | Self::Affected { .. }
        )
    }
}

/// Includes excluded siblings and input provenance, never just a caller's target list.
pub struct Decision<'a> {
    unit: &'a Unit,
    reason: Reason,
    owners: Vec<Annotation>,
    baseline: Option<Tier>,
}

impl<'a> Decision<'a> {
    pub const fn unit(&self) -> &'a Unit {
        self.unit
    }

    pub const fn reason(&self) -> &Reason {
        &self.reason
    }

    pub fn owners(&self) -> &[Annotation] {
        &self.owners
    }

    pub const fn baseline_tier(&self) -> Option<Tier> {
        self.baseline
    }
}

/// A checked scope over a complete plan, not evidence or publication authorization.
pub struct Selection<'a> {
    plan: &'a Plan,
    scope: Resolved,
    stage: Stage,
    decisions: Vec<Decision<'a>>,
    cli_partition: Option<(&'a Unit, super::super::nix::cli_partition::Partition)>,
}

/// A single observed invocation may discharge multiple canonical acceptance owners.
#[derive(Serialize)]
pub struct Invocation {
    observed: Observed,
    duration: Duration,
    owners: Vec<Annotation>,
    baseline: Option<Tier>,
    shared_with: Option<Name>,
}

impl Invocation {
    pub const fn observed(&self) -> &Observed {
        &self.observed
    }

    pub const fn duration(&self) -> Duration {
        self.duration
    }

    pub fn owners(&self) -> &[Annotation] {
        &self.owners
    }

    pub const fn baseline_tier(&self) -> Option<Tier> {
        self.baseline
    }

    /// Only this invocation's first equivalent execution counts as a subprocess/check call.
    pub const fn shared_with(&self) -> Option<&Name> {
        self.shared_with.as_ref()
    }

    pub fn into_observed(self) -> Observed {
        self.observed
    }
}

impl Plan {
    /// Resolve affectedness using exact admitted resources, not globs or annotation guesses.
    ///
    /// # Errors
    /// An invalid sibling rejects selection even if that sibling would be excluded.
    pub fn select(&self, scope: &Resolved) -> Result<Selection<'_>, Error> {
        self.select_tiers(scope, &[Tier::Check, Tier::Test, Tier::System])
    }

    /// Select a closed deterministic lane set, retaining native baselines without annotations.
    ///
    /// # Errors
    /// Rechecks the complete admitted context, not only the requested lanes.
    pub fn select_tiers(&self, scope: &Resolved, tiers: &[Tier]) -> Result<Selection<'_>, Error> {
        let stage = if scope.target().is_some() {
            Stage::Diagnostic
        } else {
            Stage::Feedback
        };
        self.select_at(scope, tiers, stage)
    }

    /// Select lanes and criterion-local applicability without reducing admission coverage.
    ///
    /// # Errors
    /// Invalid context rejects the entire request before any selected unit runs.
    pub fn select_at(
        &self,
        scope: &Resolved,
        tiers: &[Tier],
        stage: Stage,
    ) -> Result<Selection<'_>, Error> {
        self.require_current()?;
        if scope.platform() != self.platform {
            return Err(Error::PlatformScope);
        }
        if self
            .feedback_scope
            .as_ref()
            .is_some_and(|prepared| prepared != scope || stage != Stage::Feedback)
        {
            return Err(Error::FeedbackScope);
        }
        let stage = if scope.target().is_some() {
            Stage::Diagnostic
        } else {
            stage
        };
        let files = scope.files().map(|files| {
            let root = self.snapshot().root();
            files
                .iter()
                .map(|file| file.strip_prefix(root).unwrap_or(file))
                .collect::<BTreeSet<_>>()
        });
        let mut decisions: Vec<_> = self
            .units()
            .iter()
            .map(|unit| {
                let lane_owners: Vec<_> = unit
                    .annotations()
                    .iter()
                    .filter(|annotation| tiers.contains(&annotation.tier))
                    .cloned()
                    .collect();
                let owners: Vec<_> = lane_owners
                    .iter()
                    .filter(|annotation| {
                        scope.is_tree()
                            || stage != Stage::Feedback
                            || !self.deferred.contains(annotation)
                    })
                    .cloned()
                    .collect();
                let baseline = (self.baselines.contains(unit.target())
                    && unit.outside_platform_scope().is_none()
                    && tiers.contains(&native_tier(unit)))
                .then(|| native_tier(unit));
                let reason = if owners.is_empty() && baseline.is_none() {
                    if let Some(system) = unit.outside_platform_scope()
                        && lane_owners.is_empty()
                    {
                        Reason::OutsidePlatformScope {
                            system: system.clone(),
                        }
                    } else if lane_owners.is_empty() {
                        Reason::OtherLane
                    } else {
                        Reason::DeferredUntilPublication
                    }
                } else if self.feedback_scope.is_some()
                    && baseline.is_some()
                    && unit
                        .nix_graph()
                        .is_some_and(|graph| graph.cargo_package().is_some())
                {
                    Reason::OwnerLocal
                } else if scope.is_tree() {
                    Reason::WholeTree
                } else if stage == Stage::Publication
                    && baseline == Some(Tier::Test)
                    && unit
                        .nix_graph()
                        .is_some_and(crate::provider::nix::Graph::is_workspace_test_suite)
                {
                    // Always owe this baseline on publication. A diff may affect
                    // none of its named criteria; nothing else runs this suite.
                    Reason::RequiredPublicationSuite
                } else if let Some(target) = scope.target() {
                    if target == unit.target().as_str() {
                        Reason::ExactTarget
                    } else {
                        Reason::OtherTarget
                    }
                } else if let Some(files) = &files {
                    if files.is_empty() {
                        Reason::EmptyFiniteScope
                    } else {
                        let inputs: Vec<_> = unit
                            .inputs()
                            .iter()
                            .filter(|input| files.contains(input.path().as_path()))
                            .cloned()
                            .collect();
                        if inputs.is_empty() {
                            Reason::Unaffected
                        } else {
                            Reason::Affected { inputs }
                        }
                    }
                } else {
                    Reason::Unaffected
                };
                Decision {
                    unit,
                    reason,
                    owners,
                    baseline,
                }
            })
            .collect();
        // Lane order is semantic, not target-name order or caller argument order.
        // A shared unit runs at its earliest selected owner/baseline lane.
        decisions.sort_by_key(|decision| {
            decision
                .owners
                .iter()
                .map(|owner| owner.tier)
                .chain(decision.baseline)
                .map(lane_order)
                .min()
                .unwrap_or_else(|| lane_order(native_tier(decision.unit)))
        });
        Ok(Selection {
            plan: self,
            scope: scope.clone(),
            stage,
            decisions,
            cli_partition: None,
        })
    }
}

const fn lane_order(tier: Tier) -> u8 {
    match tier {
        Tier::Check => 0,
        Tier::Test => 1,
        Tier::System => 2,
        Tier::Judge => 3,
    }
}

fn execution_inputs(unit: &Unit) -> impl Iterator<Item = (&ResourcePath, &Digest)> {
    // Owning contracts participate in selection and freshness, but are not an
    // execution argument. Keep contracts that are also real analysis/runtime
    // inputs; exclude only the attribution-only additions made at resolution.
    unit.inputs()
        .iter()
        .filter(|input| {
            input
                .origins()
                .iter()
                .any(|origin| !matches!(origin, Origin::Contract(_)))
        })
        .map(|input| (input.path(), input.content()))
}

pub(crate) fn same_execution(left: &Unit, right: &Unit) -> bool {
    let matching_execution = match (&left.definition.execution, &right.definition.execution) {
        (
            Execution::Tracked {
                members: left_members,
                executable: left_executable,
                ..
            },
            Execution::Tracked {
                members: right_members,
                executable: right_executable,
                ..
            },
        ) => {
            left_members == right_members
                && left_executable == right_executable
                && matches!((left.description(), right.description()),
                (Candidate::TrackedRust { execution: left, .. }, Candidate::TrackedRust { execution: right, .. }) if left == right)
        }
        (
            Execution::Nix(left) | Execution::Report { graph: left, .. },
            Execution::Nix(right) | Execution::Report { graph: right, .. },
        ) => left.shares_execution_with(right),
        (Execution::NativePlatform(left), Execution::NativePlatform(right)) => {
            left.same_operation(right)
        }
        (Execution::Effect(_), Execution::Effect(_)) => matches!(
            (left.description(), right.description()),
            (Candidate::SupportedEffect { execution: left, .. }, Candidate::SupportedEffect { execution: right, .. }) if left == right
        ),
        _ => false,
    };
    matching_execution && execution_inputs(left).eq(execution_inputs(right))
}

fn native_tier(unit: &Unit) -> Tier {
    unit.baseline_tier()
}

impl<'a> Selection<'a> {
    pub(super) fn require_current(&self) -> Result<(), Error> {
        self.plan.require_current()
    }

    fn require_ready(&self) -> Result<(), Error> {
        self.plan.require_current()?;
        let mut checked = Vec::<&Unit>::new();
        let mut derivations = BTreeSet::new();
        let mut readiness = super::super::nix::readiness::Readiness::default();
        for decision in self
            .decisions
            .iter()
            .filter(|decision| decision.reason.selected())
        {
            if checked
                .iter()
                .any(|unit| same_execution(unit, decision.unit))
            {
                continue;
            }
            if let Some(graph) = decision.unit.nix_graph() {
                let cold = readiness.cold_inputs(graph)?;
                tracing::debug!(
                    target = decision.unit.target().as_str(),
                    cold_inputs = cold.len(),
                    "checked selected native readiness"
                );
                derivations.extend(cold);
            }
            checked.push(decision.unit);
        }
        self.plan.require_current()?;
        if !derivations.is_empty() {
            return Err(super::super::nix::Error::ColdInputs { derivations }.into());
        }
        Ok(())
    }

    /// Restrict only the canonical test execution on a closed verifier CLI edit.
    /// A missing proof retains the ordinary full suite and all other checks.
    ///
    /// # Errors
    /// Rejects stale plan context before or after the admission attempt.
    pub fn admit_cli_partition(&mut self) -> Result<(), Error> {
        if self.stage != Stage::Publication || self.scope.diff().is_none() {
            return Ok(());
        }
        self.require_current()?;
        let canonical = format!(
            "nix build .#packages.{}.loom-tests",
            crate::runner::result::current_platform()
        );
        let Some(root) = self.decisions.iter().find_map(|decision| {
            (decision.reason == Reason::RequiredPublicationSuite
                && decision.unit.target().as_str() == canonical)
                .then_some(decision.unit)
        }) else {
            return Ok(());
        };
        let Some(graph) = root.nix_graph() else {
            return Ok(());
        };
        let partition = super::super::nix::cli_partition::Partition::admit(
            self.plan.snapshot(),
            &self.scope,
            graph,
        );
        self.require_current()?;
        let partition = match partition {
            Ok(partition) => partition,
            Err(error) => {
                tracing::debug!(%error, "no closed CLI test partition; keep the complete native suite");
                return Ok(());
            }
        };
        for decision in &mut self.decisions {
            if decision.reason.selected()
                && same_execution(root, decision.unit)
                && let Execution::Report { target, .. } = &decision.unit.definition.execution
                && !partition.contains(target)
            {
                decision.reason = Reason::UnchangedPublicationTestPartition;
            }
        }
        self.cli_partition = Some((root, partition));
        Ok(())
    }

    /// Assign every native root-flake build on this exact publication plan.
    /// The independently required first hook still checks flake structure.
    ///
    /// # Errors
    /// Fail closed on stale admission or a failed raw flake obligation.
    pub fn start_root_flake_checks(
        &self,
    ) -> Result<crate::provider::nix::flake_checks::Checks, crate::provider::nix::flake_checks::Error>
    {
        self.require_current()?;
        crate::provider::nix::flake_checks::Checks::start(self, self.plan.snapshot())
    }

    pub const fn stage(&self) -> Stage {
        self.stage
    }
    pub const fn scope(&self) -> &Resolved {
        &self.scope
    }

    pub fn decisions(&self) -> &[Decision<'a>] {
        &self.decisions
    }

    /// Revalidate a narrowed suite's exact endpoints before success.
    ///
    /// # Errors
    /// A stale checkout cannot discharge the narrowed suite.
    pub fn require_partition_current(&self) -> Result<(), Error> {
        if let Some((_, partition)) = &self.cli_partition {
            partition.require_current()?;
        }
        Ok(())
    }

    /// Selection never narrows the resources a selected cross-file check can inspect.
    pub fn analysis_inputs(&self) -> impl Iterator<Item = &ResourcePath> {
        self.decisions
            .iter()
            .filter(|decision| decision.reason.selected())
            .flat_map(|decision| decision.unit.inputs().iter().map(Input::path))
    }

    /// Required executions are fresh; equivalent current invocations may share within this run only.
    /// Each alias retains its own owners and outcome, including capability and inapplicability states.
    ///
    /// # Errors
    /// Revalidates the whole context before any invocation, not only selected siblings.
    pub fn run(&self) -> Result<Vec<Invocation>, Error> {
        self.invocations()?.collect()
    }

    /// Capture equivalent projections and adjacent isolated checks in order.
    ///
    /// Whole-context checks surround each effect or bounded batch, not
    /// each later delivery of an already captured historical fact. Consumers must
    /// revalidate the plan before reporting current-scope success. Dropping this
    /// stream never launches another effect or another batch.
    /// Use `invocations` when a caller needs revalidation between alias deliveries.
    ///
    /// # Errors
    /// Rejects stale context even when no execution is selected.
    pub fn observations(&self) -> Result<Observations<'_, '_>, Error> {
        self.require_ready()?;
        Ok(Observations {
            selection: self,
            decisions: self.decisions.iter(),
            captured: BTreeMap::new(),
            failed: false,
        })
    }

    /// Stream fresh, current-run-shared observations without losing earlier
    /// failures when a later execution reports infrastructure failure.
    ///
    /// # Errors
    /// Rejects stale context before the first callback, including an empty selection.
    pub fn invocations(&self) -> Result<Invocations<'_, '_>, Error> {
        self.require_ready()?;
        Ok(Invocations {
            selection: self,
            decisions: self.decisions.iter(),
            executed: Vec::new(),
            failed: false,
        })
    }
}

/// Lazy execution is confined to this selection and invocation. Dropping the
/// iterator launches nothing else; the first error permanently stops it.
pub struct Invocations<'selection, 'plan> {
    selection: &'selection Selection<'plan>,
    decisions: std::slice::Iter<'selection, Decision<'plan>>,
    executed: Vec<(&'plan Unit, Observed, Duration)>,
    failed: bool,
}

impl Iterator for Invocations<'_, '_> {
    type Item = Result<Invocation, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let decision = self.decisions.find(|decision| decision.reason.selected())?;
        let result = (|| {
            self.selection.plan.require_current()?;
            let (observed, duration, shared_with) = if let Some((_, previous, duration)) = self
                .executed
                .iter()
                .find(|(unit, _, _)| same_execution(unit, decision.unit))
            {
                (
                    decision.unit.project(previous)?,
                    *duration,
                    Some(previous.target().clone()),
                )
            } else {
                let clock = loom_driver::clock::SystemClock::new();
                let start = loom_driver::clock::Clock::now(&clock);
                let observed = decision.unit.run()?;
                let duration =
                    loom_driver::clock::Clock::now(&clock).saturating_duration_since(start);
                self.executed
                    .push((decision.unit, observed.clone(), duration));
                (observed, duration, None)
            };
            Ok(Invocation {
                observed,
                duration,
                owners: decision.owners.clone(),
                baseline: decision.baseline,
                shared_with,
            })
        })();
        if result.is_err() {
            self.failed = true;
        }
        Some(result)
    }
}

impl std::iter::FusedIterator for Invocations<'_, '_> {}

/// Invocation-local facts, never cached verdicts or current-context authority.
///
/// Projections and bounded batches of isolated checks are captured inside live
/// plan barriers. Later deliveries do not launch callbacks or reinterpret captured
/// facts under a new context. The first error permanently stops execution,
/// including after source repair.
pub struct Observations<'selection, 'plan> {
    selection: &'selection Selection<'plan>,
    decisions: std::slice::Iter<'selection, Decision<'plan>>,
    captured: BTreeMap<Name, Result<Invocation, Error>>,
    failed: bool,
}

const ISOLATED_CHECK_BATCH: usize = 32;

type Captured = BTreeMap<Name, Result<Invocation, Error>>;

fn isolated_check(decision: &Decision<'_>) -> bool {
    decision.baseline.is_none_or(|tier| tier == Tier::Check)
        && decision
            .owners
            .iter()
            .all(|owner| owner.tier == Tier::Check)
        && match &decision.unit.definition.execution {
            Execution::Tracked { snapshot_only, .. } => *snapshot_only,
            Execution::Nix(graph) => graph.is_confined_realization(),
            _ => false,
        }
}

impl Observations<'_, '_> {
    fn capture(
        &self,
        decision: &Decision<'_>,
        captured: &mut Captured,
    ) -> Result<Invocation, Error> {
        use loom_driver::clock::{Clock as _, SystemClock};
        let clock = SystemClock::new();
        let start = clock.now();
        let observed = if let Some((root, partition)) = &self.selection.cli_partition
            && same_execution(root, decision.unit)
            && let Some(graph) = decision.unit.nix_graph()
        {
            decision
                .unit
                .project_observed(&partition.run(graph, decision.unit.target().clone())?)?
        } else {
            decision.unit.observe()?
        };
        let duration = clock.now().saturating_duration_since(start);
        let mut aliases = BTreeMap::new();
        for alias in self.selection.decisions.iter().filter(|alias| {
            alias.reason.selected()
                && alias.unit.target() != decision.unit.target()
                && same_execution(alias.unit, decision.unit)
        }) {
            aliases.insert(
                alias.unit.target().clone(),
                Ok(Invocation {
                    observed: alias.unit.project_observed(&observed)?,
                    duration,
                    owners: alias.owners.clone(),
                    baseline: alias.baseline,
                    shared_with: Some(decision.unit.target().clone()),
                }),
            );
        }
        captured.extend(aliases);
        Ok(Invocation {
            observed,
            duration,
            owners: decision.owners.clone(),
            baseline: decision.baseline,
            shared_with: None,
        })
    }

    fn capture_adjacent_isolated_checks(&self, captured: &mut Captured) {
        let mut remaining = ISOLATED_CHECK_BATCH - 1;
        for decision in self.decisions.clone().filter(|d| d.reason.selected()) {
            if self.captured.contains_key(decision.unit.target())
                || captured.contains_key(decision.unit.target())
            {
                continue;
            }
            if remaining == 0 || !isolated_check(decision) {
                break;
            }
            remaining -= 1;
            let result = self.capture(decision, captured);
            let failed = result.is_err();
            captured.insert(decision.unit.target().clone(), result);
            if failed {
                break;
            }
        }
        tracing::debug!(
            isolated_checks = ISOLATED_CHECK_BATCH - remaining,
            "captured adjacent isolated check batch"
        );
    }
}

impl Iterator for Observations<'_, '_> {
    type Item = Result<Invocation, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let decision = self.decisions.find(|decision| decision.reason.selected())?;
        if let Some(captured) = self.captured.remove(decision.unit.target()) {
            self.failed = captured.is_err();
            return Some(captured);
        }
        let result = (|| {
            self.selection.plan.require_current()?;
            let mut captured = BTreeMap::new();
            let first = self.capture(decision, &mut captured)?;
            if isolated_check(decision) {
                self.capture_adjacent_isolated_checks(&mut captured);
            }
            // No projection escapes if a callback invalidated even an excluded sibling.
            self.selection.plan.require_current()?;
            self.captured.extend(captured);
            Ok(first)
        })();
        if result.is_err() {
            self.failed = true;
        }
        Some(result)
    }
}

impl std::iter::FusedIterator for Observations<'_, '_> {}

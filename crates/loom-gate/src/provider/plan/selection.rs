//! Selection is a read-only projection; execution always retains the complete admitted closure.

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
    Affected { inputs: Vec<Input> },
    Unaffected,
    OtherTarget,
    EmptyFiniteScope,
    OtherLane,
    DeferredUntilPublication,
}

impl Reason {
    pub const fn selected(&self) -> bool {
        matches!(
            self,
            Self::WholeTree | Self::ExactTarget | Self::Affected { .. }
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
        let stage = if scope.target().is_some() {
            Stage::Diagnostic
        } else {
            stage
        };
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
                    && tiers.contains(&native_tier(unit)))
                .then(|| native_tier(unit));
                let reason = if owners.is_empty() && baseline.is_none() {
                    if lane_owners.is_empty() {
                        Reason::OtherLane
                    } else {
                        Reason::DeferredUntilPublication
                    }
                } else if scope.is_tree() {
                    Reason::WholeTree
                } else if let Some(target) = scope.target() {
                    if target == unit.target().as_str() {
                        Reason::ExactTarget
                    } else {
                        Reason::OtherTarget
                    }
                } else if let Some(files) = scope.files() {
                    if files.is_empty() {
                        Reason::EmptyFiniteScope
                    } else {
                        let inputs: Vec<_> = unit
                            .inputs()
                            .iter()
                            .filter(|input| {
                                files.iter().any(|file| {
                                    let path =
                                        file.strip_prefix(self.snapshot().root()).unwrap_or(file);
                                    path == input.path().as_path()
                                })
                            })
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
    if !execution_inputs(left).eq(execution_inputs(right)) {
        return false;
    }
    match (&left.definition.execution, &right.definition.execution) {
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
        ) => left.identity() == right.identity(),
        (Execution::NativePlatform(left), Execution::NativePlatform(right)) => {
            left.same_operation(right)
        }
        (Execution::Effect(_), Execution::Effect(_)) => matches!(
            (left.description(), right.description()),
            (Candidate::SupportedEffect { execution: left, .. }, Candidate::SupportedEffect { execution: right, .. }) if left == right
        ),
        _ => false,
    }
}

fn native_tier(unit: &Unit) -> Tier {
    unit.baseline_tier()
}

impl<'a> Selection<'a> {
    pub(super) fn require_current(&self) -> Result<(), Error> {
        self.plan.require_current()
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

    /// Stream fresh, current-run-shared observations without losing earlier
    /// failures when a later execution reports infrastructure failure.
    ///
    /// # Errors
    /// Rejects stale context before the first callback, including an empty selection.
    pub fn invocations(&self) -> Result<Invocations<'_, '_>, Error> {
        self.plan.require_current()?;
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

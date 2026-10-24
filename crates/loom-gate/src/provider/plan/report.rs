//! Serializable observations of an admitted plan, never reusable evidence or authority.

use serde::Serialize;

use super::selection::{Invocation, Reason, Selection, Stage};
use crate::annotation::{Annotation, Tier};
use crate::inventory::Name;
use crate::provider::{Candidate, Error, Unit};
use crate::scope::Resolved;

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Execution {
    Excluded,
    FreshRequired,
    SharedInRun { target: Name },
}

#[derive(Serialize)]
pub struct Decision<'a> {
    definition: &'a Candidate,
    reason: &'a Reason,
    owners: &'a [Annotation],
    baseline: Option<Tier>,
    execution: Execution,
}

/// Inputs and origins describe complete analysis, not only affected selection inputs.
#[derive(Serialize)]
pub struct Explanation<'a> {
    stage: Stage,
    scope: &'a Resolved,
    decisions: Vec<Decision<'a>>,
}

/// Compact selection diagnostics omit repeated definitions, never obligations.
#[derive(Serialize)]
pub struct Summary<'a> {
    stage: Stage,
    scope: &'a Resolved,
    units: usize,
    selected_units: usize,
    shared_units: usize,
    executions: Vec<ExecutionSummary<'a>>,
    exclusions: Vec<Exclusion<'a>>,
}

#[derive(Serialize)]
struct ExecutionSummary<'a> {
    target: &'a Name,
    owners: usize,
    aliases: usize,
    lanes: Vec<Lane>,
    reasons: Vec<&'a Reason>,
}

#[derive(Serialize)]
struct Lane {
    tier: Tier,
    owners: usize,
    baselines: usize,
}

impl ExecutionSummary<'_> {
    fn include_lanes(&mut self, decision: &Decision<'_>) {
        for tier in [Tier::Check, Tier::Test, Tier::System] {
            let owners = decision
                .owners
                .iter()
                .filter(|owner| owner.tier == tier)
                .count();
            let baselines = usize::from(decision.baseline == Some(tier));
            if owners + baselines == 0 {
                continue;
            }
            if let Some(lane) = self.lanes.iter_mut().find(|lane| lane.tier == tier) {
                lane.owners += owners;
                lane.baselines += baselines;
            } else {
                self.lanes.push(Lane {
                    tier,
                    owners,
                    baselines,
                });
            }
        }
    }
}

#[derive(Serialize)]
struct Exclusion<'a> {
    reason: &'a Reason,
    units: usize,
}

impl Explanation<'_> {
    pub fn summary(&self) -> Summary<'_> {
        let mut executions: Vec<ExecutionSummary<'_>> = Vec::new();
        let mut exclusions: Vec<Exclusion<'_>> = Vec::new();
        let mut selected_units = 0;
        let mut shared_units = 0;
        for decision in &self.decisions {
            match &decision.execution {
                Execution::Excluded => {
                    if let Some(exclusion) = exclusions
                        .iter_mut()
                        .find(|entry| entry.reason == decision.reason)
                    {
                        exclusion.units += 1;
                    } else {
                        exclusions.push(Exclusion {
                            reason: decision.reason,
                            units: 1,
                        });
                    }
                }
                Execution::FreshRequired => {
                    selected_units += 1;
                    let mut execution = ExecutionSummary {
                        target: decision.definition.target(),
                        owners: decision.owners.len(),
                        aliases: 0,
                        lanes: Vec::new(),
                        reasons: vec![decision.reason],
                    };
                    execution.include_lanes(decision);
                    executions.push(execution);
                }
                Execution::SharedInRun { target } => {
                    selected_units += 1;
                    shared_units += 1;
                    if let Some(execution) =
                        executions.iter_mut().find(|entry| entry.target == target)
                    {
                        execution.owners += decision.owners.len();
                        execution.aliases += 1;
                        execution.include_lanes(decision);
                        if !execution.reasons.contains(&decision.reason) {
                            execution.reasons.push(decision.reason);
                        }
                    }
                }
            }
        }
        Summary {
            stage: self.stage,
            scope: self.scope,
            units: self.decisions.len(),
            selected_units,
            shared_units,
            executions,
            exclusions,
        }
    }
}

/// Counts describe actual calls in this run; shared aliases are not cache hits.
#[derive(Serialize)]
pub struct Completed<'a> {
    plan: Explanation<'a>,
    invocations: Vec<Invocation>,
    fresh_invocations: usize,
    shared_invocations: usize,
}

impl Completed<'_> {
    pub fn invocations(&self) -> &[Invocation] {
        &self.invocations
    }

    pub const fn fresh_invocations(&self) -> usize {
        self.fresh_invocations
    }

    pub const fn shared_invocations(&self) -> usize {
        self.shared_invocations
    }
}

impl Selection<'_> {
    /// Explain selected and excluded siblings without executing verifier callbacks.
    /// Prior results are not supplied to this API, so selected calls require fresh execution.
    ///
    /// # Errors
    /// Rejects a changed admitted context, including excluded siblings.
    pub fn explain(&self) -> Result<Explanation<'_>, Error> {
        self.require_current()?;
        let mut selected: Vec<&Unit> = Vec::new();
        let mut decisions = Vec::new();
        for decision in self.decisions() {
            let execution = if !decision.reason().selected() {
                Execution::Excluded
            } else if let Some(previous) = selected
                .iter()
                .find(|previous| super::selection::same_execution(previous, decision.unit()))
            {
                Execution::SharedInRun {
                    target: previous.target().clone(),
                }
            } else {
                selected.push(decision.unit());
                Execution::FreshRequired
            };
            decisions.push(Decision {
                definition: decision.unit().description(),
                reason: decision.reason(),
                owners: decision.owners(),
                baseline: decision.baseline_tier(),
                execution,
            });
        }
        Ok(Explanation {
            stage: self.stage(),
            scope: self.scope(),
            decisions,
        })
    }

    /// Execute once and retain actual durations, per-owner results and sharing counts.
    ///
    /// # Errors
    /// A changed context or execution error cannot produce a completed report.
    pub fn run_report(&self) -> Result<Completed<'_>, Error> {
        let plan = self.explain()?;
        let invocations = self.run()?;
        let fresh_invocations = invocations
            .iter()
            .filter(|invocation| invocation.shared_with().is_none())
            .count();
        let shared_invocations = invocations.len() - fresh_invocations;
        Ok(Completed {
            plan,
            invocations,
            fresh_invocations,
            shared_invocations,
        })
    }
}

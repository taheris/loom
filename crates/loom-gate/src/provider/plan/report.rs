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

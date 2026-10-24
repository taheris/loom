//! Inventory-wide admission precedes selection and all execution.

pub mod property;
pub mod report;
pub mod selection;

use std::collections::{BTreeMap, BTreeSet};

use super::tracked::Observed;
use super::{Candidate, Error, Registry, Responsibility, Unit};
use crate::annotation::Tier;
use crate::inventory::snapshot::Snapshot;
use crate::inventory::{Inventory, Name};

/// All current deterministic declarations and independently required duties have checked units.
///
/// This is execution admission, not result evidence or publication authorization.
///
/// ```compile_fail
/// use loom_gate::provider::plan::Plan;
/// let plan: Plan = serde_json::from_str("{}").unwrap();
/// ```
pub struct Plan {
    snapshot: Snapshot,
    units: Vec<Unit>,
    baselines: BTreeSet<Name>,
    pending: Vec<crate::Annotation>,
    deferred: Vec<crate::Annotation>,
}

impl Registry {
    /// Resolve current acceptance declarations and every inventory obligation before dispatch.
    ///
    /// # Errors
    /// Rejects malformed packages, missing registrations, stale definitions and uncovered duties.
    pub fn admit(&self, snapshot: &Snapshot, inventory: &Inventory) -> Result<Plan, Error> {
        super::require_context(snapshot, inventory)?;
        let mut current = super::Current::new(self.definitions.values())?;
        let mut units: BTreeMap<Name, Unit> = self
            .resolve_baselines(snapshot, inventory, &mut current)?
            .into_iter()
            .map(|unit| (unit.target().clone(), unit))
            .collect();
        let baselines: BTreeSet<_> = units.keys().cloned().collect();
        let mut pending = Vec::new();
        let mut deferred = Vec::new();
        for contract in crate::inventory::contracts(snapshot)? {
            let declarations =
                crate::annotation::parse_content(contract.as_path(), snapshot.text(&contract)?)?;
            pending.extend(
                declarations
                    .annotations
                    .iter()
                    .filter(|annotation| annotation.pending)
                    .cloned(),
            );
            for declaration in &declarations.strategy {
                if let crate::annotation::strategy::Declaration::Criterion { line, .. } =
                    declaration
                {
                    deferred.extend(
                        declarations
                            .annotations
                            .iter()
                            .filter(|annotation| annotation.criterion_line == *line)
                            .cloned(),
                    );
                }
            }
            let mut resolved = BTreeSet::new();
            for annotation in declarations
                .annotations
                .into_iter()
                .filter(|annotation| annotation.tier != Tier::Judge)
            {
                let target = Name::try_from(annotation.target)?;
                if (annotation.pending && !self.definitions.contains_key(&target))
                    || !resolved.insert(target.clone())
                {
                    continue;
                }
                let definition = self
                    .definition(&target)
                    .map_err(|source| Error::Admission {
                        target: target.clone(),
                        contract: contract.clone(),
                        source: Box::new(source),
                    })?;
                let unit = self
                    .resolve_current(
                        snapshot,
                        inventory,
                        definition.description(),
                        contract.clone(),
                        &mut current,
                    )
                    .map_err(|source| Error::Admission {
                        target: target.clone(),
                        contract: contract.clone(),
                        source: Box::new(source),
                    })?;
                match units.entry(target) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(unit);
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        merge_contract(entry.get_mut(), unit);
                    }
                }
            }
        }
        let offered: BTreeSet<_> = units
            .values()
            .filter(|unit| {
                baselines.contains(unit.target())
                    || unit.annotations().iter().any(|owner| !owner.pending)
            })
            .flat_map(|unit| unit.responsibilities().iter().cloned())
            .collect();
        require_coverage(inventory, &offered)?;
        snapshot.require_current()?;
        Ok(Plan {
            snapshot: snapshot.clone(),
            units: units.into_values().collect(),
            baselines,
            pending,
            deferred,
        })
    }

    /// Reject already-known independent gaps before compiler collection effects.
    /// This validates constructed offerings only; it does not admit declarations or verdicts.
    ///
    /// # Errors
    /// Rejects stale definitions and uncovered independent obligations.
    pub fn require_inventory_coverage(
        &self,
        snapshot: &Snapshot,
        inventory: &Inventory,
    ) -> Result<(), Error> {
        super::require_context(snapshot, inventory)?;
        let mut current = super::Current::new(self.definitions.values())?;
        let baseline = self.resolve_baselines(snapshot, inventory, &mut current)?;
        let mut offered: BTreeSet<_> = baseline
            .iter()
            .flat_map(|unit| unit.responsibilities().iter().cloned())
            .collect();
        let baselines: BTreeSet<_> = baseline.iter().map(|unit| unit.target().clone()).collect();
        let mut targets = BTreeSet::new();
        for contract in crate::inventory::contracts(snapshot)? {
            let declarations =
                crate::annotation::parse_content(contract.as_path(), snapshot.text(&contract)?)?;
            for annotation in declarations
                .annotations
                .into_iter()
                .filter(|annotation| !annotation.pending && annotation.tier != Tier::Judge)
            {
                targets.insert(Name::try_from(annotation.target)?);
            }
        }
        for target in targets.difference(&baselines) {
            if let Some(definition) = self.definitions.get(target) {
                current.require(definition)?;
                offered.extend(definition.description().responsibilities().iter().cloned());
            }
        }
        require_coverage(inventory, &offered)?;
        snapshot.require_current()?;
        Ok(())
    }

    /// Native constructed checks retain independent responsibilities without optional annotations.
    ///
    /// # Errors
    /// Rejects stale constructors or responsibilities not present in the current inventory.
    fn resolve_baselines(
        &self,
        snapshot: &Snapshot,
        inventory: &Inventory,
        current: &mut super::Current,
    ) -> Result<Vec<Unit>, Error> {
        super::require_context(snapshot, inventory)?;
        let obligations: BTreeSet<_> = inventory
            .obligations()
            .iter()
            .map(Responsibility::from_obligation)
            .collect();
        let mut units = Vec::new();
        for definition in self.definitions.values() {
            if matches!(definition.execution, super::Execution::Report { .. }) {
                continue;
            }
            let candidate = definition.description();
            if !matches!(
                candidate,
                Candidate::HermeticNix { .. }
                    | Candidate::TrackedRust {
                        execution: super::wire::Rust {
                            checker: super::wire::RustChecker::Library { .. },
                            ..
                        },
                        ..
                    }
            ) {
                continue;
            }
            if self.snapshot.identity() != snapshot.identity()
                || candidate.snapshot() != snapshot.identity()
            {
                return Err(Error::Context {
                    target: candidate.target().clone(),
                });
            }
            current.require(definition)?;
            if !candidate.responsibilities().is_subset(&obligations) {
                return Err(Error::Responsibility {
                    target: candidate.target().clone(),
                });
            }
            units.push(Unit {
                snapshot: snapshot.clone(),
                definition: definition.clone(),
                inputs: candidate.inputs().to_vec(),
                annotations: Vec::new(),
            });
        }
        snapshot.require_current()?;
        Ok(units)
    }
}

fn require_coverage(
    inventory: &Inventory,
    offered: &BTreeSet<Responsibility>,
) -> Result<(), Error> {
    for obligation in inventory.obligations() {
        if !offered.contains(&Responsibility::from_obligation(obligation)) {
            return Err(Error::Coverage {
                subject: obligation.subject().clone(),
                duty: obligation.duty(),
                responsible: obligation.responsible().clone(),
                producer: inventory.subject(obligation.subject())?.origin().clone(),
            });
        }
    }
    Ok(())
}

pub(super) fn merge_contract(existing: &mut Unit, unit: Unit) {
    for input in unit.inputs {
        if let Some(current) = existing
            .inputs
            .iter_mut()
            .find(|current| current.path == input.path)
        {
            current.origins.extend(input.origins);
        } else {
            existing.inputs.push(input);
        }
    }
    existing
        .inputs
        .sort_by(|left, right| left.path.cmp(&right.path));
    for annotation in unit.annotations {
        if !existing.annotations.contains(&annotation) {
            existing.annotations.push(annotation);
        }
    }
}

impl Plan {
    pub fn units(&self) -> &[Unit] {
        &self.units
    }

    pub fn pending(&self) -> &[crate::Annotation] {
        &self.pending
    }

    /// Run only after rechecking every admitted sibling, retaining each observed outcome.
    ///
    /// # Errors
    /// Any invalidated definition rejects the plan before the first unit executes.
    pub fn run(&self) -> Result<Vec<Observed>, Error> {
        self.run_scoped(&crate::scope::Resolved::whole_tree())
    }

    /// Apply the ordinary resolved scope to admitted inputs before dispatching selected units.
    /// Every inventory obligation is admitted before selection, including unselected siblings.
    ///
    /// # Errors
    /// Rejects stale context before the first effect; no scope authorizes an invalid sibling.
    pub fn run_scoped(&self, scope: &crate::scope::Resolved) -> Result<Vec<Observed>, Error> {
        Ok(self
            .select(scope)?
            .run()?
            .into_iter()
            .map(selection::Invocation::into_observed)
            .collect())
    }

    /// Captured source data; inspecting it is not an execution or publication grant.
    pub const fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// Revalidate every sibling before any caller starts effects.
    ///
    /// # Errors
    /// Rejects stale source, inventory or executable context even outside the selected scope.
    pub fn require_current(&self) -> Result<(), Error> {
        self.snapshot.require_current()?;
        let mut current = super::Current::new(self.units.iter().map(|unit| &unit.definition))?;
        for unit in &self.units {
            current.require(&unit.definition)?;
        }
        self.snapshot.require_current()?;
        Ok(())
    }
}

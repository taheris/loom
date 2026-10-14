//! Inventory-wide admission precedes selection and all execution.

use std::collections::{BTreeMap, BTreeSet};

use super::tracked::Observed;
use super::{Error, Registry, Responsibility, Unit};
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
}

impl Registry {
    /// Resolve current acceptance declarations and every inventory obligation before dispatch.
    ///
    /// # Errors
    /// Rejects malformed packages, missing registrations, stale definitions and uncovered duties.
    pub fn admit(&self, snapshot: &Snapshot, inventory: &Inventory) -> Result<Plan, Error> {
        super::require_context(snapshot, inventory)?;
        let mut units = BTreeMap::<Name, Unit>::new();
        for contract in crate::inventory::contracts(snapshot)? {
            let declarations =
                crate::annotation::parse_content(contract.as_path(), snapshot.text(&contract)?)?;
            for annotation in declarations
                .annotations
                .into_iter()
                .filter(|annotation| annotation.tier != Tier::Judge)
            {
                let target = Name::try_from(annotation.target)?;
                let definition = self
                    .definition(&target)
                    .map_err(|source| Error::Admission {
                        target: target.clone(),
                        contract: contract.clone(),
                        source: Box::new(source),
                    })?;
                let unit = self
                    .resolve(
                        snapshot,
                        inventory,
                        definition.description(),
                        contract.clone(),
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
            .flat_map(|unit| unit.responsibilities().iter().cloned())
            .collect();
        for obligation in inventory.obligations() {
            let responsibility = Responsibility::from_obligation(obligation);
            if !offered.contains(&responsibility) {
                return Err(Error::Coverage {
                    subject: obligation.subject().clone(),
                    duty: obligation.duty(),
                    responsible: obligation.responsible().clone(),
                    producer: inventory.subject(obligation.subject())?.origin().clone(),
                });
            }
        }
        snapshot.require_current()?;
        Ok(Plan {
            snapshot: snapshot.clone(),
            units: units.into_values().collect(),
        })
    }
}

fn merge_contract(existing: &mut Unit, unit: Unit) {
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

    /// Run only after rechecking every admitted sibling, retaining each observed outcome.
    ///
    /// # Errors
    /// Any invalidated definition rejects the plan before the first unit executes.
    pub fn run(&self) -> Result<Vec<Observed>, Error> {
        self.snapshot.require_current()?;
        for unit in &self.units {
            unit.require_current()?;
        }
        self.units.iter().map(Unit::run).collect()
    }
}

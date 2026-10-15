//! Captured reads and membership observations for registered Rust analysis.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::{Capability, Digest, Error, Input, Origin};
use crate::inventory::Name;
use crate::inventory::snapshot::{ResourcePath, Snapshot};

pub type Check = dyn for<'a> Fn(&mut Context<'a>) -> Result<Outcome, Error> + Send + Sync;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", rename_all = "kebab-case")]
pub enum Outcome {
    Pass,
    Fail {
        detail: String,
    },
    Unavailable {
        capability: Capability,
        detail: String,
    },
    Inapplicable {
        system: Name,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Observation {
    Content {
        path: ResourcePath,
        digest: Digest,
    },
    Existence {
        path: ResourcePath,
        present: bool,
    },
    Membership {
        subject: Name,
        paths: BTreeSet<ResourcePath>,
    },
    ExecutionGraph {
        digest: Digest,
    },
    Output {
        path: super::nix::StorePath,
    },
}

/// An observed result is not evidence admission or publication authorization.
#[derive(Debug, Serialize)]
pub struct Observed {
    target: Name,
    outcome: Outcome,
    observations: Vec<Observation>,
}

impl Observed {
    pub(super) const fn execution(
        target: Name,
        outcome: Outcome,
        observations: Vec<Observation>,
    ) -> Self {
        Self {
            target,
            outcome,
            observations,
        }
    }

    pub const fn target(&self) -> &Name {
        &self.target
    }

    pub const fn outcome(&self) -> &Outcome {
        &self.outcome
    }

    pub const fn observations(&self) -> &[Observation] {
        self.observations.as_slice()
    }
}

/// Checks receive captured input access, not a filesystem root or caller-selected file subset.
pub struct Context<'a> {
    snapshot: &'a Snapshot,
    inputs: BTreeSet<&'a ResourcePath>,
    members: &'a BTreeMap<Name, BTreeSet<ResourcePath>>,
    observations: Vec<Observation>,
    violation: Option<Violation>,
}

enum Violation {
    Outside(ResourcePath),
    Subject(Name),
}

impl<'a> Context<'a> {
    pub(super) fn new(
        snapshot: &'a Snapshot,
        inputs: &'a [Input],
        members: &'a BTreeMap<Name, BTreeSet<ResourcePath>>,
    ) -> Self {
        Self {
            snapshot,
            inputs: inputs
                .iter()
                .filter(|input| {
                    input.origins().iter().any(|origin| {
                        matches!(origin, Origin::Subject(subject) if members.contains_key(subject))
                    })
                })
                .map(Input::path)
                .collect(),
            members,
            observations: Vec::new(),
            violation: None,
        }
    }

    /// Analysis subjects available to this check, excluding checker-build dependencies.
    pub fn subjects(&self) -> impl Iterator<Item = &Name> {
        self.members.keys()
    }

    /// Captured analysis paths, including their native definition origins.
    pub fn paths(&self) -> impl Iterator<Item = &ResourcePath> {
        self.inputs.iter().copied()
    }

    /// Read bytes only from the resolved analysis closure.
    ///
    /// # Errors
    /// Outside reads poison the run even if checker code ignores the returned error.
    pub fn read(&mut self, path: &ResourcePath) -> Result<&'a [u8], Error> {
        self.require_input(path)?;
        let bytes = self
            .snapshot
            .files()
            .get(path)
            .ok_or_else(|| crate::inventory::snapshot::Error::Missing { path: path.clone() })?;
        self.record(Observation::Content {
            path: path.clone(),
            digest: Digest::of(bytes),
        });
        Ok(bytes)
    }

    /// Text uses the same tracked read boundary as bytes.
    ///
    /// # Errors
    /// Non-UTF-8 content is not silently decoded or replaced.
    pub fn text(&mut self, path: &ResourcePath) -> Result<&'a str, Error> {
        std::str::from_utf8(self.read(path)?).map_err(|_| Error::Encoding { path: path.clone() })
    }

    /// Observe absence as well as presence; snapshot membership binds either result.
    ///
    /// # Errors
    /// Existing resources outside the analysis closure cannot be probed.
    pub fn exists(&mut self, path: &ResourcePath) -> Result<bool, Error> {
        self.require_input(path)?;
        let present = self.snapshot.files().contains_key(path);
        self.record(Observation::Existence {
            path: path.clone(),
            present,
        });
        Ok(present)
    }

    /// Obtain the complete current membership of a registered analysis subject.
    ///
    /// # Errors
    /// Unknown analysis subjects poison the run, rather than yielding an empty scan.
    pub fn members(&mut self, subject: &Name) -> Result<&'a BTreeSet<ResourcePath>, Error> {
        let Some(paths) = self.members.get(subject) else {
            self.violation = Some(Violation::Subject(subject.clone()));
            return Err(Error::AnalysisSubject {
                subject: subject.clone(),
            });
        };
        self.record(Observation::Membership {
            subject: subject.clone(),
            paths: paths.clone(),
        });
        Ok(paths)
    }

    fn require_input(&mut self, path: &ResourcePath) -> Result<(), Error> {
        if self.snapshot.files().contains_key(path) && !self.inputs.contains(path) {
            self.violation = Some(Violation::Outside(path.clone()));
            return Err(Error::Outside { path: path.clone() });
        }
        Ok(())
    }

    fn record(&mut self, observation: Observation) {
        if !self.observations.contains(&observation) {
            self.observations.push(observation);
        }
    }

    pub(super) fn finish(self, target: Name, outcome: Outcome) -> Result<Observed, Error> {
        match self.violation {
            Some(Violation::Outside(path)) => Err(Error::Outside { path }),
            Some(Violation::Subject(subject)) => Err(Error::AnalysisSubject { subject }),
            None => Ok(Observed {
                target,
                outcome,
                observations: self.observations,
            }),
        }
    }
}

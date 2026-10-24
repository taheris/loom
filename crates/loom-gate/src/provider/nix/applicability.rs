//! Read-only native platform classification; foreign recipes are never execution registrations.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{Error, Platforms, StorePath, locate_tool, query};
use crate::inventory::snapshot::{ResourcePath, Snapshot};
use crate::inventory::{Inventory, Name};
use crate::provider::tracked::{Observation, Observed, Outcome};
use crate::provider::{Candidate, Definition, Digest, Execution, Origin, Responsibility, wire};

/// Actual backend configuration, not a caller-supplied applicability or approval flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Description {
    system: Name,
    supported: BTreeSet<Name>,
    executor: StorePath,
    binary: ResourcePath,
    artifact: Digest,
}

pub(crate) struct Applicability {
    description: Description,
    tool: PathBuf,
    executable: PathBuf,
    artifact: Digest,
}

fn platforms(tool: &std::path::Path) -> Result<BTreeSet<Name>, Error> {
    let platform: Platforms = serde_json::from_slice(&query(tool, &["config", "show", "--json"])?)?;
    let mut supported = platform.extra.value;
    supported.insert(platform.system.value);
    Ok(supported)
}

impl Definition {
    /// Whether this fixed metadata operation accounts for a native output's platform applicability.
    pub fn native_applicability_covers(&self, native: &Name) -> bool {
        matches!(self.execution, Execution::NativePlatform(_))
            && self
                .description()
                .responsibilities()
                .iter()
                .any(|responsibility| responsibility.subject() == native)
    }

    pub(super) fn native_inapplicable(
        snapshot: &Snapshot,
        inventory: &Inventory,
        target: Name,
        native: &Name,
    ) -> Result<Option<Self>, crate::provider::Error> {
        let subject = inventory.subject(native)?;
        let binding = subject.nix_target().ok_or_else(|| Error::Native {
            subject: native.clone(),
        })?;
        let tool = locate_tool()?;
        let supported = platforms(&tool)?;
        if supported.contains(binding.system()) {
            return Ok(None);
        }
        let executor = StorePath::root(&tool)?;
        let description = Description {
            system: binding.system().clone(),
            supported,
            binary: ResourcePath::try_from(
                tool.strip_prefix(executor.as_str())
                    .map_err(|_| Error::StorePath)?
                    .to_path_buf(),
            )?,
            executor,
            artifact: Digest::of(&std::fs::read(&tool).map_err(Error::Io)?),
        };
        let executable = std::env::current_exe().map_err(crate::provider::Error::Executable)?;
        let artifact = crate::provider::read_artifact(&executable)?;
        let operation = Arc::new(Applicability {
            description: description.clone(),
            tool,
            executable,
            artifact: artifact.clone(),
        });
        let mut owners: BTreeSet<_> = inventory
            .subjects()
            .values()
            .filter(|subject| {
                subject.nix_target().is_some_and(|other| {
                    other.system() == binding.system() && other.flake() == binding.flake()
                })
            })
            .map(|subject| subject.name().clone())
            .collect();
        let applicable = inventory.subjects().values().any(|subject| {
            subject.nix_target().is_some_and(|other| {
                other.flake() == binding.flake() && description.supported.contains(other.system())
            })
        });
        if !applicable {
            // A wholly foreign program retains unverified source duties too. An
            // applicable consumer must always supply its own checked construction.
            let members: BTreeSet<_> = inventory
                .subjects()
                .values()
                .filter(|subject| {
                    subject.nix_target().is_none()
                        && subject.role() == crate::inventory::Role::NixSource
                        && subject.origin() == binding.flake()
                })
                .flat_map(|subject| subject.members().iter().cloned())
                .collect();
            owners.extend(
                inventory
                    .subjects()
                    .values()
                    .filter(|subject| {
                        subject.nix_target().is_none()
                            && subject.role() == crate::inventory::Role::NixSource
                            && !subject.members().is_empty()
                            && subject.members().is_subset(&members)
                    })
                    .map(|subject| subject.name().clone()),
            );
        }
        let responsibilities: BTreeSet<_> = inventory
            .obligations()
            .iter()
            .filter(|obligation| {
                owners.contains(obligation.responsible())
                    && matches!(
                        obligation.duty(),
                        crate::inventory::Duty::Evaluate | crate::inventory::Duty::Check
                    )
            })
            .map(Responsibility::from_obligation)
            .collect();
        let directory = binding.flake().as_path().parent().ok_or(Error::StorePath)?;
        let mut inputs: BTreeMap<_, _> = snapshot
            .files()
            .keys()
            .filter(|path| {
                path.as_path().starts_with(directory)
                    && path
                        .as_path()
                        .extension()
                        .is_some_and(|extension| extension == "nix")
            })
            .map(|path| {
                (
                    path.clone(),
                    BTreeSet::from([Origin::Definition(binding.flake().clone())]),
                )
            })
            .collect();
        for responsibility in &responsibilities {
            for path in inventory.subject(responsibility.subject())?.members() {
                inputs
                    .entry(path.clone())
                    .or_default()
                    .insert(Origin::Subject(responsibility.subject().clone()));
            }
        }
        let inputs = crate::provider::capture_inputs(snapshot, inputs)?;
        let checker = wire::RustChecker::Library {
            library: wire::LibraryChecker::NativePlatform(Box::new(description)),
            source: Digest::of(include_bytes!("applicability.rs")),
        };
        let entry = Name::try_from(
            "loom_gate::provider::nix::applicability::Applicability::observe".to_owned(),
        )?;
        let identity = Digest::of(&serde_json::to_vec(&(
            snapshot.identity(),
            &target,
            &checker,
            &responsibilities,
            &inputs,
            &artifact,
            &entry,
        ))?);
        Ok(Some(Self {
            candidate: Candidate::TrackedRust {
                contract: wire::Contract {
                    target,
                    snapshot: snapshot.identity().clone(),
                    definition: identity,
                    origin: binding.flake().clone(),
                    responsibilities,
                    inputs,
                },
                execution: wire::Rust {
                    checker,
                    artifact,
                    entry,
                },
            },
            execution: Execution::NativePlatform(operation),
        }))
    }
}

impl Applicability {
    pub(crate) fn require_current(&self) -> Result<(), crate::provider::Error> {
        if locate_tool()? != self.tool
            || Digest::of(&std::fs::read(&self.tool).map_err(Error::Io)?)
                != self.description.artifact
            || crate::provider::read_artifact(&self.executable)? != self.artifact
            || platforms(&self.tool)? != self.description.supported
        {
            return Err(Error::GraphChanged.into());
        }
        Ok(())
    }

    pub(crate) fn observe(&self, target: Name) -> Result<Observed, crate::provider::Error> {
        self.require_current()?;
        Ok(Observed::execution(
            target,
            Outcome::Inapplicable {
                system: self.description.system.clone(),
            },
            vec![Observation::NativePlatform {
                system: self.description.system.clone(),
                supported: self.description.supported.clone(),
            }],
        ))
    }

    pub(crate) fn same_operation(&self, other: &Self) -> bool {
        self.description == other.description
            && self.executable == other.executable
            && self.artifact == other.artifact
    }
}

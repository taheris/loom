//! Checked definitions produce descriptions; only current-context resolution produces units.

pub mod nix;
pub mod tracked;
mod wire;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use displaydoc::Display;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::inventory::snapshot::{Identity, ResourcePath, Snapshot};
use crate::inventory::{Duty, Inventory, Name, Obligation, Role};
use tracked::{Check, Context, Observed, Outcome};
pub use wire::{Candidate, Capability, Digest, Discovery, Effect, Mapping};

/// Offered responsibility must join an independently discovered obligation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Responsibility {
    subject: Name,
    duty: Duty,
    responsible: Name,
}

impl Responsibility {
    fn from_obligation(obligation: &Obligation) -> Self {
        Self {
            subject: obligation.subject().clone(),
            duty: obligation.duty(),
            responsible: obligation.responsible().clone(),
        }
    }

    pub const fn subject(&self) -> &Name {
        &self.subject
    }

    pub const fn duty(&self) -> Duty {
        self.duty
    }

    pub const fn responsible(&self) -> &Name {
        &self.responsible
    }
}

/// Source provenance is retained even when several subjects share a file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "reference",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum Origin {
    Subject(Name),
    Definition(ResourcePath),
    Contract(ResourcePath),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    path: ResourcePath,
    content: Digest,
    #[serde(deserialize_with = "parse_set")]
    origins: BTreeSet<Origin>,
}

impl Input {
    pub const fn path(&self) -> &ResourcePath {
        &self.path
    }

    pub const fn content(&self) -> &Digest {
        &self.content
    }

    pub const fn origins(&self) -> &BTreeSet<Origin> {
        &self.origins
    }
}

/// A registration binds the callable, source identity, responsibility and tracked scope together.
#[derive(Clone)]
pub struct Definition {
    candidate: Candidate,
    execution: Execution,
}

#[derive(Clone)]
enum Execution {
    Tracked {
        check: Arc<Check>,
        members: BTreeMap<Name, BTreeSet<ResourcePath>>,
        executable: PathBuf,
        artifact: Digest,
    },
    Nix(Arc<nix::Graph>),
}

impl Definition {
    /// Construct a tracked check from current project subjects, not a copied file-input table.
    ///
    /// # Errors
    /// Rejects stale context, unknown subjects, absent duties, or a checker without Rust ownership.
    pub fn tracked<F>(
        snapshot: &Snapshot,
        inventory: &Inventory,
        target: Name,
        checker: ResourcePath,
        subjects: &BTreeSet<Name>,
        duties: &BTreeSet<Duty>,
        check: F,
    ) -> Result<Self, Error>
    where
        F: for<'a> Fn(&mut Context<'a>) -> Result<Outcome, Error> + Send + Sync + 'static,
    {
        require_context(snapshot, inventory)?;
        let callable = std::any::type_name::<F>();
        if std::mem::size_of::<F>() != 0
            || callable.contains("{{closure}}")
            || callable.starts_with("fn(")
        {
            return Err(Error::Callable { target });
        }
        let entry = Name::try_from(callable.to_owned())?;
        if !inventory.subjects().values().any(|subject| {
            subject.role() == Role::RustSource && subject.members().contains(&checker)
        }) {
            return Err(Error::Checker { target, checker });
        }
        let mut members = BTreeMap::new();
        let mut inputs = BTreeMap::<ResourcePath, BTreeSet<Origin>>::new();
        for name in subjects {
            let subject = inventory
                .subject(name)
                .map_err(|source| Error::Construction {
                    target: target.clone(),
                    producer: checker.clone(),
                    source: Box::new(source.into()),
                })?;
            members.insert(name.clone(), subject.members().clone());
            add_subject_inputs(&mut inputs, subject);
        }
        let responsibilities = responsibility(inventory, &target, subjects, duties)?;
        for subject in inventory
            .subjects()
            .values()
            .filter(|subject| matches!(subject.role(), Role::RustSource | Role::Configuration))
        {
            add_subject_inputs(&mut inputs, subject);
        }
        inputs
            .entry(checker.clone())
            .or_default()
            .insert(Origin::Definition(checker.clone()));
        let inputs = capture_inputs(snapshot, inputs)?;
        let executable = std::env::current_exe().map_err(Error::Executable)?;
        let artifact = read_artifact(&executable)?;
        let definition = Digest::of(&serde_json::to_vec(&(
            snapshot.identity(),
            &target,
            &checker,
            &responsibilities,
            &inputs,
            &artifact,
            &entry,
        ))?);
        Ok(Self {
            candidate: Candidate::TrackedRust {
                contract: wire::Contract {
                    target,
                    snapshot: snapshot.identity().clone(),
                    definition,
                    origin: checker.clone(),
                    responsibilities,
                    inputs,
                },
                execution: wire::Rust {
                    checker,
                    artifact: artifact.clone(),
                    entry,
                },
            },
            execution: Execution::Tracked {
                check: Arc::new(check),
                members,
                executable,
                artifact,
            },
        })
    }

    pub const fn description(&self) -> &Candidate {
        &self.candidate
    }

    fn require_current(&self) -> Result<(), Error> {
        match &self.execution {
            Execution::Tracked {
                executable,
                artifact,
                ..
            } => {
                if read_artifact(executable)? != *artifact {
                    return Err(Error::BuildChanged {
                        target: self.candidate.target().clone(),
                    });
                }
            }
            Execution::Nix(graph) => graph.require_current()?,
        }
        Ok(())
    }
}

fn responsibility(
    inventory: &Inventory,
    target: &Name,
    subjects: &BTreeSet<Name>,
    duties: &BTreeSet<Duty>,
) -> Result<BTreeSet<Responsibility>, Error> {
    let responsibilities: BTreeSet<_> = inventory
        .obligations()
        .iter()
        .filter(|obligation| {
            subjects.contains(obligation.subject()) && duties.contains(&obligation.duty())
        })
        .map(Responsibility::from_obligation)
        .collect();
    if subjects.is_empty()
        || duties.is_empty()
        || subjects.iter().any(|name| {
            !responsibilities
                .iter()
                .any(|offered| &offered.subject == name)
        })
        || duties
            .iter()
            .any(|duty| !responsibilities.iter().any(|offered| &offered.duty == duty))
    {
        return Err(Error::Responsibility {
            target: target.clone(),
        });
    }
    Ok(responsibilities)
}

fn parse_set<'de, D, T>(deserializer: D) -> Result<BTreeSet<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Ord,
{
    let values = Vec::<T>::deserialize(deserializer)?;
    let mut unique = BTreeSet::new();
    for value in values {
        if !unique.insert(value) {
            return Err(serde::de::Error::custom(
                "duplicate provider contract entry",
            ));
        }
    }
    Ok(unique)
}

fn add_subject_inputs(
    inputs: &mut BTreeMap<ResourcePath, BTreeSet<Origin>>,
    subject: &crate::inventory::Subject,
) {
    for path in subject
        .members()
        .iter()
        .chain(std::iter::once(subject.origin()))
    {
        inputs
            .entry(path.clone())
            .or_default()
            .insert(Origin::Subject(subject.name().clone()));
    }
}

fn capture_inputs(
    snapshot: &Snapshot,
    inputs: BTreeMap<ResourcePath, BTreeSet<Origin>>,
) -> Result<Vec<Input>, Error> {
    inputs
        .into_iter()
        .map(|(path, origins)| {
            let bytes = snapshot
                .files()
                .get(&path)
                .ok_or_else(|| crate::inventory::snapshot::Error::Missing { path: path.clone() })?;
            Ok(Input {
                path,
                content: Digest::of(bytes),
                origins,
            })
        })
        .collect()
}

/// The registry owns actual definitions; query JSON cannot add registrations.
pub struct Registry {
    snapshot: Identity,
    definitions: BTreeMap<Name, Definition>,
}

impl Registry {
    /// Reject duplicate targets and registrations from a different captured project.
    ///
    /// # Errors
    /// Duplicate or stale definitions cannot be silently replaced.
    pub fn new(snapshot: &Snapshot, definitions: Vec<Definition>) -> Result<Self, Error> {
        snapshot.require_current()?;
        let mut checked = BTreeMap::new();
        for definition in definitions {
            let candidate = definition.description();
            let target = candidate.target().clone();
            if candidate.snapshot() != snapshot.identity() {
                return Err(Error::Context { target });
            }
            if checked.insert(target.clone(), definition).is_some() {
                return Err(Error::Duplicate { target });
            }
        }
        Ok(Self {
            snapshot: snapshot.identity().clone(),
            definitions: checked,
        })
    }

    /// Generate a single or batched typed description from the registered execution definition.
    ///
    /// # Errors
    /// Unknown or repeated requests fail; no target is omitted from a successful response.
    pub fn query(&self, targets: &[Name]) -> Result<String, Error> {
        let mut seen = BTreeSet::new();
        let mut units = Vec::new();
        for target in targets {
            if !seen.insert(target) {
                return Err(Error::Duplicate {
                    target: target.clone(),
                });
            }
            units.push(self.definition(target)?.description());
        }
        Ok(serde_json::to_string(&wire::Query { units })?)
    }

    /// Resolve every requested target before returning any executable units.
    ///
    /// # Errors
    /// Malformed, duplicate, extra or missing responses fail without partial dispatch.
    pub fn resolve_query(
        &self,
        snapshot: &Snapshot,
        inventory: &Inventory,
        json: &str,
        contracts: &BTreeMap<Name, ResourcePath>,
    ) -> Result<Vec<Unit>, Error> {
        let query: wire::Response = serde_json::from_str(json)?;
        let mut seen = BTreeSet::new();
        let mut units = Vec::new();
        for candidate in query.units {
            let target = candidate.target();
            if !seen.insert(target.clone()) {
                return Err(Error::Duplicate {
                    target: target.clone(),
                });
            }
            let contract = contracts.get(target).ok_or_else(|| Error::Unexpected {
                target: target.clone(),
            })?;
            units.push(
                self.resolve(snapshot, inventory, &candidate, contract.clone())
                    .map_err(|source| Error::Admission {
                        target: target.clone(),
                        contract: contract.clone(),
                        source: Box::new(source),
                    })?,
            );
        }
        for target in contracts.keys() {
            if !seen.contains(target) {
                return Err(Error::MissingResponse {
                    target: target.clone(),
                });
            }
        }
        Ok(units)
    }

    /// Resolve a parsed description against the actual definition and current inventory.
    ///
    /// # Errors
    /// Stale, missing or inconsistent contracts never become executable units.
    pub fn resolve(
        &self,
        snapshot: &Snapshot,
        inventory: &Inventory,
        candidate: &Candidate,
        contract: ResourcePath,
    ) -> Result<Unit, Error> {
        require_context(snapshot, inventory)?;
        let target = candidate.target();
        if &self.snapshot != snapshot.identity() || candidate.snapshot() != snapshot.identity() {
            return Err(Error::Context {
                target: target.clone(),
            });
        }
        let definition = self.definition(target)?;
        definition.require_current()?;
        if candidate != definition.description() {
            return Err(Error::Inconsistent {
                target: target.clone(),
                producer: definition.description().origin().clone(),
            });
        }
        let path = contract.as_path();
        let parts: Vec<_> = path.iter().collect();
        if !(parts.len() == 3 && parts[0] == "specs" && parts[2] == "tests.md"
            || parts.len() == 2
                && parts[0] == "specs"
                && path.extension().is_some_and(|ext| ext == "md"))
        {
            return Err(Error::Contract {
                target: target.clone(),
                contract,
            });
        }
        let contract_bytes = snapshot.files().get(&contract).ok_or_else(|| {
            crate::inventory::snapshot::Error::Missing {
                path: contract.clone(),
            }
        })?;
        let annotations: Vec<_> =
            crate::annotation::parse_content(contract.as_path(), snapshot.text(&contract)?)?
                .annotations
                .into_iter()
                .filter(|annotation| {
                    matches!(
                        annotation.tier,
                        crate::annotation::Tier::Check | crate::annotation::Tier::System
                    ) && annotation.target == target.as_str()
                })
                .collect();
        if annotations.is_empty() {
            return Err(Error::Annotation {
                target: target.clone(),
                contract,
            });
        }
        let obligations: BTreeSet<_> = inventory
            .obligations()
            .iter()
            .map(Responsibility::from_obligation)
            .collect();
        if !candidate.responsibilities().is_subset(&obligations) {
            return Err(Error::Responsibility {
                target: target.clone(),
            });
        }
        let mut inputs = candidate.inputs().to_vec();
        if let Some(input) = inputs.iter_mut().find(|input| input.path == contract) {
            input.origins.insert(Origin::Contract(contract));
        } else {
            inputs.push(Input {
                path: contract.clone(),
                content: Digest::of(contract_bytes),
                origins: BTreeSet::from([Origin::Contract(contract)]),
            });
        }
        inputs.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(Unit {
            snapshot: snapshot.clone(),
            definition: definition.clone(),
            inputs,
            annotations,
        })
    }

    fn definition(&self, target: &Name) -> Result<&Definition, Error> {
        self.definitions.get(target).ok_or_else(|| Error::Missing {
            target: target.clone(),
        })
    }
}

fn read_artifact(path: &std::path::Path) -> Result<Digest, Error> {
    Ok(Digest::of(&std::fs::read(path).map_err(|source| {
        Error::Artifact {
            path: path.to_path_buf(),
            source,
        }
    })?))
}

fn require_context(snapshot: &Snapshot, inventory: &Inventory) -> Result<(), Error> {
    snapshot.require_current()?;
    if snapshot.identity() != inventory.snapshot() {
        return Err(Error::Inventory);
    }
    Ok(())
}

/// An immutable resolved execution, not verification evidence or publication authority.
///
/// ```compile_fail
/// use loom_gate::provider::Unit;
/// let unit: Unit = serde_json::from_str("{}").unwrap();
/// ```
/// ```compile_fail
/// use loom_gate::provider::Unit;
/// fn rewrite(unit: &mut Unit) { unit.inputs.clear(); }
/// ```
pub struct Unit {
    snapshot: Snapshot,
    definition: Definition,
    inputs: Vec<Input>,
    annotations: Vec<crate::annotation::Annotation>,
}

impl Unit {
    pub const fn target(&self) -> &Name {
        self.definition.description().target()
    }

    pub const fn inputs(&self) -> &[Input] {
        self.inputs.as_slice()
    }

    pub const fn responsibilities(&self) -> &BTreeSet<Responsibility> {
        self.definition.description().responsibilities()
    }

    pub const fn description(&self) -> &Candidate {
        self.definition.description()
    }

    pub fn nix_graph(&self) -> Option<&nix::Graph> {
        match &self.definition.execution {
            Execution::Nix(graph) => Some(graph),
            Execution::Tracked { .. } => None,
        }
    }

    pub const fn annotations(&self) -> &[crate::annotation::Annotation] {
        self.annotations.as_slice()
    }

    /// Run the registered execution over captured inputs, collecting observations cold.
    ///
    /// # Errors
    /// Boundary violations and a changed checkout are failures, not empty input observations.
    pub fn run(&self) -> Result<Observed, Error> {
        self.require_current()?;
        let observed = match &self.definition.execution {
            Execution::Tracked { check, members, .. } => {
                let mut context = Context::new(&self.snapshot, &self.inputs, members);
                let result = check(&mut context)?;
                context.finish(self.target().clone(), result)?
            }
            Execution::Nix(graph) => graph.run(self.target().clone())?,
        };
        self.require_current()?;
        Ok(observed)
    }

    fn require_current(&self) -> Result<(), Error> {
        self.snapshot.require_current()?;
        self.definition.require_current()
    }
}

impl From<crate::inventory::snapshot::Error> for Error {
    fn from(source: crate::inventory::snapshot::Error) -> Self {
        Self::Snapshot(Box::new(source))
    }
}

impl From<crate::inventory::Error> for Error {
    fn from(source: crate::inventory::Error) -> Self {
        Self::Subject(Box::new(source))
    }
}

impl From<crate::annotation::ParseError> for Error {
    fn from(source: crate::annotation::ParseError) -> Self {
        Self::Strategy(Box::new(source))
    }
}

#[derive(Debug, Display, Error)]
pub enum Error {
    /// checked Nix construction failed
    Nix(#[from] nix::Error),
    /// provider `{target:?}` construction failed at producer `{producer:?}`; correct the reported cause and retry `loom gate verify`
    Construction {
        target: Name,
        producer: ResourcePath,
        #[source]
        source: Box<Error>,
    },
    /// provider `{target:?}` erased or ambiguous Rust callable identity; register a named function item, not a closure/function pointer, and retry `loom gate verify`
    Callable { target: Name },
    /// failed to locate the running checker artifact
    Executable(#[source] std::io::Error),
    /// failed to read checker artifact `{path}`; rebuild the provider and retry `loom gate verify`
    Artifact {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// provider `{target:?}` checker artifact changed; rebuild its definition and retry `loom gate verify`
    BuildChanged { target: Name },
    /// failed to capture provider context
    Snapshot(#[source] Box<crate::inventory::snapshot::Error>),
    /// failed to resolve independent project responsibility
    Subject(#[source] Box<crate::inventory::Error>),
    /// malformed typed provider description; use the checked definition's generated query and retry `loom gate verify`
    Description(#[from] serde_json::Error),
    /// failed to parse the captured acceptance contract
    Strategy(#[source] Box<crate::annotation::ParseError>),
    /// no owning executable annotation for provider `{target:?}` in `{contract:?}`; correct its target mapping and retry `loom gate verify`
    Annotation {
        target: Name,
        contract: ResourcePath,
    },
    /// inventory does not describe the captured snapshot; rediscover project definitions and retry `loom gate verify`
    Inventory,
    /// provider `{target:?}` belongs to a stale snapshot; regenerate its description and retry `loom gate verify`
    Context { target: Name },
    /// provider `{target:?}` has no checked execution definition; register the supported constructor and retry `loom gate verify`
    Missing { target: Name },
    /// provider admission failed for `{target:?}` at annotation source `{contract:?}`; correct the reported cause and retry `loom gate verify`
    Admission {
        target: Name,
        contract: ResourcePath,
        #[source]
        source: Box<Error>,
    },
    /// query omitted requested provider `{target:?}`; generate the complete per-target response and retry `loom gate verify`
    MissingResponse { target: Name },
    /// query returned unrequested provider `{target:?}`; correct the producer's target mapping and retry `loom gate verify`
    Unexpected { target: Name },
    /// duplicate or ambiguous provider `{target:?}`; give checked definitions distinct targets and retry `loom gate verify`
    Duplicate { target: Name },
    /// provider `{target:?}` disagrees with producer `{producer:?}`; regenerate from that definition and retry `loom gate verify`
    Inconsistent {
        target: Name,
        producer: ResourcePath,
    },
    /// provider `{target:?}` offers no matching current subject/duty; correct its responsibility and retry `loom gate verify`
    Responsibility { target: Name },
    /// provider `{target:?}` checker `{checker:?}` has no native Rust owner; register its actual source and retry `loom gate verify`
    Checker { target: Name, checker: ResourcePath },
    /// provider `{target:?}` contract `{contract:?}` is not a spec acceptance document; correct the annotation source and retry `loom gate verify`
    Contract {
        target: Name,
        contract: ResourcePath,
    },
    /// tracked provider read outside its resolved inputs: `{path:?}`; add the responsible project subject to the checked definition and retry `loom gate verify`
    Outside { path: ResourcePath },
    /// tracked provider requested unknown subject `{subject:?}`; register that analysis subject and retry `loom gate verify`
    AnalysisSubject { subject: Name },
    /// tracked input `{path:?}` is not UTF-8; use the byte-read boundary
    Encoding { path: ResourcePath },
}

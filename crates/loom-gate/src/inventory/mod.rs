//! Project subjects and baseline obligations discovered independently of providers.

pub mod nix;
mod rust;
pub mod snapshot;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use displaydoc::Display;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::annotation::strategy::{Declaration, Source};
use snapshot::{Identity, ResourcePath, Snapshot};

/// An exact project target or resource name, never a membership glob.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct Name(String);

impl TryFrom<String> for Name {
    type Error = Error;

    fn try_from(value: String) -> Result<Self, Error> {
        if value.trim().is_empty()
            || value
                .chars()
                .any(|c| c.is_control() || matches!(c, '*' | '?' | '[' | ']'))
        {
            return Err(Error::Name { value });
        }
        Ok(Self(value))
    }
}

impl Name {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display)]
pub enum Tool {
    /// cargo
    Cargo,
    /// nix
    Nix,
}

impl Tool {
    const fn program(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Nix => "nix",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    RustSource,
    NixSource,
    TestFixture,
    Generated,
    Document,
    Configuration,
    RuntimeResource,
    ProjectContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Duty {
    Build,
    Lint,
    Test,
    Evaluate,
    Check,
    Produce,
    Document,
}

/// Required responsibility exists even when no provider offers it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Obligation {
    subject: Name,
    duty: Duty,
    responsible: Name,
}

impl Obligation {
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

/// Membership and relationships come from the project definition, not spec policy.
#[derive(Debug, Clone)]
pub struct Subject {
    name: Name,
    role: Role,
    members: BTreeSet<ResourcePath>,
    consumers: BTreeSet<Name>,
    producer: Option<Name>,
    testable: bool,
    origin: ResourcePath,
    nix: Option<nix::Target>,
}

impl Subject {
    pub const fn name(&self) -> &Name {
        &self.name
    }
    pub const fn role(&self) -> Role {
        self.role
    }
    pub const fn members(&self) -> &BTreeSet<ResourcePath> {
        &self.members
    }
    pub const fn origin(&self) -> &ResourcePath {
        &self.origin
    }
    pub const fn nix_target(&self) -> Option<&nix::Target> {
        self.nix.as_ref()
    }
}

/// A resolved policy is bound to one independently discovered snapshot.
#[derive(Debug, Clone)]
pub struct Inventory {
    snapshot: Identity,
    subjects: BTreeMap<Name, Subject>,
    obligations: BTreeSet<Obligation>,
    compilation: BTreeMap<ResourcePath, rust::Compilation>,
    native_graphs: BTreeMap<Name, std::sync::Arc<crate::provider::nix::Graph>>,
}

impl Inventory {
    /// Discover native targets, source ownership, and project resources before joining providers.
    ///
    /// # Errors
    /// Rejects failed discovery, orphan/unsupported subjects, and unresolved resource responsibilities.
    pub fn discover(snapshot: &Snapshot) -> Result<Self, Error> {
        let staged = snapshot.stage()?;
        let mut subjects = BTreeMap::new();
        let compilation = rust::discover(snapshot, staged.path(), &mut subjects)?;
        discover_context(snapshot, &mut subjects)?;
        let native_graphs = nix::discover(snapshot, staged.path(), &mut subjects)?;
        classify_remaining(snapshot, &mut subjects, &compilation)?;
        snapshot.require_current()?;
        let mut inventory = Self::finish(snapshot.identity().clone(), subjects, compilation)?;
        inventory.native_graphs = native_graphs;
        let mut declarations = Vec::new();
        for path in contracts(snapshot)? {
            declarations.extend(
                crate::annotation::parse_content(path.as_path(), snapshot.text(&path)?)?.strategy,
            );
        }
        inventory.with_policy(&declarations)
    }

    fn finish(
        snapshot: Identity,
        subjects: BTreeMap<Name, Subject>,
        compilation: BTreeMap<ResourcePath, rust::Compilation>,
    ) -> Result<Self, Error> {
        for subject in subjects
            .values()
            .filter(|subject| subject.role == Role::Document)
        {
            for path in &subject.members {
                if !is_document(path) {
                    return Err(Error::Unclassified { path: path.clone() });
                }
            }
        }
        let obligations = subjects
            .values()
            .map(|subject| obligations_for(subject, &subjects))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        Ok(Self {
            snapshot,
            subjects,
            obligations,
            compilation,
            native_graphs: BTreeMap::new(),
        })
    }

    /// Successful resource discovery retains every constructed output on its native system.
    /// These are immutable recipe facts, never execution outcomes or admission grants.
    pub(crate) const fn native_graphs(
        &self,
    ) -> &BTreeMap<Name, std::sync::Arc<crate::provider::nix::Graph>> {
        &self.native_graphs
    }

    /// Captured native-system recipe facts; callers must still construct and admit a definition.
    pub fn native_graph(
        &self,
        name: &Name,
    ) -> Option<&std::sync::Arc<crate::provider::nix::Graph>> {
        self.native_graphs.get(name)
    }

    pub const fn snapshot(&self) -> &Identity {
        &self.snapshot
    }
    pub const fn subjects(&self) -> &BTreeMap<Name, Subject> {
        &self.subjects
    }
    pub const fn obligations(&self) -> &BTreeSet<Obligation> {
        &self.obligations
    }

    pub(crate) fn is_production(&self, name: &Name) -> bool {
        self.compilation
            .values()
            .any(|compilation| compilation.subjects.contains(name))
    }

    /// Resolve a reference to exactly one current subject; no alias or glob inference.
    ///
    /// # Errors
    /// Unknown references retain their source and an existing retry command.
    pub fn subject(&self, name: &Name) -> Result<&Subject, Error> {
        self.subjects
            .get(name)
            .ok_or_else(|| Error::Unknown { name: name.clone() })
    }

    /// Native production compilation subjects for the Cargo manifest owning a checker source.
    ///
    /// Includes normal/build workspace dependencies and compiler configuration, not unrelated
    /// integration tests, examples, benches, or fixture resources.
    ///
    /// # Errors
    /// Rejects checker paths without exactly one discovered owning Cargo manifest.
    pub fn checker_subjects(&self, checker: &ResourcePath) -> Result<BTreeSet<Name>, Error> {
        let owners: BTreeSet<_> = self
            .subjects
            .values()
            .filter(|subject| {
                subject.members.contains(checker)
                    && self
                        .compilation
                        .get(&subject.origin)
                        .is_some_and(|compilation| compilation.subjects.contains(&subject.name))
            })
            .map(|subject| subject.origin.clone())
            .collect();
        if owners.len() != 1 {
            return Err(Error::Checker {
                path: checker.clone(),
            });
        }
        let compilation = owners
            .iter()
            .next()
            .and_then(|origin| self.compilation.get(origin))
            .ok_or_else(|| Error::Checker {
                path: checker.clone(),
            })?;
        if let Some(path) = compilation.outside.first() {
            return Err(Error::Outside { path: path.clone() });
        }
        let mut names = compilation.subjects.clone();
        for path in &compilation.configuration {
            let name = Name::try_from(format!("resource:{}", path.as_path().display()))?;
            self.subject(&name)?;
            names.insert(name);
        }
        Ok(names)
    }

    fn with_policy(&self, declarations: &[Declaration]) -> Result<Self, Error> {
        let mut subjects = self.subjects.clone();
        let mut seen = BTreeSet::new();
        for declaration in declarations {
            let Declaration::Package { source, policy } = declaration else {
                continue;
            };
            for exception in &policy.role_exceptions {
                let name =
                    Name::try_from(exception.subject.as_str().to_owned()).map_err(|error| {
                        Error::Policy {
                            source_context: source.clone(),
                            detail: error.to_string(),
                        }
                    })?;
                let role = match exception.role.as_str() {
                    "test-fixture" => Role::TestFixture,
                    "generated" => Role::Generated,
                    "document" => Role::Document,
                    other => {
                        return Err(Error::Policy {
                            source_context: source.clone(),
                            detail: format!(
                                "unsupported role `{other}`; use a supported classification"
                            ),
                        });
                    }
                };
                let subject = subjects.get_mut(&name).ok_or_else(|| Error::Policy {
                    source_context: source.clone(), detail: format!("unknown subject `{}`; reference a current project target/resource, not a file glob", name.as_str()),
                })?;
                if !seen.insert(name.clone()) {
                    return Err(Error::Policy {
                        source_context: source.clone(),
                        detail: format!("ambiguous repeated exception for `{}`", name.as_str()),
                    });
                }
                if role == Role::Document && subject.role != Role::Document {
                    return Err(Error::Policy {
                        source_context: source.clone(),
                        detail:
                            "source cannot become documentation to remove its consumer obligations"
                                .into(),
                    });
                }
                subject.role = role;
                let name = subject.name.clone();
                if let Some(subject) = subjects.get(&name) {
                    obligations_for(subject, &subjects).map_err(|error| Error::Policy {
                        source_context: source.clone(),
                        detail: error.to_string(),
                    })?;
                }
            }
        }
        prune_context_documents(&mut subjects)?;
        let mut inventory =
            Self::finish(self.snapshot.clone(), subjects, self.compilation.clone())?;
        inventory.native_graphs.clone_from(&self.native_graphs);
        Ok(inventory)
    }
}

pub(crate) fn contracts(snapshot: &Snapshot) -> Result<Vec<ResourcePath>, Error> {
    if let Some(contracts) = snapshot.contracts.get() {
        return Ok(contracts.clone());
    }
    if !snapshot
        .files()
        .keys()
        .any(|path| path.as_path().starts_with("specs"))
    {
        return Ok(Vec::new());
    }
    let staged = snapshot.stage()?;
    let contracts = loom_driver::spec::package::discover_workspace(staged.path())?
        .into_iter()
        .map(|package| {
            let relative = package
                .acceptance()
                .strip_prefix(staged.path())
                .map_err(|_| Error::Outside {
                    path: package.acceptance().to_path_buf(),
                })?;
            Ok(ResourcePath::try_from(relative.to_path_buf())?)
        })
        .collect::<Result<_, Error>>()?;
    Ok(snapshot.contracts.get_or_init(|| contracts).clone())
}

fn obligations_for(
    subject: &Subject,
    subjects: &BTreeMap<Name, Subject>,
) -> Result<Vec<Obligation>, Error> {
    let duties = match subject.role {
        Role::RustSource => {
            let mut duties = vec![
                (Duty::Build, subject.name.clone()),
                (Duty::Lint, subject.name.clone()),
            ];
            if subject.testable {
                duties.push((Duty::Test, subject.name.clone()));
            }
            duties
        }
        Role::NixSource => {
            let mut duties = vec![(Duty::Evaluate, subject.name.clone())];
            duties.extend(
                subject
                    .consumers
                    .iter()
                    .cloned()
                    .map(|name| (Duty::Check, name)),
            );
            if subject.consumers.is_empty() {
                duties.push((Duty::Check, subject.name.clone()));
            }
            duties
        }
        Role::Document => vec![(Duty::Document, subject.name.clone())],
        Role::ProjectContext => vec![(Duty::Check, subject.name.clone())],
        Role::TestFixture => consumer_tests(subject, subjects)?,
        Role::Configuration | Role::RuntimeResource => consumer_duties(subject, subjects)?,
        Role::Generated => {
            let Some(producer) = &subject.producer else {
                return Err(Error::Responsibility {
                    name: subject.name.clone(),
                    role: subject.role,
                });
            };
            if subject.consumers.is_empty() {
                return Err(Error::Responsibility {
                    name: subject.name.clone(),
                    role: subject.role,
                });
            }
            let mut duties = vec![(Duty::Produce, producer.clone())];
            if !subjects.contains_key(producer) {
                return Err(Error::Unknown {
                    name: producer.clone(),
                });
            }
            duties.extend(consumer_duties(subject, subjects)?);
            duties
        }
    };
    Ok(duties
        .into_iter()
        .map(|(duty, responsible)| Obligation {
            subject: subject.name.clone(),
            duty,
            responsible,
        })
        .collect())
}

fn consumer_duties(
    subject: &Subject,
    subjects: &BTreeMap<Name, Subject>,
) -> Result<Vec<(Duty, Name)>, Error> {
    if subject.consumers.is_empty() {
        return Err(Error::Responsibility {
            name: subject.name.clone(),
            role: subject.role,
        });
    }
    let mut duties = Vec::new();
    for name in &subject.consumers {
        let consumer = subjects
            .get(name)
            .ok_or_else(|| Error::Unknown { name: name.clone() })?;
        match consumer.role {
            Role::ProjectContext => duties.push((Duty::Check, name.clone())),
            Role::RustSource => {
                duties.extend([(Duty::Build, name.clone()), (Duty::Lint, name.clone())]);
                if consumer.testable {
                    duties.push((Duty::Test, name.clone()));
                }
            }
            Role::NixSource => {
                duties.extend([(Duty::Evaluate, name.clone()), (Duty::Check, name.clone())]);
            }
            Role::TestFixture if consumer.nix.is_some() => {
                duties.extend([(Duty::Evaluate, name.clone()), (Duty::Check, name.clone())]);
            }
            Role::TestFixture if consumer.testable => {
                duties.push((Duty::Test, name.clone()));
            }
            Role::TestFixture => duties.extend(consumer_tests(consumer, subjects)?),
            _ => {
                return Err(Error::Responsibility {
                    name: subject.name.clone(),
                    role: subject.role,
                });
            }
        }
    }
    Ok(duties)
}

fn consumer_tests(
    subject: &Subject,
    subjects: &BTreeMap<Name, Subject>,
) -> Result<Vec<(Duty, Name)>, Error> {
    if subject.consumers.is_empty() {
        return Err(Error::Responsibility {
            name: subject.name.clone(),
            role: subject.role,
        });
    }
    let mut duties = Vec::new();
    for name in &subject.consumers {
        let consumer = subjects
            .get(name)
            .ok_or_else(|| Error::Unknown { name: name.clone() })?;
        if consumer.nix.is_some() || consumer.role == Role::NixSource {
            duties.extend([(Duty::Evaluate, name.clone()), (Duty::Check, name.clone())]);
        } else if consumer.testable {
            duties.push((Duty::Test, name.clone()));
        } else {
            return Err(Error::Responsibility {
                name: subject.name.clone(),
                role: subject.role,
            });
        }
    }
    Ok(duties)
}

fn insert(subjects: &mut BTreeMap<Name, Subject>, subject: Subject) -> Result<(), Error> {
    if subjects.contains_key(&subject.name) {
        return Err(Error::Ambiguous { name: subject.name });
    }
    subjects.insert(subject.name.clone(), subject);
    Ok(())
}

fn single(path: &ResourcePath, role: Role, prefix: &str) -> Result<Subject, Error> {
    Ok(Subject {
        name: Name::try_from(format!("{prefix}:{}", path.as_path().display()))?,
        role,
        members: BTreeSet::from([path.clone()]),
        consumers: BTreeSet::new(),
        producer: None,
        testable: false,
        origin: path.clone(),
        nix: None,
    })
}

pub(crate) const PROJECT_CONTEXT: &str = "project:context";

fn data_documents(subjects: &BTreeMap<Name, Subject>) -> BTreeSet<ResourcePath> {
    subjects
        .values()
        .filter(|subject| matches!(subject.role, Role::TestFixture | Role::Generated))
        .flat_map(|subject| subject.members.iter())
        .cloned()
        .collect()
}

fn prune_context_documents(subjects: &mut BTreeMap<Name, Subject>) -> Result<(), Error> {
    let data = data_documents(subjects);
    let reader = Name::try_from(PROJECT_CONTEXT.to_owned())?;
    if let Some(context) = subjects.get_mut(&reader) {
        context.members.retain(|path| !data.contains(path));
        if let Some(origin) = context.members.first() {
            context.origin.clone_from(origin);
        } else {
            subjects.remove(&reader);
        }
    }
    Ok(())
}

fn discover_context(
    snapshot: &Snapshot,
    subjects: &mut BTreeMap<Name, Subject>,
) -> Result<(), Error> {
    let data = data_documents(subjects);
    let mut members: BTreeSet<_> = snapshot
        .files()
        .keys()
        .filter(|path| {
            !data.contains(*path)
                && (is_document(path)
                    || matches!(
                        path.as_path().to_str(),
                        Some(
                            "loom.toml"
                                | "wrix.toml"
                                | ".pre-commit-config.yaml"
                                | ".beads/config.yaml"
                                | ".beads/metadata.json"
                                | ".envrc"
                        )
                    )
                    || path.as_path().file_name().is_some_and(|name| {
                        name == ".gitignore" || name == ".pre-commit-config.yaml"
                    }))
        })
        .cloned()
        .collect();
    for contract in contracts(snapshot)? {
        let parsed =
            crate::annotation::parse_content(contract.as_path(), snapshot.text(&contract)?)?;
        for annotation in parsed
            .annotations
            .iter()
            .filter(|annotation| annotation.tier == crate::Tier::Judge)
        {
            if let Some(path) = crate::integrity::resolve_spec_relative_script_path(
                &annotation.target,
                &annotation.source_spec,
                snapshot.root(),
            ) {
                let relative = match path.strip_prefix(snapshot.root()) {
                    Ok(relative) => relative.to_path_buf(),
                    Err(_) => path,
                };
                let path = ResourcePath::try_from(relative)?;
                if snapshot.files().contains_key(&path) {
                    members.insert(path);
                }
            }
        }
    }
    let Some(origin) = members.first().cloned() else {
        return Ok(());
    };
    insert(
        subjects,
        Subject {
            name: Name::try_from(PROJECT_CONTEXT.to_owned())?,
            role: Role::ProjectContext,
            members,
            consumers: BTreeSet::new(),
            producer: None,
            testable: false,
            origin,
            nix: None,
        },
    )
}

fn classify_remaining(
    snapshot: &Snapshot,
    subjects: &mut BTreeMap<Name, Subject>,
    compilation: &BTreeMap<ResourcePath, rust::Compilation>,
) -> Result<(), Error> {
    let accounted: BTreeSet<_> = subjects
        .values()
        .flat_map(|subject| subject.members.iter().cloned())
        .collect();
    let documents: BTreeSet<_> = subjects
        .values()
        .filter(|subject| subject.role == Role::Document)
        .flat_map(|subject| subject.members.iter().cloned())
        .collect();
    let resources: BTreeSet<_> = subjects
        .values()
        .filter(|subject| {
            matches!(
                subject.role,
                Role::TestFixture | Role::Generated | Role::Document
            ) || (subject.role == Role::RuntimeResource
                && !subject.consumers.is_empty()
                && subject.consumers.iter().all(|consumer| {
                    subjects
                        .get(consumer)
                        .is_some_and(|reader| reader.nix.is_some())
                }))
        })
        .flat_map(|subject| subject.members.iter().cloned())
        .collect();
    for path in snapshot.files().keys() {
        let extension = path.as_path().extension().and_then(|ext| ext.to_str());
        let document = is_document(path);
        let configuration = is_configuration(path);
        if extension == Some("rs")
            && !resources.contains(path)
            && !subjects
                .values()
                .any(|subject| subject.role == Role::RustSource && subject.members.contains(path))
        {
            return Err(Error::Orphan { path: path.clone() });
        }
        if accounted.contains(path)
            && (resources.contains(path)
                || (!document && !configuration)
                || documents.contains(path))
        {
            continue;
        }
        let role = if document {
            Role::Document
        } else if configuration {
            Role::Configuration
        } else {
            return Err(Error::Unclassified { path: path.clone() });
        };
        let mut subject = single(path, role, "resource")?;
        if role == Role::Configuration {
            subject.consumers.extend(
                subjects
                    .values()
                    .filter(|reader| {
                        (matches!(reader.role, Role::ProjectContext | Role::RustSource)
                            && reader.members.contains(path))
                            || (reader.role == Role::RustSource
                                && compilation
                                    .get(&reader.origin)
                                    .is_some_and(|native| native.configuration.contains(path)))
                            || reader.nix_target().is_some_and(|native| {
                                native.flake().as_path().parent().is_some_and(|directory| {
                                    path.as_path() == directory.join("flake.lock")
                                })
                            })
                    })
                    .map(|reader| reader.name.clone()),
            );
            if let Some(native) = subjects.get_mut(&subject.name) {
                native.consumers.extend(subject.consumers);
                continue;
            }
        }
        insert(subjects, subject)?;
    }
    Ok(())
}

fn is_configuration(path: &ResourcePath) -> bool {
    matches!(
        path.as_path().extension().and_then(|ext| ext.to_str()),
        Some("toml" | "lock" | "yaml" | "yml" | "json")
    ) || matches!(
        path.as_path().file_name().and_then(|name| name.to_str()),
        Some(".gitignore" | ".envrc" | "rust-toolchain")
    ) || (path
        .as_path()
        .file_name()
        .is_some_and(|name| name == "config")
        && path
            .as_path()
            .parent()
            .is_some_and(|parent| parent.ends_with(".cargo")))
}

fn is_document(path: &ResourcePath) -> bool {
    matches!(
        path.as_path().extension().and_then(|ext| ext.to_str()),
        Some("md" | "txt" | "rst")
    ) || matches!(
        path.as_path().file_name().and_then(|name| name.to_str()),
        Some("LICENSE" | "README")
    )
}

fn command(root: &Path, program: Tool, args: &[&str]) -> Result<Vec<u8>, Error> {
    let clock = loom_driver::clock::SystemClock::new();
    let started = loom_driver::clock::Clock::now(&clock);
    tracing::debug!(
        ?program,
        operation = args.first().copied(),
        "inventory operation started"
    );
    let mut command = Command::new(program.program());
    command.args(args).current_dir(root);
    if matches!(program, Tool::Nix) {
        command.args(["--extra-experimental-features", "nix-command flakes"]);
    }
    loom_driver::workspace::cargo::discovery_command(&mut command);
    let output = command.output().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            Error::Capability { program }
        } else {
            Error::Io(error)
        }
    })?;
    tracing::debug!(?program, operation = args.first().copied(), elapsed = ?loom_driver::clock::Clock::now(&clock).saturating_duration_since(started), code = output.status.code(), stdout_bytes = output.stdout.len(), "inventory operation finished");
    if !output.status.success() {
        return Err(Error::Discovery {
            program,
            detail: format!(
                "{}: {}",
                output.status,
                crate::inputs::bounded_query_tail_diagnostic(&String::from_utf8_lossy(
                    &output.stderr
                ))
            ),
        });
    }
    Ok(output.stdout)
}

fn relative(root: &Path, path: &Path) -> Result<ResourcePath, Error> {
    Ok(ResourcePath::try_from(
        path.strip_prefix(root)
            .map_err(|_| Error::Outside {
                path: path.to_path_buf(),
            })?
            .to_path_buf(),
    )?)
}

#[derive(Debug, Display, Error)]
pub enum Error {
    /// failed to capture current project inputs
    Snapshot(#[from] snapshot::Error),
    /// project discovery I/O failed
    Io(#[from] std::io::Error),
    /// required discovery tool `{program}` is unavailable; provision the declared capability and rerun `loom gate verify`
    Capability { program: Tool },
    /// invalid constructed project resource for `{target:?}`; repair its source/consumer definition and rerun `loom gate verify`
    NativeResource {
        target: Name,
        #[source]
        source: Box<crate::provider::nix::Error>,
    },
    /// invalid constructed project resources {targets:?}; repair their source/consumer definitions and rerun `loom gate verify`
    NativeResources {
        targets: Vec<Name>,
        #[source]
        source: Box<crate::provider::nix::Error>,
    },
    /// malformed native project definition `{definition:?}`; repair it and rerun `loom gate verify`
    Definition {
        definition: ResourcePath,
        #[source]
        source: serde_json::Error,
    },
    /// spec ownership discovery failed: {0}; repair the owning acceptance documents/index and rerun `loom gate verify`
    Package(#[from] loom_driver::spec::package::Error),
    /// invalid spec-owned verification strategy
    Strategy(#[from] crate::annotation::ParseError),
    /// source syntax cannot be resolved for `{path:?}`
    Syntax {
        path: ResourcePath,
        #[source]
        source: syn::Error,
    },
    /// `{program}` project discovery failed: {detail}; repair the project and rerun `loom gate verify`
    Discovery { program: Tool, detail: String },
    /// project definition references external source `{path}`; declare its origin explicitly
    Outside { path: PathBuf },
    /// invalid subject name `{value}`; use one exact current project target/resource
    Name { value: String },
    /// duplicate or ambiguous project subject `{name:?}`; give project resources unique names
    Ambiguous { name: Name },
    /// unknown project subject `{name:?}`; correct the project reference and rerun `loom gate verify`
    Unknown { name: Name },
    /// checker `{path:?}` has no unique native Cargo owner; attach it to one discovered Cargo target
    Checker { path: ResourcePath },
    /// orphan Rust source `{path:?}`; attach it to a Cargo target or consuming fixture resource
    Orphan { path: ResourcePath },
    /// unsupported/unclassified subject `{path:?}`; supply a supported project resource definition and rerun `loom gate verify`
    Unclassified { path: ResourcePath },
    /// subject `{name:?}` classified as {role:?} has no required consumer/producer; wire its responsible target and rerun `loom gate verify`
    Responsibility { name: Name, role: Role },
    /// invalid role exception at `{source_context:?}`: {detail}; correct `tests.md` and rerun `loom gate verify`
    Policy {
        source_context: Source,
        detail: String,
    },
    /// ambiguous Rust module `{path}`; remove the duplicate module definition
    Module { path: PathBuf },
}

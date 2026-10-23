//! Compiler-produced Cargo/Nextest facts; preparation is not a test verdict.

mod profile;

use std::path::PathBuf;

use guppy::graph::PackageGraph;
use nextest_metadata::{RustBinaryId, TestCaseName, TestListSummary};

use super::{CargoOperation, Definition, Digest, Error, Execution, Graph, Name, Observed, Outcome};

/// A preparation failure retains its actual unverified outcome, never an empty catalogue.
pub enum Preparation<'a> {
    Ready(Box<Catalogue<'a>>),
    Unverified(Observed),
}

impl<'a> Preparation<'a> {
    /// Require actual preparation, retaining any semantic or unavailable outcome.
    ///
    /// # Errors
    /// An unverified preparation is never replaced by an empty or cached catalogue.
    pub fn require_verified(self) -> Result<Box<Catalogue<'a>>, Error> {
        match self {
            Self::Ready(catalogue) => Ok(catalogue),
            Self::Unverified(observed) => Err(Error::Preparation {
                observed: Box::new(observed),
            }),
        }
    }
}

/// A live, complete catalogue bound to the actual fixed native compiler operation.
/// There is deliberately no deserializer or caller-provided listing constructor.
pub struct Catalogue<'a> {
    producer: &'a Graph,
    cargo_path: PathBuf,
    tests_path: PathBuf,
    identity: Digest,
    packages: PackageGraph,
    tests: TestListSummary,
    profile: profile::Profile,
}

/// An actual native case identity, not a source-name or input-list assertion.
pub struct Case<'a> {
    binary: &'a RustBinaryId,
    name: &'a TestCaseName,
    ignored: bool,
    runnable: bool,
}

impl Case<'_> {
    pub const fn binary(&self) -> &RustBinaryId {
        self.binary
    }
    pub const fn name(&self) -> &TestCaseName {
        self.name
    }
    pub const fn ignored(&self) -> bool {
        self.ignored
    }
    pub const fn runnable(&self) -> bool {
        self.runnable
    }
}

impl Graph {
    /// Compile and enumerate through the validated companion, rebuilding warm outputs.
    /// This executes preparation only; it cannot discharge any Rust test obligation.
    ///
    /// # Errors
    /// Rejects absent/changed constructors and malformed or incomplete native facts.
    pub fn prepare_catalogue(&self, target: Name) -> Result<Preparation<'_>, Error> {
        if self.cargo != Some(CargoOperation::Test) {
            return Err(Error::CargoExecution);
        }
        let root = self.catalogue.as_ref().ok_or(Error::CargoExecution)?;
        let observed = self.run_root(target, root)?;
        if !matches!(observed.outcome(), Outcome::Pass) {
            return Ok(Preparation::Unverified(observed));
        }
        let output = self
            .nodes
            .get(root)
            .ok_or(Error::Root)?
            .outputs
            .iter()
            .find(|(name, _)| name.as_str() == "out")
            .map(|(_, path)| path)
            .ok_or(Error::CargoExecution)?;
        let cargo_path = PathBuf::from(output.as_str()).join("cargo.json");
        let tests_path = PathBuf::from(output.as_str()).join("nextest.json");
        let cargo_bytes = read(&cargo_path)?;
        let tests_bytes = read(&tests_path)?;
        let packages = PackageGraph::from_json(
            std::str::from_utf8(&cargo_bytes).map_err(|_| Error::Encoding)?,
        )
        .map_err(|error| Error::Catalogue {
            detail: error.to_string(),
        })?;
        let tests = serde_json::from_slice(&tests_bytes)?;
        crate::provider::plan::property::Context::require_native_catalogue(&packages, &tests)
            .map_err(|error| Error::Catalogue {
                detail: error.to_string(),
            })?;
        let profile = profile::Profile::load(std::path::Path::new(output.as_str()), &tests)?;
        let identity = fingerprint(&self.identity, &cargo_bytes, &tests_bytes, &profile)?;
        self.require_current()?;
        Ok(Preparation::Ready(Box::new(Catalogue {
            producer: self,
            cargo_path,
            tests_path,
            identity,
            packages,
            tests,
            profile,
        })))
    }
}

impl Catalogue<'_> {
    pub(super) fn report_contract(&self) -> Result<super::report::Contract, Error> {
        self.require_current()?;
        let mut targets = std::collections::BTreeSet::new();
        for suite in self.tests.rust_suites.values() {
            for name in suite.test_cases.keys() {
                let target = Name::try_from(format!(
                    "{}::{}${name}",
                    suite.package_name, suite.binary.binary_name
                ))
                .map_err(|_| Error::Result)?;
                if !targets.insert(target) {
                    return Err(Error::Catalogue {
                        detail: "compiled binaries have colliding Nextest report identities".into(),
                    });
                }
            }
        }
        Ok(super::report::Contract {
            parser: super::report::Parser::LibtestJson,
            targets,
            tier: super::report::Lane::Check,
        })
    }

    pub const fn packages(&self) -> &PackageGraph {
        &self.packages
    }
    pub const fn tests(&self) -> &TestListSummary {
        &self.tests
    }
    pub const fn producer(&self) -> &Graph {
        self.producer
    }

    /// Revalidate the producer and actual metadata bytes, not a saved result cache.
    ///
    /// # Errors
    /// Rejects changed tools, graph, resources or compiler-produced metadata.
    pub fn require_current(&self) -> Result<(), Error> {
        self.producer.require_current()?;
        let cargo = read(&self.cargo_path)?;
        let tests = read(&self.tests_path)?;
        if fingerprint(&self.producer.identity, &cargo, &tests, &self.profile)? != self.identity {
            return Err(Error::GraphChanged);
        }
        Ok(())
    }

    /// Partition actual compiled cases with the actual Nextest default/group context.
    /// This is classification, not an execution verdict or a responsibility grant.
    ///
    /// # Errors
    /// Rejects stale native facts and invalid or empty applicable filters.
    pub fn partition(
        &self,
        requests: &[crate::provider::plan::property::Request<'_>],
    ) -> Result<crate::provider::plan::property::Partition, Error> {
        self.require_current()?;
        crate::provider::plan::property::Context {
            graph: &self.packages,
            tests: &self.tests,
            evaluation: nextest_filtering::EvalContext {
                default_filter: &self.profile.default,
            },
            known_groups: &self.profile.known,
            groups: &self.profile,
        }
        .resolve(requests)
        .map_err(|error| Error::Catalogue {
            detail: error.to_string(),
        })
    }

    /// Resolve an exact native identity, or an unambiguous source-level case name.
    ///
    /// # Errors
    /// Missing and ambiguous cases fail rather than falling back to source scans or commands.
    pub fn resolve(&self, target: &Name) -> Result<Case<'_>, Error> {
        self.require_current()?;
        self.resolve_captured(target)
    }

    /// Look up a batch in one freshly validated compiler context. Absent names
    /// are omitted; ambiguous identities remain errors, including for pending owners.
    ///
    /// # Errors
    /// Rejects stale preparation and ambiguous native identities.
    pub fn lookup(
        &self,
        targets: &std::collections::BTreeSet<Name>,
    ) -> Result<std::collections::BTreeMap<Name, Case<'_>>, Error> {
        self.require_current()?;
        let mut cases = std::collections::BTreeMap::new();
        for target in targets {
            match self.resolve_captured(target) {
                Ok(case) => {
                    cases.insert(target.clone(), case);
                }
                Err(Error::MissingCase { .. }) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(cases)
    }

    fn resolve_captured(&self, target: &Name) -> Result<Case<'_>, Error> {
        let mut matches = self.tests.rust_suites.values().flat_map(|suite| {
            suite.test_cases.iter().filter_map(move |(name, case)| {
                let qualified = format!("{}::{}", suite.binary.binary_id, name);
                (qualified == target.as_str()
                    || name.as_str() == target.as_str()
                    || name.as_str().rsplit("::").next() == Some(target.as_str()))
                .then_some(Case {
                    binary: &suite.binary.binary_id,
                    name,
                    ignored: case.ignored,
                    runnable: !case.ignored
                        && case.filter_match.is_match()
                        && case.kind == Some(nextest_metadata::RustTestKind::TEST),
                })
            })
        });
        let case = matches.next().ok_or_else(|| Error::MissingCase {
            target: target.clone(),
        })?;
        if matches.next().is_some() {
            return Err(Error::AmbiguousCase {
                target: target.clone(),
            });
        }
        Ok(case)
    }
}

#[derive(serde::Deserialize)]
struct Producer {
    namespace: String,
    attribute: String,
}

impl Definition {
    /// Discover current-platform fixed Cargo test producers from actual native outputs.
    /// Metadata selects candidates only; each producer must pass full graph/phase validation.
    ///
    /// # Errors
    /// Rejects failed native queries, stale source and falsely labelled constructors.
    pub fn native_rust_suites(
        snapshot: &crate::inventory::snapshot::Snapshot,
        inventory: &crate::inventory::Inventory,
    ) -> Result<Vec<Self>, super::super::Error> {
        super::super::require_context(snapshot, inventory)?;
        let tool = super::locate_tool()?;
        let platform: super::Platforms =
            serde_json::from_slice(&super::query(&tool, &["config", "show", "--json"])?)?;
        let captured = inventory.native_graphs();
        if !captured.is_empty()
            && captured.values().all(|graph| {
                graph.tool == tool
                    && graph
                        .nodes
                        .get(&graph.root)
                        .is_some_and(|node| node.system == platform.system.value)
            })
        {
            let mut definitions = Vec::new();
            let mut seen = std::collections::BTreeSet::new();
            for (subject, graph) in captured {
                if graph.cargo == Some(CargoOperation::Test) {
                    let definition =
                        Self::hermetic_baseline(snapshot, inventory, subject.clone(), subject)?;
                    if seen.insert(String::from(graph.identity.clone())) {
                        definitions.push(definition);
                    }
                }
            }
            snapshot.require_current()?;
            return Ok(definitions);
        }
        let mut flakes = std::collections::BTreeMap::new();
        for subject in inventory.subjects().values() {
            if let Some(native) = subject.nix_target() {
                flakes
                    .entry(native.flake())
                    .or_insert_with(|| native.project());
            }
        }
        let mut definitions = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for (flake, project) in flakes {
            let reference = format!("path:{}", super::StorePath::root(project)?.as_str());
            let metadata: super::Metadata = serde_json::from_slice(&super::query(
                &tool,
                &[
                    "flake",
                    "metadata",
                    &reference,
                    "--json",
                    "--offline",
                    "--no-write-lock-file",
                ],
            )?)?;
            let locked = serde_json::to_string(&metadata.url)?.replace("${", "\\${");
            let system = serde_json::to_string(platform.system.value.as_str())?;
            let expression = format!(
                "let flake = builtins.getFlake {locked}; select = namespace: let values = flake.${{namespace}}.{system} or {{}}; in map (attribute: {{ inherit namespace attribute; }}) (builtins.filter (name: builtins.isAttrs values.${{name}} && (values.${{name}}.loom.cargo or null) == \"test\") (builtins.attrNames values)); in select \"checks\" ++ select \"packages\""
            );
            let producers: Vec<Producer> = serde_json::from_slice(&super::query(
                &tool,
                &[
                    "eval",
                    "--json",
                    "--expr",
                    &expression,
                    "--offline",
                    "--no-write-lock-file",
                    "--option",
                    "allow-import-from-derivation",
                    "false",
                ],
            )?)?;
            for producer in producers {
                let subject = inventory
                    .subjects()
                    .values()
                    .find(|subject| {
                        subject.nix_target().is_some_and(|native| {
                            native.flake() == flake
                                && native.system() == &platform.system.value
                                && native.kind().namespace() == producer.namespace
                                && native.attribute().as_str() == producer.attribute
                        })
                    })
                    .ok_or_else(|| Error::Catalogue {
                        detail: "native compiler producer is absent from independent inventory"
                            .into(),
                    })?;
                let definition = Self::hermetic_baseline(
                    snapshot,
                    inventory,
                    subject.name().clone(),
                    subject.name(),
                )?;
                let graph = definition.nix_graph().ok_or(Error::CargoExecution)?;
                if graph.cargo != Some(CargoOperation::Test) {
                    return Err(Error::CargoExecution.into());
                }
                if seen.insert(String::from(graph.identity.clone())) {
                    definitions.push(definition);
                }
            }
        }
        snapshot.require_current()?;
        Ok(definitions)
    }

    /// Bind named non-ignored cases only after actual native compilation and enumeration.
    /// The shared suite still executes its complete native command, not a name-driven shell.
    ///
    /// # Errors
    /// Rejects stale/mismatched producers, absent/ambiguous cases, and ignored cases that
    /// the ordinary native invocation would not execute. No source-only fallback is used.
    pub fn native_rust_cases(
        &self,
        snapshot: &crate::inventory::snapshot::Snapshot,
        inventory: &crate::inventory::Inventory,
        catalogue: &Catalogue<'_>,
        targets: &std::collections::BTreeSet<Name>,
    ) -> Result<Vec<Self>, super::super::Error> {
        super::super::require_context(snapshot, inventory)?;
        let Execution::Nix(graph) = &self.execution else {
            return Err(Error::CargoExecution.into());
        };
        if graph.identity != catalogue.producer.identity
            || self.description().snapshot() != snapshot.identity()
            || graph.cargo != Some(CargoOperation::Test)
        {
            return Err(Error::CargoExecution.into());
        }
        catalogue.require_current()?;
        let mut definitions = Vec::new();
        for target in targets {
            let case = catalogue.resolve_captured(target)?;
            if !case.runnable {
                return Err(Error::Catalogue {
                    detail: format!(
                        "native case `{}` is not a runnable nonignored test in the actual producer mode",
                        target.as_str()
                    ),
                }
                .into());
            }
            let suite = &catalogue.tests.rust_suites[case.binary];
            let owner = Name::try_from(format!(
                "cargo:{}:{}:{}",
                suite.package_name,
                suite.binary.kind.as_str(),
                suite.binary.binary_name
            ))?;
            if !self
                .description()
                .responsibilities()
                .iter()
                .any(|responsibility| {
                    responsibility.subject() == &owner
                        && responsibility.responsible() == &owner
                        && responsibility.duty() == crate::inventory::Duty::Test
                })
            {
                return Err(Error::Catalogue {
                    detail: format!(
                        "compiled case `{}` is outside the suite's actual Rust responsibility",
                        target.as_str()
                    ),
                }
                .into());
            }
            let report_target = Name::try_from(format!(
                "{}::{}${}",
                suite.package_name, suite.binary.binary_name, case.name
            ))?;
            let mut definition = self.clone().named(target.clone())?;
            definition.execution = Execution::Report {
                graph: graph.clone(),
                target: report_target,
            };
            definitions.push(definition);
        }
        catalogue.require_current()?;
        snapshot.require_current()?;
        Ok(definitions)
    }
}

fn fingerprint(
    producer: &Digest,
    cargo: &[u8],
    tests: &[u8],
    profile: &profile::Profile,
) -> Result<Digest, Error> {
    let hashes: Vec<_> = [Digest::of(cargo), Digest::of(tests)]
        .into_iter()
        .chain(profile.bytes()?.iter().map(|bytes| Digest::of(bytes)))
        .collect();
    Ok(Digest::of(&serde_json::to_vec(&(producer, hashes))?))
}

fn read(path: &std::path::Path) -> Result<Vec<u8>, Error> {
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(Error::Catalogue {
            detail: format!("native metadata is not a regular file: {}", path.display()),
        });
    }
    Ok(std::fs::read(path)?)
}

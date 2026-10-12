//! Hermetic execution is bound to a captured source and the actual immutable derivation graph.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use base64::Engine as _;
use displaydoc::Display;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::tracked::{Observation, Observed, Outcome};
use super::{Candidate, Capability, Definition, Digest, Execution, Origin, wire};
use crate::inventory::snapshot::{ResourcePath, Snapshot};
use crate::inventory::{Duty, Inventory, Name};

/// One immutable Nix store object, never an arbitrary host path or installable expression.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct StorePath(String);

impl TryFrom<String> for StorePath {
    type Error = Error;

    fn try_from(value: String) -> Result<Self, Error> {
        let base = value.strip_prefix("/nix/store/").ok_or(Error::StorePath)?;
        if base.len() < 34
            || !base.as_bytes()[..32]
                .iter()
                .all(|byte| b"0123456789abcdfghijklmnpqrsvwxyz".contains(byte))
            || base.as_bytes()[32] != b'-'
            || !base[33..]
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"+-._=".contains(&byte))
        {
            return Err(Error::StorePath);
        }
        Ok(Self(value))
    }
}

impl StorePath {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn basename(value: &str) -> Result<Self, Error> {
        Self::try_from(format!("/nix/store/{value}"))
    }

    fn root(path: &Path) -> Result<Self, Error> {
        if !path.is_absolute()
            || path
                .components()
                .skip(1)
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(Error::StorePath);
        }
        let root: PathBuf = path.components().take(4).collect();
        Self::try_from(root.to_str().ok_or(Error::StorePath)?.to_owned())
    }
}

impl Definition {
    /// Construct from one discovered native check; the captured project is the staged source.
    ///
    /// # Errors
    /// Rejects absent native targets, missing responsibility, omitted source and unsafe graphs.
    pub fn hermetic(
        snapshot: &Snapshot,
        inventory: &Inventory,
        target: Name,
        native: &Name,
        subjects: &BTreeSet<Name>,
        duties: &BTreeSet<Duty>,
    ) -> Result<Self, super::Error> {
        super::require_context(snapshot, inventory)?;
        let subject = inventory.subject(native)?;
        let binding = subject.nix_target().ok_or_else(|| Error::Native {
            subject: native.clone(),
        })?;
        let mut inputs = BTreeMap::<ResourcePath, BTreeSet<Origin>>::new();
        let directory = binding.flake().as_path().parent().ok_or(Error::StorePath)?;
        for name in subjects {
            let subject = inventory.subject(name)?;
            if subject
                .members()
                .iter()
                .any(|path| !path.as_path().starts_with(directory))
            {
                return Err(Error::Outside {
                    subject: name.clone(),
                }
                .into());
            }
        }
        let responsibilities = super::responsibility(inventory, &target, subjects, duties)?;
        for subject in inventory.subjects().values() {
            super::add_subject_inputs(&mut inputs, subject);
        }
        inputs
            .entry(binding.flake().clone())
            .or_default()
            .insert(Origin::Definition(binding.flake().clone()));
        let inputs = super::capture_inputs(snapshot, inputs)?;
        let graph =
            Graph::construct(snapshot, binding).map_err(|source| super::Error::Construction {
                target: target.clone(),
                producer: binding.flake().clone(),
                source: Box::new(source.into()),
            })?;
        let executor = StorePath::root(&graph.tool)?;
        let binary = ResourcePath::try_from(
            graph
                .tool
                .strip_prefix(executor.as_str())
                .map_err(|_| Error::StorePath)?
                .to_path_buf(),
        )?;
        let execution = wire::Nix {
            flake: binding.flake().clone(),
            attribute: binding.attribute().clone(),
            system: binding.system().clone(),
            graph: graph.identity.clone(),
            derivation: graph.root.clone(),
            source: graph.source.clone(),
            executor,
            binary,
            artifact: graph.artifact.clone(),
        };
        let definition = Digest::of(&serde_json::to_vec(&(
            snapshot.identity(),
            &target,
            &responsibilities,
            &inputs,
            &execution,
        ))?);
        Ok(Self {
            candidate: Candidate::HermeticNix {
                contract: wire::Contract {
                    target,
                    snapshot: snapshot.identity().clone(),
                    definition,
                    origin: binding.flake().clone(),
                    responsibilities,
                    inputs,
                },
                execution,
            },
            execution: Execution::Nix(Arc::new(graph)),
        })
    }
}

/// A graph is constructed by evaluating the captured native definition, not deserialized by callers.
#[derive(Debug)]
pub struct Graph {
    tool: PathBuf,
    artifact: Digest,
    root: StorePath,
    source: StorePath,
    nodes: BTreeMap<StorePath, Node>,
    resources: BTreeMap<StorePath, Resource>,
    identity: Digest,
}

/// Canonical NAR content identity for a native source or execution tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct NarHash(String);

impl TryFrom<String> for NarHash {
    type Error = Error;

    fn try_from(value: String) -> Result<Self, Error> {
        let encoded = value.strip_prefix("sha256-").ok_or(Error::NarHash)?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| Error::NarHash)?;
        if decoded.len() != 32
            || base64::engine::general_purpose::STANDARD.encode(decoded) != encoded
        {
            return Err(Error::NarHash);
        }
        Ok(Self(value))
    }
}

/// Resource contents and runtime references are observed from the same native store graph.
#[derive(Debug, Serialize, Deserialize)]
pub struct Resource {
    #[serde(rename = "narHash")]
    content: NarHash,
    references: BTreeSet<StorePath>,
}

impl Resource {
    pub const fn content(&self) -> &NarHash {
        &self.content
    }
    pub const fn references(&self) -> &BTreeSet<StorePath> {
        &self.references
    }
}

#[derive(Debug, Serialize)]
struct Node {
    builder: String,
    system: Name,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    sources: BTreeSet<StorePath>,
    dependencies: BTreeMap<StorePath, BTreeSet<Name>>,
    outputs: BTreeMap<Name, StorePath>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u8,
    derivations: BTreeMap<String, RawNode>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNode {
    version: u8,
    #[serde(rename = "name")]
    _name: Name,
    builder: String,
    system: Name,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    inputs: Inputs,
    outputs: BTreeMap<Name, RawOutput>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Inputs {
    srcs: Vec<String>,
    drvs: BTreeMap<String, Dependency>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Dependency {
    outputs: BTreeSet<Name>,
    #[serde(rename = "dynamicOutputs")]
    dynamic: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOutput {
    path: Option<String>,
    hash: Option<String>,
    method: Option<String>,
}

#[derive(Deserialize)]
struct Metadata {
    path: StorePath,
}

impl Graph {
    fn construct(
        snapshot: &Snapshot,
        native: &crate::inventory::nix::Target,
    ) -> Result<Self, Error> {
        let tool = locate_tool()?;
        let artifact = Digest::of(&std::fs::read(&tool)?);
        let staged = snapshot.stage()?;
        let directory = native.flake().as_path().parent().ok_or(Error::StorePath)?;
        let reference = format!("path:{}", staged.path().join(directory).display());
        let metadata: Metadata = serde_json::from_slice(&query(
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
        let attribute = format!(
            "checks.{}.{}.drvPath",
            serde_json::to_string(native.system().as_str())?,
            serde_json::to_string(native.attribute().as_str())?
        );
        let installable = format!("{reference}#{attribute}");
        let root = String::from_utf8(query(
            &tool,
            &[
                "eval",
                "--raw",
                &installable,
                "--offline",
                "--no-write-lock-file",
                "--option",
                "allow-import-from-derivation",
                "false",
            ],
        )?)
        .map_err(|_| Error::Encoding)?
        .try_into()?;
        let nodes = read_graph(&tool, &root)?;
        check_graph(&nodes, &root, &metadata.path)?;
        let system = &nodes.get(&root).ok_or(Error::Root)?.system;
        if system != native.system() {
            return Err(Error::System {
                expected: native.system().clone(),
                actual: system.clone(),
            });
        }
        let resources = read_resources(&tool, &nodes)?;
        let identity = Digest::of(&serde_json::to_vec(&(
            &root,
            &metadata.path,
            &nodes,
            &resources,
            &tool,
            &artifact,
        ))?);
        snapshot.require_current()?;
        Ok(Self {
            tool,
            artifact,
            root,
            source: metadata.path,
            nodes,
            resources,
            identity,
        })
    }

    pub const fn identity(&self) -> &Digest {
        &self.identity
    }

    pub const fn derivation(&self) -> &StorePath {
        &self.root
    }

    pub const fn source(&self) -> &StorePath {
        &self.source
    }

    pub fn dependencies(&self) -> BTreeSet<&StorePath> {
        self.nodes.keys().chain(self.resources.keys()).collect()
    }

    pub const fn resources(&self) -> &BTreeMap<StorePath, Resource> {
        &self.resources
    }

    pub(super) fn require_current(&self) -> Result<(), Error> {
        if Digest::of(&std::fs::read(&self.tool)?) != self.artifact {
            return Err(Error::ToolChanged);
        }
        let nodes = read_graph(&self.tool, &self.root)?;
        let resources = read_resources(&self.tool, &nodes)?;
        let identity = Digest::of(&serde_json::to_vec(&(
            &self.root,
            &self.source,
            &nodes,
            &resources,
            &self.tool,
            &self.artifact,
        ))?);
        if identity != self.identity {
            return Err(Error::GraphChanged);
        }
        Ok(())
    }

    fn presence(&self, outputs: &BTreeMap<Name, StorePath>) -> Result<Presence, Error> {
        let mut arguments = vec!["path-info", "--json", "--json-format", "1", "--offline"];
        arguments.extend(outputs.values().map(StorePath::as_str));
        let paths: BTreeMap<StorePath, Option<Validity>> =
            serde_json::from_slice(&query(&self.tool, &arguments)?)?;
        if paths.len() != outputs.len() || outputs.values().any(|path| !paths.contains_key(path)) {
            return Err(Error::Result);
        }
        Ok(if paths.values().any(Option::is_none) {
            Presence::Cold
        } else {
            Presence::Present
        })
    }

    pub(super) fn run(&self, target: Name) -> Result<Observed, Error> {
        self.require_current()?;
        let root = self.nodes.get(&self.root).ok_or(Error::Root)?;
        let platform: Platforms =
            serde_json::from_slice(&query(&self.tool, &["config", "show", "--json"])?)?;
        if root.system != platform.system.value && !platform.extra.value.contains(&root.system) {
            return Ok(Observed::execution(
                target,
                Outcome::Inapplicable {
                    system: root.system.clone(),
                },
                vec![Observation::ExecutionGraph {
                    digest: self.identity.clone(),
                }],
            ));
        }
        let installable = format!("{}^*", self.root.as_str());
        let mut arguments = vec![
            "build",
            &installable,
            "--json",
            "--no-link",
            "--offline",
            "--no-substitute",
            "--option",
            "sandbox",
            "true",
            "--option",
            "sandbox-fallback",
            "false",
            "--option",
            "sandbox-paths",
            "",
            "--option",
            "extra-sandbox-paths",
            "",
            "--option",
            "builders",
            "",
        ];
        if matches!(self.presence(&root.outputs)?, Presence::Present) {
            arguments.push("--rebuild");
        }
        let output = invoke(&self.tool, &arguments)?;
        let mut observations = vec![Observation::ExecutionGraph {
            digest: self.identity.clone(),
        }];
        let outcome = if output.status.success() {
            let built: Vec<Built> = serde_json::from_slice(&output.stdout)?;
            let expected = &self.nodes.get(&self.root).ok_or(Error::Root)?.outputs;
            if built.len() != 1 || built[0].drv_path != self.root || &built[0].outputs != expected {
                return Err(Error::Result);
            }
            observations.extend(
                expected
                    .values()
                    .cloned()
                    .map(|path| Observation::Output { path }),
            );
            Outcome::Pass
        } else {
            let detail = diagnostic(&output.stderr);
            if detail
                .contains("does not support the kernel namespaces that are required for sandboxing")
                || detail.contains("unable to start build process")
                    && detail.contains("Operation not permitted")
            {
                Outcome::Unavailable {
                    capability: Capability::NixSandbox,
                    detail,
                }
            } else {
                Outcome::Fail { detail }
            }
        };
        self.require_current()?;
        Ok(Observed::execution(target, outcome, observations))
    }
}

enum Presence {
    Cold,
    Present,
}

#[derive(Deserialize)]
struct Validity {
    #[serde(rename = "narSize")]
    _size: u64,
}

#[derive(Deserialize)]
struct Platforms {
    system: Setting<Name>,
    #[serde(rename = "extra-platforms")]
    extra: Setting<BTreeSet<Name>>,
}

#[derive(Deserialize)]
struct Setting<T> {
    value: T,
}

#[derive(Deserialize)]
struct Built {
    #[serde(rename = "drvPath")]
    drv_path: StorePath,
    outputs: BTreeMap<Name, StorePath>,
}

fn read_graph(tool: &Path, root: &StorePath) -> Result<BTreeMap<StorePath, Node>, Error> {
    let document: Document = serde_json::from_slice(&query(
        tool,
        &["derivation", "show", "--recursive", root.as_str()],
    )?)?;
    if document.version != 4 {
        return Err(Error::Version);
    }
    document
        .derivations
        .into_iter()
        .map(|(path, raw)| {
            if raw.version != 4 {
                return Err(Error::Version);
            }
            let mut outputs = BTreeMap::new();
            for (name, output) in raw.outputs {
                let path = match output.path {
                    Some(path) => StorePath::basename(&path)?,
                    None if output.hash.is_some() && output.method.is_some() => raw
                        .env
                        .get(name.as_str())
                        .ok_or(Error::Output)?
                        .clone()
                        .try_into()?,
                    None => return Err(Error::Output),
                };
                outputs.insert(name, path);
            }
            let dependencies = raw
                .inputs
                .drvs
                .into_iter()
                .map(|(path, dependency)| {
                    if !dependency.dynamic.is_empty() || dependency.outputs.is_empty() {
                        return Err(Error::Dynamic);
                    }
                    Ok((StorePath::basename(&path)?, dependency.outputs))
                })
                .collect::<Result<_, Error>>()?;
            let sources = raw
                .inputs
                .srcs
                .into_iter()
                .map(|path| StorePath::basename(&path))
                .collect::<Result<_, _>>()?;
            Ok((
                StorePath::basename(&path)?,
                Node {
                    builder: raw.builder,
                    system: raw.system,
                    args: raw.args,
                    env: raw.env,
                    sources,
                    dependencies,
                    outputs,
                },
            ))
        })
        .collect()
}

fn read_resources(
    tool: &Path,
    nodes: &BTreeMap<StorePath, Node>,
) -> Result<BTreeMap<StorePath, Resource>, Error> {
    let mut sources: BTreeSet<_> = nodes
        .values()
        .flat_map(|node| node.sources.iter().cloned())
        .collect();
    sources.insert(StorePath::root(tool)?);
    let mut arguments = vec![
        "path-info",
        "--recursive",
        "--json",
        "--json-format",
        "1",
        "--offline",
    ];
    arguments.extend(sources.iter().map(StorePath::as_str));
    let paths: BTreeMap<StorePath, Option<Resource>> =
        serde_json::from_slice(&query(tool, &arguments)?)?;
    for source in &sources {
        if !paths.get(source).is_some_and(Option::is_some) {
            return Err(Error::Resource {
                path: source.clone(),
            });
        }
    }
    let resources: BTreeMap<_, _> = paths
        .into_iter()
        .map(|(path, resource)| {
            let resource = resource.ok_or_else(|| Error::Resource { path: path.clone() })?;
            Ok((path, resource))
        })
        .collect::<Result<_, Error>>()?;
    for resource in resources.values() {
        for reference in &resource.references {
            if !resources.contains_key(reference) {
                return Err(Error::Resource {
                    path: reference.clone(),
                });
            }
        }
    }
    Ok(resources)
}

fn check_graph(
    nodes: &BTreeMap<StorePath, Node>,
    root: &StorePath,
    source: &StorePath,
) -> Result<(), Error> {
    let check = nodes.get(root).ok_or(Error::Root)?;
    if check.env.contains_key("outputHash") {
        return Err(Error::FixedCheck);
    }
    if !nodes.values().any(|node| node.sources.contains(source)) {
        return Err(Error::Source {
            source_path: source.clone(),
        });
    }
    for (path, node) in nodes {
        if node.outputs.is_empty() {
            return Err(Error::Output);
        }
        if [
            "__noChroot",
            "__impure",
            "__json",
            "__darwinAllowLocalNetworking",
            "__darwinImpureHostDeps",
        ]
        .iter()
        .any(|key| {
            node.env
                .get(*key)
                .is_some_and(|value| !value.is_empty() && value != "0")
        }) || node
            .env
            .get("impureEnvVars")
            .is_some_and(|value| !value.is_empty())
            && !node.env.contains_key("outputHash")
        {
            return Err(Error::Unsafe {
                derivation: path.clone(),
            });
        }
        let mut inputs = node.sources.clone();
        for (dependency, outputs) in &node.dependencies {
            let input = nodes.get(dependency).ok_or_else(|| Error::Dependency {
                derivation: dependency.clone(),
            })?;
            for output in outputs {
                inputs.insert(input.outputs.get(output).ok_or(Error::Output)?.clone());
            }
        }
        if node.builder == "builtin:fetchurl" && node.env.contains_key("outputHash") {
            continue;
        }
        let builder = StorePath::root(Path::new(&node.builder))?;
        if !inputs.contains(&builder) {
            return Err(Error::Builder {
                derivation: path.clone(),
            });
        }
    }
    Ok(())
}

fn locate_tool() -> Result<PathBuf, Error> {
    let search = std::env::var_os("PATH").ok_or(Error::Capability)?;
    for directory in std::env::split_paths(&search) {
        let candidate = directory.join("nix");
        if candidate.is_file() {
            let canonical = candidate.canonicalize()?;
            StorePath::root(&canonical)?;
            return Ok(canonical);
        }
    }
    Err(Error::Capability)
}

fn invoke(tool: &Path, args: &[&str]) -> Result<Output, Error> {
    Ok(Command::new(tool)
        .args(args)
        .env_remove("NIX_CONFIG")
        .env_remove("NIX_PATH")
        .args(["--option", "accept-flake-config", "false"])
        .output()?)
}

fn query(tool: &Path, args: &[&str]) -> Result<Vec<u8>, Error> {
    let output = invoke(tool, args)?;
    if !output.status.success() {
        return Err(Error::Query {
            detail: diagnostic(&output.stderr),
        });
    }
    Ok(output.stdout)
}

fn diagnostic(bytes: &[u8]) -> String {
    crate::inputs::bounded_query_diagnostic(&String::from_utf8_lossy(bytes)).replace(
        "use '--no-sandbox' to disable sandboxing",
        "provision the declared sandbox capability and retry `loom gate verify`",
    )
}

#[derive(Debug, Display, Error)]
pub enum Error {
    /// invalid NAR identity; native resources must have canonical SHA256 content hashes
    NarHash,
    /// native resource closure omitted `{path:?}`; provision the declared store inputs and retry `loom gate verify`
    Resource { path: StorePath },
    /// invalid Nix store identity; use a native immutable store resource, not a host path
    StorePath,
    /// subject `{subject:?}` is outside the staged native project; construct its actual resource before offering responsibility and retry `loom gate verify`
    Outside { subject: Name },
    /// native Nix subject `{subject:?}` is not a discovered check; correct the project target and retry `loom gate verify`
    Native { subject: Name },
    /// Nix discovery capability is unavailable; provision Nix and retry `loom gate verify`
    Capability,
    /// Nix resource I/O failed
    Io(#[from] std::io::Error),
    /// captured Nix project changed
    Snapshot(#[from] crate::inventory::snapshot::Error),
    /// malformed native derivation graph; investigate the producer with `nix derivation show --recursive` and retry `loom gate verify`
    Json(#[from] serde_json::Error),
    /// native Nix query failed: {detail}; correct the definition or provision its declared inputs and retry `loom gate verify`
    Query { detail: String },
    /// native check system `{expected:?}` disagrees with derivation system `{actual:?}`; correct the output binding and retry `loom gate verify`
    System { expected: Name, actual: Name },
    /// native Nix query was not UTF-8
    Encoding,
    /// unsupported derivation graph version; provision Nix with graph format 4 and retry `loom gate verify`
    Version,
    /// derivation graph omitted its requested root
    Root,
    /// derivation graph omits captured project source `{source_path:?}`; stage the checked project in the native check and retry `loom gate verify`
    Source { source_path: StorePath },
    /// a deterministic check cannot be a network-enabled fixed-output derivation; construct a sandboxed check over pinned resources and retry `loom gate verify`
    FixedCheck,
    /// derivation graph has unresolved outputs; supply fixed native output mappings and retry `loom gate verify`
    Output,
    /// dynamic derivation dependencies are unsupported; use a statically constructed graph and retry `loom gate verify`
    Dynamic,
    /// derivation `{derivation:?}` requests impure or unsandboxed execution; correct the deterministic definition and retry `loom gate verify`
    Unsafe { derivation: StorePath },
    /// graph omitted transitive derivation `{derivation:?}`; repair native construction and retry `loom gate verify`
    Dependency { derivation: StorePath },
    /// derivation `{derivation:?}` builder is outside its constructed inputs; pin the actual tool resource and retry `loom gate verify`
    Builder { derivation: StorePath },
    /// Nix tool changed after construction; regenerate the checked definition and retry `loom gate verify`
    ToolChanged,
    /// Nix graph changed after construction; regenerate the checked definition and retry `loom gate verify`
    GraphChanged,
    /// Nix returned inconsistent derivation/output results; investigate the executor and retry `loom gate verify`
    Result,
}

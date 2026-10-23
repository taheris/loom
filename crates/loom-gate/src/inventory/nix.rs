//! Nix evaluation and check subjects from current native project definitions.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::snapshot::{ResourcePath, Snapshot};
use super::{Error, Name, Role, Subject, command, insert, single};

/// Native output namespaces have distinct execution and obligation contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Check,
    Package,
    App,
}

impl Kind {
    pub const fn namespace(self) -> &'static str {
        match self {
            Self::Check => "checks",
            Self::Package => "packages",
            Self::App => "apps",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Package => "package",
            Self::App => "app",
        }
    }
}

/// Native outputs distinguish applicable native duties from foreign-system applicability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Usage {
    Verification,
    Inapplicable,
}

/// Exact native execution binding; subject names are diagnostic identities, not command syntax.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Target {
    flake: ResourcePath,
    kind: Kind,
    project: PathBuf,
    system: Name,
    attribute: Name,
    usage: Usage,
}

impl Target {
    pub(crate) fn project(&self) -> &Path {
        &self.project
    }
    pub const fn kind(&self) -> Kind {
        self.kind
    }
    pub const fn usage(&self) -> Usage {
        self.usage
    }
    pub const fn flake(&self) -> &ResourcePath {
        &self.flake
    }
    pub const fn system(&self) -> &Name {
        &self.system
    }
    pub const fn attribute(&self) -> &Name {
        &self.attribute
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Outputs {
    checks: BTreeMap<String, Vec<String>>,
    packages: BTreeMap<String, Vec<String>>,
    apps: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    system: Option<String>,
    #[serde(default)]
    constructed_checks: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    constructed_packages: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    constructed_apps: BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize)]
struct Metadata {
    url: String,
    path: PathBuf,
}

pub(super) fn discover(
    snapshot: &Snapshot,
    root: &Path,
    subjects: &mut BTreeMap<Name, Subject>,
) -> Result<BTreeMap<Name, std::sync::Arc<crate::provider::nix::Graph>>, Error> {
    let nix_files: BTreeSet<_> = snapshot
        .files()
        .keys()
        .filter(|path| path.as_path().extension().is_some_and(|ext| ext == "nix"))
        .cloned()
        .collect();
    let flakes: Vec<_> = nix_files
        .iter()
        .filter(|path| {
            path.as_path()
                .file_name()
                .is_some_and(|name| name == "flake.nix")
        })
        .collect();
    let mut constructed = Vec::new();
    for flake in &flakes {
        let directory = flake.as_path().parent().unwrap_or_else(|| Path::new(""));
        let members: BTreeSet<_> = nix_files
            .iter()
            .filter(|path| {
                path.as_path().starts_with(directory)
                    && !flakes.iter().any(|other| {
                        other != flake
                            && other.as_path().parent().is_some_and(|nested| {
                                nested.starts_with(directory) && path.as_path().starts_with(nested)
                            })
                    })
            })
            .cloned()
            .collect();
        let project = root.join(directory);
        let reference = format!("path:{}", project.display());
        let metadata: Metadata = serde_json::from_slice(&command(
            &project,
            super::Tool::Nix,
            &[
                "flake",
                "metadata",
                &reference,
                "--json",
                "--no-write-lock-file",
                "--offline",
            ],
        )?)
        .map_err(|source| Error::Definition {
            definition: (*flake).clone(),
            source,
        })?;
        let locked = serde_json::to_string(&metadata.url)
            .map_err(|source| Error::Definition {
                definition: (*flake).clone(),
                source,
            })?
            .replace("${", "\\${");
        let (system, platforms) = {
            #[derive(Deserialize)]
            struct Configuration {
                system: System,
                #[serde(default, rename = "extra-platforms")]
                extra: Extra,
            }
            #[derive(Deserialize, Default)]
            struct Extra {
                #[serde(default)]
                value: BTreeSet<String>,
            }
            #[derive(Deserialize)]
            struct System {
                value: String,
            }
            let configuration: Configuration = serde_json::from_slice(&command(
                &project,
                super::Tool::Nix,
                &["config", "show", "--json"],
            )?)
            .map_err(|source| Error::Definition {
                definition: (*flake).clone(),
                source,
            })?;
            let mut platforms = configuration.extra.value;
            platforms.insert(configuration.system.value.clone());
            (configuration.system.value, platforms)
        };
        let system = serde_json::to_string(&system).map_err(|source| Error::Definition {
            definition: (*flake).clone(),
            source,
        })?;
        let platform_names =
            serde_json::to_string(&platforms).map_err(|source| Error::Definition {
                definition: (*flake).clone(),
                source,
            })?;
        let platform_names = serde_json::to_string(&platform_names)
            .map_err(|source| Error::Definition {
                definition: (*flake).clone(),
                source,
            })?
            .replace("${", "\\${");
        let expression = format!(
            "let flake = builtins.getFlake {locked}; names = namespace: builtins.mapAttrs (_system: entries: builtins.attrNames entries) (flake.${{namespace}} or {{}}); constructed = namespace: builtins.mapAttrs (system: entries: if builtins.elem system (builtins.fromJSON {platform_names}) then builtins.filter (name: let probe = builtins.tryEval (builtins.isAttrs entries.${{name}} && (if namespace == \"apps\" then (entries.${{name}}.meta or {{}}) ? loom else entries.${{name}} ? loom)); in probe.success && probe.value) (builtins.attrNames entries) else []) (flake.${{namespace}} or {{}}); in {{ checks = names \"checks\"; packages = names \"packages\"; apps = names \"apps\"; system = {system}; constructed_checks = constructed \"checks\"; constructed_packages = constructed \"packages\"; constructed_apps = constructed \"apps\"; }}"
        );
        let outputs: Outputs = serde_json::from_slice(&command(
            &project,
            super::Tool::Nix,
            &[
                "eval",
                "--json",
                "--expr",
                &expression,
                "--no-write-lock-file",
                "--offline",
                "--option",
                "allow-import-from-derivation",
                "false",
            ],
        )?)
        .map_err(|source| Error::Definition {
            definition: (*flake).clone(),
            source,
        })?;
        let mut checks = BTreeSet::new();
        for (kind, systems) in [
            (Kind::Check, outputs.checks),
            (Kind::Package, outputs.packages),
            (Kind::App, outputs.apps),
        ] {
            for (system, entries) in systems {
                for attribute in entries {
                    let name = Name::try_from(format!(
                        "nix:{}:{}:{system}:{attribute}",
                        kind.label(),
                        flake.as_path().display()
                    ))?;
                    let supported = match kind {
                        Kind::Check => outputs.constructed_checks.get(&system),
                        Kind::Package => outputs.constructed_packages.get(&system),
                        Kind::App => outputs.constructed_apps.get(&system),
                    };
                    let current = outputs
                        .system
                        .as_ref()
                        .is_none_or(|current| current == &system);
                    let registered = supported.is_some_and(|entries| entries.contains(&attribute));
                    let usage = if current {
                        Usage::Verification
                    } else {
                        Usage::Inapplicable
                    };
                    if usage == Usage::Verification {
                        checks.insert(name.clone());
                    }
                    // Extra native backends require the same actual resource
                    // provenance, not repeated isolated one-output reconstruction.
                    // Unsupported platforms retain separate applicability duties.
                    if registered && platforms.contains(&system) {
                        constructed.push(name.clone());
                    }
                    insert(
                        subjects,
                        Subject {
                            name,
                            role: Role::NixSource,
                            members: members.clone(),
                            consumers: BTreeSet::new(),
                            producer: None,
                            testable: false,
                            origin: (*flake).clone(),
                            nix: Some(Target {
                                flake: (*flake).clone(),
                                kind,
                                project: metadata.path.clone(),
                                system: Name::try_from(system.clone())?,
                                attribute: Name::try_from(attribute)?,
                                usage,
                            }),
                        },
                    )?;
                }
            }
        }
        insert(
            subjects,
            Subject {
                name: Name::try_from(format!("nix:flake:{}", flake.as_path().display()))?,
                role: Role::NixSource,
                members,
                consumers: checks,
                producer: None,
                testable: false,
                origin: (*flake).clone(),
                nix: None,
            },
        )?;
    }
    for path in &nix_files {
        insert(subjects, single(path, Role::NixSource, "nix")?)?;
    }
    discover_resources(snapshot, subjects, &constructed)
}

fn discover_resources(
    snapshot: &Snapshot,
    subjects: &mut BTreeMap<Name, Subject>,
    constructed: &[Name],
) -> Result<BTreeMap<Name, std::sync::Arc<crate::provider::nix::Graph>>, Error> {
    let accounted: BTreeSet<_> = subjects
        .values()
        .flat_map(|subject| subject.members.iter().cloned())
        .collect();
    let mut consumers = BTreeMap::<ResourcePath, BTreeSet<Name>>::new();
    let mut evaluation = BTreeSet::new();
    let mut graphs = BTreeMap::new();
    let bindings = constructed
        .iter()
        .map(|name| {
            subjects[name]
                .nix_target()
                .cloned()
                .ok_or_else(|| Error::Unknown { name: name.clone() })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let prepared =
        crate::provider::nix::Graph::construct_many(snapshot, &bindings).map_err(|source| {
            if let [target] = constructed {
                Error::NativeResource {
                    target: target.clone(),
                    source: Box::new(source),
                }
            } else {
                Error::NativeResources {
                    targets: constructed.to_vec(),
                    source: Box::new(source),
                }
            }
        })?;
    for (name, graph) in constructed.iter().zip(prepared) {
        for path in graph.origins().union(graph.evaluation_origins()) {
            if !accounted.contains(path)
                && !super::is_document(path)
                && (path.as_path().extension().is_none_or(|ext| ext != "rs")
                    || (graph.explicit_file_origins().contains(path)
                        && graph.origins().contains(path)))
            {
                if graph.evaluation_origins().contains(path)
                    && path.as_path().extension().is_none_or(|ext| ext != "rs")
                {
                    evaluation.insert(path.clone());
                }
                consumers
                    .entry(path.clone())
                    .or_default()
                    .insert(name.clone());
            }
        }
        graphs.insert(name.clone(), std::sync::Arc::new(graph));
    }
    for (path, consumers) in consumers {
        let role = if evaluation.contains(&path) || super::is_configuration(&path) {
            Role::Configuration
        } else {
            Role::RuntimeResource
        };
        let mut subject = single(&path, role, "resource")?;
        subject.consumers = consumers;
        insert(subjects, subject)?;
    }
    Ok(graphs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_workspace_packages_construct_complete_native_graphs_from_captured_definitions() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap()
            .to_path_buf();
        let snapshot = Snapshot::discover(&root).unwrap();
        let staged = snapshot.stage().unwrap();
        let inventory = super::super::Inventory::discover(&snapshot).unwrap();
        let subjects = inventory.subjects();
        let attributes = [
            "verification-build",
            "verification-clippy",
            "verification-clippy-all-targets",
            "verification-nextest",
        ];
        for system in ["aarch64-darwin", "aarch64-linux", "x86_64-linux"] {
            for attribute in attributes {
                let target =
                    Name::try_from(format!("nix:package:flake.nix:{system}:{attribute}")).unwrap();
                let binding = subjects[&target].nix_target().unwrap();
                assert_eq!(binding.system().as_str(), system);
                assert_eq!(binding.attribute().as_str(), attribute);
            }
        }
        let config: serde_json::Value = serde_json::from_slice(
            &command(
                staged.path(),
                super::super::Tool::Nix,
                &["config", "show", "--json"],
            )
            .unwrap(),
        )
        .unwrap();
        let system = config["system"]["value"].as_str().unwrap();
        let mut responsibilities = BTreeSet::new();
        for attribute in attributes {
            let target =
                Name::try_from(format!("nix:package:flake.nix:{system}:{attribute}")).unwrap();
            let graph = inventory.native_graph(&target).unwrap();
            assert!(!graph.origins().is_empty());
            assert!(!graph.outputs().unwrap().is_empty());
            assert!(!graph.resources().is_empty());
            assert_eq!(
                graph.catalogue_derivation().is_some(),
                attribute == "verification-nextest"
            );
            let definition = crate::provider::Definition::hermetic_baseline(
                &snapshot,
                &inventory,
                target.clone(),
                &target,
            )
            .unwrap();
            responsibilities.extend(definition.description().responsibilities().iter().cloned());
        }
        for attribute in ["worker-native-preflight", "test-app-hook-runner"] {
            let target =
                Name::try_from(format!("nix:check:flake.nix:{system}:{attribute}")).unwrap();
            let definition = crate::provider::Definition::hermetic_baseline(
                &snapshot,
                &inventory,
                target.clone(),
                &target,
            )
            .unwrap();
            assert!(
                definition
                    .description()
                    .responsibilities()
                    .iter()
                    .all(|responsibility| {
                        matches!(
                            responsibility.duty(),
                            super::super::Duty::Evaluate | super::super::Duty::Check
                        )
                    })
            );
        }
        for obligation in inventory.obligations().iter().filter(|obligation| {
            inventory.subjects()[obligation.responsible()].role() == Role::RustSource
        }) {
            assert!(
                responsibilities.iter().any(|responsibility| {
                    responsibility.subject() == obligation.subject()
                        && responsibility.duty() == obligation.duty()
                        && responsibility.responsible() == obligation.responsible()
                }),
                "missing actual Cargo execution for {obligation:?}"
            );
        }
    }
}

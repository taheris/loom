//! Cargo target ownership and native project resource membership.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use syn::{Expr, Item, Lit, Meta};

use super::snapshot::{ResourcePath, Snapshot};
use super::{Error, Name, Role, Subject, command, insert, relative};

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_members: BTreeSet<String>,
}

#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    manifest_path: PathBuf,
    targets: Vec<Target>,
    metadata: serde_json::Value,
}

#[derive(Deserialize)]
struct Target {
    name: String,
    kind: Vec<Kind>,
    src_path: PathBuf,
    test: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Kind {
    Lib,
    Rlib,
    Dylib,
    Cdylib,
    Staticlib,
    ProcMacro,
    Bin,
    Test,
    Example,
    Bench,
    CustomBuild,
}

impl Kind {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Lib => "lib",
            Self::Rlib => "rlib",
            Self::Dylib => "dylib",
            Self::Cdylib => "cdylib",
            Self::Staticlib => "staticlib",
            Self::ProcMacro => "proc-macro",
            Self::Bin => "bin",
            Self::Test => "test",
            Self::Example => "example",
            Self::Bench => "bench",
            Self::CustomBuild => "custom-build",
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Policy {
    #[serde(default)]
    resource: Vec<Resource>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resource {
    name: Name,
    directory: ResourcePath,
    role: Role,
    consumers: BTreeSet<Name>,
    producer: Option<Name>,
}

pub(super) fn discover(
    snapshot: &Snapshot,
    root: &Path,
    subjects: &mut BTreeMap<Name, Subject>,
) -> Result<(), Error> {
    let manifest = ResourcePath::try_from(PathBuf::from("Cargo.toml"))?;
    if !snapshot.files().contains_key(&manifest) {
        return Ok(());
    }
    let metadata: Metadata = serde_json::from_slice(&command(
        root,
        super::Tool::Cargo,
        &[
            "metadata",
            "--format-version=1",
            "--no-deps",
            "--offline",
            "--locked",
        ],
    )?)
    .map_err(|source| Error::Definition {
        definition: manifest,
        source,
    })?;
    for package in metadata
        .packages
        .iter()
        .filter(|package| metadata.workspace_members.contains(&package.id))
    {
        discover_package(snapshot, root, subjects, package)?;
    }
    Ok(())
}

fn discover_package(
    snapshot: &Snapshot,
    root: &Path,
    subjects: &mut BTreeMap<Name, Subject>,
    package: &Package,
) -> Result<(), Error> {
    let origin = relative(root, &package.manifest_path)?;
    let mut targets = BTreeSet::new();
    let mut consumers = BTreeSet::new();
    for target in &package.targets {
        let name = Name::try_from(format!(
            "cargo:{}:{}:{}",
            package.name,
            target
                .kind
                .iter()
                .map(Kind::as_str)
                .collect::<Vec<_>>()
                .join("+"),
            target.name
        ))?;
        let source = relative(root, &target.src_path)?;
        let mut members = BTreeSet::new();
        collect(snapshot, &source, true, &mut members)?;
        targets.insert(name.clone());
        if target.test {
            consumers.insert(name.clone());
        }
        insert(
            subjects,
            Subject {
                name,
                role: Role::RustSource,
                members,
                consumers: BTreeSet::new(),
                producer: None,
                testable: target.test,
                origin: origin.clone(),
                nix: None,
            },
        )?;
    }
    for name in &targets {
        if let Some(subject) = subjects.get_mut(name) {
            subject.consumers.clone_from(&consumers);
        }
    }
    let directory = package
        .manifest_path
        .parent()
        .ok_or_else(|| Error::Outside {
            path: package.manifest_path.clone(),
        })?;
    for fixture_directory in [
        directory.join("tests/fixtures"),
        directory.join("tests/snapshots"),
    ] {
        let path = relative(root, &fixture_directory)?;
        let members = membership(snapshot, &path);
        if !members.is_empty() {
            insert(
                subjects,
                Subject {
                    name: Name::try_from(format!(
                        "cargo:{}:resource:{}",
                        package.name,
                        fixture_directory
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                    ))?,
                    role: Role::TestFixture,
                    members,
                    consumers: consumers.clone(),
                    producer: None,
                    testable: false,
                    origin: origin.clone(),
                    nix: None,
                },
            )?;
        }
    }
    let policy: Policy = match package
        .metadata
        .get("loom")
        .and_then(|loom| loom.get("verify"))
    {
        Some(value) => {
            serde_json::from_value(value.clone()).map_err(|source| Error::Definition {
                definition: origin.clone(),
                source,
            })?
        }
        None => Policy::default(),
    };
    for resource in policy.resource {
        let resource_path = relative(root, &directory.join(resource.directory.as_path()))?;
        let members = membership(snapshot, &resource_path);
        if members.is_empty() {
            return Err(super::snapshot::Error::Missing {
                path: resource_path,
            }
            .into());
        }
        for name in resource.consumers.iter().chain(resource.producer.iter()) {
            if !targets.contains(name) {
                return Err(Error::Unknown { name: name.clone() });
            }
        }
        if !matches!(
            resource.role,
            Role::TestFixture | Role::Generated | Role::Document
        ) {
            return Err(Error::Responsibility {
                name: resource.name,
                role: resource.role,
            });
        }
        if resource.role == Role::Document {
            for path in &members {
                if !super::is_document(path) {
                    return Err(Error::Unclassified { path: path.clone() });
                }
            }
        }
        insert(
            subjects,
            Subject {
                name: resource.name,
                role: resource.role,
                members,
                consumers: resource.consumers,
                producer: resource.producer,
                testable: false,
                origin: origin.clone(),
                nix: None,
            },
        )?;
    }
    Ok(())
}

fn membership(snapshot: &Snapshot, directory: &ResourcePath) -> BTreeSet<ResourcePath> {
    snapshot
        .files()
        .keys()
        .filter(|path| path.as_path().starts_with(directory.as_path()))
        .cloned()
        .collect()
}

fn collect(
    snapshot: &Snapshot,
    path: &ResourcePath,
    root: bool,
    members: &mut BTreeSet<ResourcePath>,
) -> Result<(), Error> {
    if !members.insert(path.clone()) {
        return Ok(());
    }
    let file = syn::parse_file(snapshot.text(path)?).map_err(|source| Error::Syntax {
        path: path.clone(),
        source,
    })?;
    let parent = path.as_path().parent().unwrap_or_else(|| Path::new(""));
    let module_directory = if root
        || path
            .as_path()
            .file_name()
            .is_some_and(|name| name == "mod.rs")
    {
        parent.to_path_buf()
    } else {
        parent.join(path.as_path().file_stem().unwrap_or_default())
    };
    collect_items(snapshot, &file.items, parent, &module_directory, members)?;
    let mut includes = Includes {
        paths: Vec::new(),
        error: None,
    };
    syn::visit::Visit::visit_file(&mut includes, &file);
    if let Some(source) = includes.error {
        return Err(Error::Syntax {
            path: path.clone(),
            source,
        });
    }
    for (include, source) in includes.paths {
        let include = normalize(&parent.join(include))?;
        if source {
            collect(snapshot, &include, false, members)?;
        } else {
            if !snapshot.files().contains_key(&include) {
                return Err(super::snapshot::Error::Missing { path: include }.into());
            }
            members.insert(include);
        }
    }
    Ok(())
}

struct Includes {
    paths: Vec<(String, bool)>,
    error: Option<syn::Error>,
}

impl<'ast> syn::visit::Visit<'ast> for Includes {
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if node.path.is_ident("include")
            || node.path.is_ident("include_str")
            || node.path.is_ident("include_bytes")
        {
            match syn::parse2::<syn::LitStr>(node.tokens.clone()) {
                Ok(path) => self
                    .paths
                    .push((path.value(), node.path.is_ident("include"))),
                Err(error) => self.error = Some(error),
            }
        } else {
            syn::visit::visit_macro(self, node);
        }
    }
}

fn normalize(path: &Path) -> Result<ResourcePath, Error> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir if normalized.pop() => {}
            _ => {
                return Err(Error::Outside {
                    path: path.to_path_buf(),
                });
            }
        }
    }
    Ok(ResourcePath::try_from(normalized)?)
}

fn collect_items(
    snapshot: &Snapshot,
    items: &[Item],
    file_directory: &Path,
    module_directory: &Path,
    members: &mut BTreeSet<ResourcePath>,
) -> Result<(), Error> {
    for item in items {
        let Item::Mod(module) = item else { continue };
        if let Some((_, items)) = &module.content {
            let inline_directory = module_directory.join(module.ident.to_string());
            collect_items(
                snapshot,
                items,
                &inline_directory,
                &inline_directory,
                members,
            )?;
            continue;
        }
        let explicit = module
            .attrs
            .iter()
            .find_map(|attribute| match &attribute.meta {
                Meta::NameValue(value) if value.path.is_ident("path") => match &value.value {
                    Expr::Lit(value) => match &value.lit {
                        Lit::Str(value) => Some(value.value()),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            });
        let path = if let Some(explicit) = explicit {
            file_directory.join(explicit)
        } else {
            let plain = module_directory.join(format!("{}.rs", module.ident));
            let nested = module_directory
                .join(module.ident.to_string())
                .join("mod.rs");
            let plain_path = ResourcePath::try_from(plain.clone())?;
            let nested_path = ResourcePath::try_from(nested.clone())?;
            match (
                snapshot.files().contains_key(&plain_path),
                snapshot.files().contains_key(&nested_path),
            ) {
                (true, true) => return Err(Error::Module { path: plain }),
                (true, false) => plain,
                _ => nested,
            }
        };
        collect(snapshot, &normalize(&path)?, false, members)?;
    }
    Ok(())
}

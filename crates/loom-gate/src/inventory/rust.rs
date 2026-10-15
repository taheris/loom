//! Cargo target ownership and native project resource membership.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use syn::ext::IdentExt as _;
use syn::{Expr, Item, Lit, Meta};

use super::snapshot::{ResourcePath, Snapshot};
use super::{Error, Name, Role, Subject, command, insert, relative};

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_members: BTreeSet<String>,
    workspace_root: PathBuf,
    #[serde(rename = "metadata")]
    workspace_policy: serde_json::Value,
}

#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    manifest_path: PathBuf,
    targets: Vec<Target>,
    metadata: serde_json::Value,
    dependencies: Vec<Dependency>,
}

#[derive(Deserialize)]
struct Dependency {
    path: Option<PathBuf>,
    kind: Option<DependencyKind>,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum DependencyKind {
    Normal,
    Build,
    Dev,
}

/// Native production compilation ownership retained independently of role exceptions.
#[derive(Debug, Clone)]
pub(super) struct Compilation {
    pub subjects: BTreeSet<Name>,
    pub configuration: BTreeSet<ResourcePath>,
    pub outside: BTreeSet<PathBuf>,
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
    #[serde(default)]
    consumers: BTreeSet<Name>,
    producer: Option<Name>,
}

pub(super) fn discover(
    snapshot: &Snapshot,
    root: &Path,
    subjects: &mut BTreeMap<Name, Subject>,
) -> Result<BTreeMap<ResourcePath, Compilation>, Error> {
    let manifest = ResourcePath::try_from(PathBuf::from("Cargo.toml"))?;
    if !snapshot.files().contains_key(&manifest) {
        return Ok(BTreeMap::new());
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
    let workspace_origin = relative(root, &metadata.workspace_root.join("Cargo.toml"))?;
    let targets = subjects
        .values()
        .filter(|subject| subject.role == Role::RustSource)
        .map(|subject| subject.name.clone())
        .collect();
    discover_resources(
        snapshot,
        root,
        subjects,
        &metadata.workspace_policy,
        &workspace_origin,
        &targets,
    )?;
    compilations(snapshot, root, &metadata)
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
        let name = target_name(package, target)?;
        let source = relative(root, &target.src_path)?;
        let mut members = BTreeSet::new();
        let directory = origin.as_path().parent().unwrap_or_else(|| Path::new(""));
        collect(snapshot, &source, directory, true, &mut members)?;
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
    discover_resources(
        snapshot,
        root,
        subjects,
        &package.metadata,
        &origin,
        &targets,
    )
}

fn discover_resources(
    snapshot: &Snapshot,
    root: &Path,
    subjects: &mut BTreeMap<Name, Subject>,
    metadata: &serde_json::Value,
    origin: &ResourcePath,
    targets: &BTreeSet<Name>,
) -> Result<(), Error> {
    let directory = root
        .join(origin.as_path())
        .parent()
        .ok_or_else(|| Error::Outside {
            path: origin.as_path().to_path_buf(),
        })?
        .to_path_buf();
    let policy: Policy = match metadata.get("loom").and_then(|loom| loom.get("verify")) {
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

fn target_name(package: &Package, target: &Target) -> Result<Name, Error> {
    Name::try_from(format!(
        "cargo:{}:{}:{}",
        package.name,
        target
            .kind
            .iter()
            .map(Kind::as_str)
            .collect::<Vec<_>>()
            .join("+"),
        target.name
    ))
}

fn production(target: &Target, owning: bool) -> bool {
    target.kind.iter().any(|kind| match kind {
        Kind::Lib
        | Kind::Rlib
        | Kind::Dylib
        | Kind::Cdylib
        | Kind::Staticlib
        | Kind::ProcMacro
        | Kind::CustomBuild => true,
        Kind::Bin => owning,
        Kind::Test | Kind::Example | Kind::Bench => false,
    })
}

fn compilations(
    snapshot: &Snapshot,
    root: &Path,
    metadata: &Metadata,
) -> Result<BTreeMap<ResourcePath, Compilation>, Error> {
    let packages: BTreeMap<_, _> = metadata
        .packages
        .iter()
        .filter(|package| metadata.workspace_members.contains(&package.id))
        .map(|package| (package.manifest_path.clone(), package))
        .collect();
    let mut result = BTreeMap::new();
    for package in packages.values() {
        let mut compilation = Compilation {
            subjects: BTreeSet::new(),
            configuration: BTreeSet::new(),
            outside: BTreeSet::new(),
        };
        let mut pending = vec![(*package, true)];
        let mut visited = BTreeSet::new();
        while let Some((current, owning)) = pending.pop() {
            if !visited.insert(&current.id) {
                continue;
            }
            let manifest = relative(root, &current.manifest_path)?;
            compilation.configuration.insert(manifest);
            for target in current
                .targets
                .iter()
                .filter(|target| production(target, owning))
            {
                compilation.subjects.insert(target_name(current, target)?);
            }
            for dependency in current
                .dependencies
                .iter()
                .filter(|dependency| dependency.kind != Some(DependencyKind::Dev))
            {
                if let Some(path) = &dependency.path {
                    let dependency_manifest = path.join("Cargo.toml");
                    if let Some(dependency) = packages.get(&dependency_manifest) {
                        pending.push((dependency, false));
                    } else {
                        compilation.outside.insert(dependency_manifest);
                    }
                }
            }
        }
        let manifest_directories: Vec<_> = compilation
            .configuration
            .iter()
            .filter_map(|path| path.as_path().parent())
            .collect();
        let mut configuration = BTreeSet::new();
        for path in snapshot.files().keys() {
            let parent = path.as_path().parent().unwrap_or_else(|| Path::new(""));
            let filename = path.as_path().file_name().and_then(|name| name.to_str());
            let native = matches!(
                filename,
                Some(
                    "Cargo.lock"
                        | "rust-toolchain"
                        | "rust-toolchain.toml"
                        | "clippy.toml"
                        | ".clippy.toml"
                )
            ) && manifest_directories
                .iter()
                .any(|directory| directory.starts_with(parent));
            let cargo_config = matches!(filename, Some("config" | "config.toml"))
                && parent.ends_with(".cargo")
                && parent.parent().is_some_and(|base| {
                    manifest_directories
                        .iter()
                        .any(|directory| directory.starts_with(base))
                });
            if native || cargo_config || path.as_path() == Path::new("Cargo.toml") {
                configuration.insert(path.clone());
            }
        }
        compilation.configuration.extend(configuration);
        result.insert(relative(root, &package.manifest_path)?, compilation);
    }
    Ok(result)
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
    manifest_directory: &Path,
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
    collect_items(
        snapshot,
        &file.items,
        parent,
        &module_directory,
        manifest_directory,
        members,
    )?;
    let mut includes = Includes {
        manifest_directory,
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
        let include = match include.base {
            IncludeBase::Source => parent.join(include.value),
            IncludeBase::Manifest => PathBuf::from(include.value),
        };
        let include = normalize(&include)?;
        if source {
            collect(snapshot, &include, manifest_directory, false, members)?;
        } else {
            if !snapshot.files().contains_key(&include) {
                return Err(super::snapshot::Error::Missing { path: include }.into());
            }
            members.insert(include);
        }
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IncludeBase {
    Source,
    Manifest,
}

struct IncludePath {
    value: String,
    base: IncludeBase,
}

fn include_path(expression: &Expr, manifest_directory: &Path) -> Result<IncludePath, syn::Error> {
    match expression {
        Expr::Lit(value) => match &value.lit {
            Lit::Str(value) => Ok(IncludePath {
                value: value.value(),
                base: IncludeBase::Source,
            }),
            _ => Err(syn::Error::new_spanned(
                expression,
                "include path must be a string",
            )),
        },
        Expr::Macro(value) if value.mac.path.is_ident("env") => {
            let variable = syn::parse2::<syn::LitStr>(value.mac.tokens.clone())?;
            if variable.value() != "CARGO_MANIFEST_DIR" {
                return Err(syn::Error::new_spanned(
                    expression,
                    "include environment must be the discovered CARGO_MANIFEST_DIR; ambient variables cannot establish tracked membership",
                ));
            }
            let directory = manifest_directory.to_str().ok_or_else(|| {
                syn::Error::new_spanned(expression, "Cargo manifest directory is not UTF-8")
            })?;
            Ok(IncludePath {
                value: directory.to_owned(),
                base: IncludeBase::Manifest,
            })
        }
        Expr::Macro(value) if value.mac.path.is_ident("concat") => {
            use syn::parse::Parser as _;
            let arguments = syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated
                .parse2(value.mac.tokens.clone())?;
            let mut result = IncludePath {
                value: String::new(),
                base: IncludeBase::Source,
            };
            for argument in arguments {
                let part = include_path(&argument, manifest_directory)?;
                if part.base == IncludeBase::Manifest {
                    if result.base == IncludeBase::Manifest || !result.value.is_empty() {
                        return Err(syn::Error::new_spanned(
                            expression,
                            "CARGO_MANIFEST_DIR must begin the include path",
                        ));
                    }
                    result.base = IncludeBase::Manifest;
                }
                result.value.push_str(&part.value);
            }
            // The discovered root package directory is the empty relative path.
            if result.base == IncludeBase::Manifest && manifest_directory.as_os_str().is_empty() {
                result.value = result.value.trim_start_matches('/').to_owned();
            }
            Ok(result)
        }
        _ => Err(syn::Error::new_spanned(
            expression,
            "include path requires string literals, concat!, or the discovered CARGO_MANIFEST_DIR",
        )),
    }
}

struct Includes<'a> {
    manifest_directory: &'a Path,
    paths: Vec<(IncludePath, bool)>,
    error: Option<syn::Error>,
}

impl<'ast> syn::visit::Visit<'ast> for Includes<'_> {
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if node.path.is_ident("include")
            || node.path.is_ident("include_str")
            || node.path.is_ident("include_bytes")
        {
            match syn::parse2::<Expr>(node.tokens.clone())
                .and_then(|expression| include_path(&expression, self.manifest_directory))
            {
                Ok(path) => self.paths.push((path, node.path.is_ident("include"))),
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
    manifest_directory: &Path,
    members: &mut BTreeSet<ResourcePath>,
) -> Result<(), Error> {
    for item in items {
        let Item::Mod(module) = item else { continue };
        if let Some((_, items)) = &module.content {
            let inline_directory = module_directory.join(module.ident.unraw().to_string());
            collect_items(
                snapshot,
                items,
                &inline_directory,
                &inline_directory,
                manifest_directory,
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
            let plain = module_directory.join(format!("{}.rs", module.ident.unraw()));
            let nested = module_directory
                .join(module.ident.unraw().to_string())
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
        collect(
            snapshot,
            &normalize(&path)?,
            manifest_directory,
            false,
            members,
        )?;
    }
    Ok(())
}

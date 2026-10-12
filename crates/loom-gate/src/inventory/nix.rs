//! Nix evaluation and check subjects from current native project definitions.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::snapshot::{ResourcePath, Snapshot};
use super::{Error, Name, Role, Subject, command, insert, single};

/// Exact native execution binding; subject names are diagnostic identities, not command syntax.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Target {
    flake: ResourcePath,
    system: Name,
    attribute: Name,
}

impl Target {
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
struct Outputs {
    #[serde(default)]
    checks: BTreeMap<String, BTreeMap<String, Check>>,
}

#[derive(Deserialize)]
struct Check {
    #[serde(rename = "type")]
    kind: Kind,
    #[serde(rename = "name")]
    _name: Name,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Derivation,
}

pub(super) fn discover(
    snapshot: &Snapshot,
    root: &Path,
    subjects: &mut BTreeMap<Name, Subject>,
) -> Result<(), Error> {
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
        let outputs: Outputs = serde_json::from_slice(&command(
            &project,
            super::Tool::Nix,
            &[
                "flake",
                "show",
                &reference,
                "--json",
                "--all-systems",
                "--no-write-lock-file",
                "--offline",
            ],
        )?)
        .map_err(|source| Error::Definition {
            definition: (*flake).clone(),
            source,
        })?;
        let mut checks = BTreeSet::new();
        for (system, entries) in outputs.checks {
            for (attribute, definition) in entries {
                let Check {
                    kind: Kind::Derivation,
                    _name: _,
                } = definition;
                let name = Name::try_from(format!(
                    "nix:check:{}:{system}:{attribute}",
                    flake.as_path().display()
                ))?;
                checks.insert(name.clone());
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
                            system: Name::try_from(system.clone())?,
                            attribute: Name::try_from(attribute)?,
                        }),
                    },
                )?;
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
    Ok(())
}

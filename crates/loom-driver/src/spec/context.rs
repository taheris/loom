//! Capture applicable documents once; rendering and compaction never re-read source files.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use displaydoc::Display;
use loom_protocol::context::{Context, Document, Package, Purpose};
use thiserror::Error;

use super::package;
use crate::identifier::SpecLabel;

/// Known labels narrow context; unknown relevance selects the full discovered roster.
#[derive(Clone, Copy)]
pub enum Selection<'a> {
    All,
    Labels(&'a [SpecLabel]),
}

#[derive(Debug, Display, Error)]
pub enum Error {
    /// package context discovery failed
    Package(#[from] package::Error),
    /// package context document could not be read: {path}
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// selected context owner {0} has no discovered package
    Missing(SpecLabel),
    /// captured context snapshot could not be written
    Write(#[source] std::io::Error),
}

/// Discover and capture current package documents without interpreting acceptance or evidence.
///
/// # Errors
/// Rejects incomplete inventories and unreadable selected documents, not empty new projects.
pub fn load(
    workspace: &Path,
    purpose: Purpose,
    selection: Selection<'_>,
) -> Result<Context, Error> {
    if purpose == Purpose::Planning {
        let inventory = match package::discover(&workspace.join("specs")) {
            Ok(packages) => packages,
            Err(package::Error::ReadDir { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Vec::new()
            }
            Err(error) => return Err(error.into()),
        };
        if inventory.is_empty() {
            let index = read(workspace, &workspace.join("docs/README.md"))?;
            let entries = package::parse_index_entries(&index.body)?;
            if let Some(entry) = entries.first() {
                return Err(Error::Missing(entry.label.clone()));
            }
            return capture(workspace, &inventory, purpose, selection);
        }
    }
    let packages = package::discover_workspace(workspace)?;
    capture(workspace, &packages, purpose, selection)
}

/// Capture a driver-selected roster, preserving exact source bytes and relative navigation.
///
/// # Errors
/// Rejects missing non-planning owners or failed reads rather than dropping obligations.
pub fn capture(
    workspace: &Path,
    packages: &[package::Package],
    purpose: Purpose,
    selection: Selection<'_>,
) -> Result<Context, Error> {
    let selected = match selection {
        Selection::All | Selection::Labels([]) => None,
        Selection::Labels(labels) => Some(labels.iter().cloned().collect::<HashSet<_>>()),
    };
    let mut new_anchors = Vec::new();
    if let Some(selected) = &selected {
        for label in selected {
            if !packages.iter().any(|package| package.label() == label) {
                if purpose == Purpose::Planning {
                    new_anchors.push(label.clone());
                } else {
                    return Err(Error::Missing(label.clone()));
                }
            }
        }
    }
    let packages = packages
        .iter()
        .filter(|package| {
            selected
                .as_ref()
                .is_none_or(|selected| selected.contains(package.label()))
        })
        .map(|package| {
            let contract = read(workspace, package.contract())?;
            let acceptance = if package.acceptance() == package.contract() {
                contract.clone()
            } else {
                read(workspace, package.acceptance())?
            };
            let support = if package.acceptance() == package.contract() {
                package.contract().with_extension("")
            } else {
                package
                    .contract()
                    .parent()
                    .unwrap_or_else(|| package.contract())
                    .to_path_buf()
            };
            Ok(Package {
                label: package.label().clone(),
                contract,
                acceptance,
                support_scope: relative(workspace, &support),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    new_anchors.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    Ok(Context::new(purpose, packages, new_anchors))
}

/// Persist captured documents in the owning private scratch session.
///
/// # Errors
/// Returns write failures rather than pointing an agent at incomplete snapshots.
pub fn write(context: &Context, scratch: &Path) -> Result<(), Error> {
    for package in context.packages() {
        let root = scratch.join("context").join(package.label.as_str());
        std::fs::create_dir_all(&root).map_err(Error::Write)?;
        std::fs::write(root.join("contract.md"), &package.contract.body).map_err(Error::Write)?;
        std::fs::write(root.join("acceptance.md"), &package.acceptance.body)
            .map_err(Error::Write)?;
    }
    Ok(())
}

fn read(workspace: &Path, path: &Path) -> Result<Document, Error> {
    let body = std::fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Document {
        path: relative(workspace, path),
        body,
    })
}

fn relative(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

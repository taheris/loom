//! Canonical packages and the mutually exclusive pre-cutover bootstrap tree.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use displaydoc::Display;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use thiserror::Error;

use crate::identifier::SpecLabel;

/// One complete owner with its contract and sole acceptance document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    label: SpecLabel,
    contract: PathBuf,
    acceptance: PathBuf,
}

impl Package {
    pub const fn label(&self) -> &SpecLabel {
        &self.label
    }

    pub fn contract(&self) -> &Path {
        &self.contract
    }

    pub fn acceptance(&self) -> &Path {
        &self.acceptance
    }

    /// Combined ownership budget; bootstrap documents count only once.
    ///
    /// # Errors
    /// Returns a contextual error if either document cannot be read.
    pub fn line_count(&self) -> Result<usize, Error> {
        let contract = read(&self.contract)?.lines().count();
        if self.contract == self.acceptance {
            Ok(contract)
        } else {
            Ok(contract + read(&self.acceptance)?.lines().count())
        }
    }
}

/// One index declaration with a workspace-relative contract path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub label: SpecLabel,
    pub contract: PathBuf,
}

/// Discovery fails rather than returning a partial owner inventory.
#[derive(Debug, Display, Error)]
pub enum Error {
    /// failed to read specs directory `{path}`
    ReadDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// failed to read spec input `{path}`
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// invalid spec label at `{path}`
    Label {
        path: PathBuf,
        #[source]
        source: loom_events::identifier::ParseSpecLabelError,
    },
    /// incomplete package: required document `{path}` is missing or not a regular file
    Incomplete { path: PathBuf },
    /// specs tree `{path}` mixes canonical packages with flat bootstrap documents
    MixedLayout { path: PathBuf },
    /// spec index disagreement: {detail}
    Index { detail: String },
}

/// Discover complete owners in lexical label order without consulting an index.
///
/// Flat documents are accepted only for an entirely flat bootstrap tree. Supporting files
/// inside packages never become additional acceptance documents.
///
/// # Errors
/// Rejects incomplete packages, invalid labels, mixed layouts, and unreadable directories.
pub fn discover(specs_dir: &Path) -> Result<Vec<Package>, Error> {
    let read_dir_error = |source| Error::ReadDir {
        path: specs_dir.to_path_buf(),
        source,
    };
    let mut paths = fs::read_dir(specs_dir)
        .map_err(read_dir_error)?
        .map(|entry| entry.map(|entry| entry.path()).map_err(read_dir_error))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    let mut packages = Vec::new();
    let mut has_flat = false;
    let mut has_package = false;
    for path in paths {
        let metadata = fs::symlink_metadata(&path).map_err(read_dir_error)?;
        let canonical = metadata.is_dir();
        if !canonical && path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let label = if canonical {
            path.file_name()
        } else {
            path.file_stem()
        };
        let label =
            SpecLabel::new(label.unwrap_or_default().to_string_lossy()).map_err(|source| {
                Error::Label {
                    path: path.clone(),
                    source,
                }
            })?;
        let (contract, acceptance) = if canonical {
            has_package = true;
            for document in ["spec.md", "tests.md"] {
                let file = path.join(document);
                match fs::symlink_metadata(&file) {
                    Ok(metadata) if metadata.is_file() => {}
                    Ok(_) => return Err(Error::Incomplete { path: file }),
                    Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                        return Err(Error::Incomplete { path: file });
                    }
                    Err(source) => return Err(Error::ReadFile { path: file, source }),
                }
            }
            (path.join("spec.md"), path.join("tests.md"))
        } else {
            if !metadata.is_file() {
                return Err(Error::Incomplete { path });
            }
            has_flat = true;
            (path.clone(), path)
        };
        packages.push(Package {
            label,
            contract,
            acceptance,
        });
    }
    if has_flat && has_package {
        return Err(Error::MixedLayout {
            path: specs_dir.to_path_buf(),
        });
    }
    packages.sort_by(|left, right| left.label.as_str().cmp(right.label.as_str()));
    Ok(packages)
}

/// Parse ordinary index links; code examples do not create index declarations.
///
/// # Errors
/// Rejects duplicate labels, malformed labels, and label/path disagreement.
pub fn parse_index(content: &str) -> Result<Vec<IndexEntry>, Error> {
    let mut entries = Vec::new();
    let mut labels = HashSet::new();
    let mut link: Option<(String, String)> = None;
    let events =
        crate::markdown::section_events(content, pulldown_cmark::HeadingLevel::H2, |heading| {
            heading == "Specs"
        })
        .map_or_else(
            || Parser::new_ext(content, Options::ENABLE_TABLES).collect::<Vec<_>>(),
            |section| section.into_iter().map(|(event, _)| event).collect(),
        );
    for event in events {
        match event {
            Event::Start(Tag::Link { dest_url, .. }) if dest_url.starts_with("../specs/") => {
                link = Some((dest_url.to_string(), String::new()));
            }
            Event::Text(text) | Event::Code(text) if link.is_some() => {
                if let Some((_, label)) = &mut link {
                    label.push_str(&text);
                }
            }
            Event::End(TagEnd::Link) => {
                let Some((destination, label)) = link.take() else {
                    continue;
                };
                let label =
                    SpecLabel::new(label.to_lowercase()).map_err(|source| Error::Label {
                        path: PathBuf::from(&destination),
                        source,
                    })?;
                let contract = PathBuf::from(destination.trim_start_matches("../"));
                let canonical = PathBuf::from(format!("specs/{label}/spec.md"));
                let bootstrap = PathBuf::from(format!("specs/{label}.md"));
                if contract != canonical && contract != bootstrap {
                    return Err(Error::Index {
                        detail: format!(
                            "label `{label}` disagrees with index path `{}`",
                            contract.display()
                        ),
                    });
                }
                if !labels.insert(label.clone()) {
                    return Err(Error::Index {
                        detail: format!("duplicate index row for spec `{label}`"),
                    });
                }
                entries.push(IndexEntry { label, contract });
            }
            _ => {}
        }
    }
    if entries.is_empty() {
        return Err(Error::Index {
            detail: "no specs indexed in docs/README.md".into(),
        });
    }
    Ok(entries)
}

/// Discover the workspace inventory and require exactly one matching index row per owner.
///
/// # Errors
/// Rejects unindexed owners, missing indexed owners, duplicates, and path mismatches.
pub fn discover_indexed(workspace: &Path) -> Result<Vec<Package>, Error> {
    let packages = discover(&workspace.join("specs"))?;
    cross_check_index(workspace, &packages)?;
    Ok(packages)
}

fn cross_check_index(workspace: &Path, packages: &[Package]) -> Result<(), Error> {
    let entries = parse_index(&read(&workspace.join("docs/README.md"))?)?;
    for entry in &entries {
        let expected = workspace.join(&entry.contract);
        if !packages
            .iter()
            .any(|package| package.label == entry.label && package.contract == expected)
        {
            return Err(Error::Index {
                detail: format!(
                    "indexed spec `{}` is missing or disagrees with `{}`",
                    entry.label,
                    entry.contract.display()
                ),
            });
        }
    }
    for package in packages {
        if !entries.iter().any(|entry| entry.label == package.label) {
            return Err(Error::Index {
                detail: format!(
                    "spec file `{}` is not listed in docs/README.md",
                    package.contract.display()
                ),
            });
        }
    }
    Ok(())
}

/// Discover live workspace owners; canonical trees always require the spec index.
///
/// # Errors
/// Rejects canonical trees without an index and any disagreement with an existing index.
pub fn discover_workspace(workspace: &Path) -> Result<Vec<Package>, Error> {
    let packages = discover(&workspace.join("specs"))?;
    let index = workspace.join("docs/README.md");
    let has_index = index.try_exists().map_err(|source| Error::ReadFile {
        path: index,
        source,
    })?;
    if has_index
        || packages
            .iter()
            .any(|package| package.contract != package.acceptance)
    {
        cross_check_index(workspace, &packages)?;
    }
    Ok(packages)
}

/// Owner name for an annotation document, independent of package relocation.
pub fn document_label(path: &Path) -> Option<&str> {
    match path.file_name()?.to_str()? {
        "spec.md" | "tests.md" if path.parent()?.parent()?.file_name()?.to_str()? == "specs" => {
            path.parent()?.file_name()?.to_str()
        }
        _ => path.file_stem()?.to_str(),
    }
}

fn read(path: &Path) -> Result<String, Error> {
    fs::read_to_string(path).map_err(|source| Error::ReadFile {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(root: &Path, label: &str) {
        let dir = root.join("specs").join(label);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("spec.md"), "# Contract\n").unwrap();
        fs::write(
            dir.join("tests.md"),
            "## Success Criteria\n- Acceptance [test](real_test)\n",
        )
        .unwrap();
    }

    #[test]
    fn quint_package_discovery_is_canonical() {
        let dir = tempfile::tempdir().unwrap();
        package(dir.path(), "alpha");
        let packages = discover(&dir.path().join("specs")).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].label().as_str(), "alpha");
        assert_eq!(
            packages[0].acceptance(),
            dir.path().join("specs/alpha/tests.md")
        );
        fs::write(dir.path().join("specs/alpha.md"), "flat authority").unwrap();
        assert!(matches!(
            discover(&dir.path().join("specs")),
            Err(Error::MixedLayout { .. })
        ));
        fs::remove_file(dir.path().join("specs/alpha.md")).unwrap();
        for document in ["spec.md", "tests.md"] {
            let file = dir.path().join("specs/alpha").join(document);
            fs::remove_file(&file).unwrap();
            assert!(matches!(
                discover(&dir.path().join("specs")),
                Err(Error::Incomplete { path }) if path == file
            ));
            package(dir.path(), "alpha");
        }
    }

    #[test]
    fn non_file_acceptance_documents_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        package(dir.path(), "alpha");
        fs::remove_file(dir.path().join("specs/alpha/tests.md")).unwrap();
        fs::create_dir(dir.path().join("specs/alpha/tests.md")).unwrap();
        assert!(matches!(
            discover(&dir.path().join("specs")),
            Err(Error::Incomplete { path }) if path.ends_with("alpha/tests.md")
        ));
    }

    #[test]
    fn indexed_discovery_rejects_unindexed_and_missing_packages() {
        let dir = tempfile::tempdir().unwrap();
        package(dir.path(), "alpha");
        fs::create_dir(dir.path().join("docs")).unwrap();
        let index = dir.path().join("docs/README.md");
        fs::write(
            &index,
            "| Spec | Purpose |\n| --- | --- |\n| [alpha](../specs/alpha/spec.md) | A |\n",
        )
        .unwrap();
        assert_eq!(discover_indexed(dir.path()).unwrap().len(), 1);
        package(dir.path(), "beta");
        assert!(matches!(
            discover_indexed(dir.path()),
            Err(Error::Index { .. })
        ));
        fs::write(index, "- [alpha](../specs/alpha/spec.md)\n- [beta](../specs/beta/spec.md)\n- [gamma](../specs/gamma/spec.md)").unwrap();
        assert!(matches!(
            discover_indexed(dir.path()),
            Err(Error::Index { .. })
        ));
    }

    #[test]
    fn index_rejects_duplicates_and_path_disagreement() {
        for content in [
            "- [alpha](../specs/alpha/spec.md)\n- [alpha](../specs/alpha/spec.md)",
            "- [alpha](../specs/beta/spec.md)",
            "- [alpha](../specs/alpha/tests.md)",
            "- [alpha](../specs/alpha/../alpha/spec.md)",
        ] {
            assert!(
                matches!(parse_index(content), Err(Error::Index { .. })),
                "{content}"
            );
        }
    }

    #[test]
    fn index_ignores_fenced_and_inline_examples() {
        let entries = parse_index("```md\n[alpha](../specs/wrong.md)\n```\n`[alpha](../specs/wrong.md)`\n- [alpha](../specs/alpha/spec.md)").unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn document_owners_survive_relocation() {
        for path in [
            "specs/alpha.md",
            "specs/alpha/spec.md",
            "specs/alpha/tests.md",
        ] {
            assert_eq!(document_label(Path::new(path)), Some("alpha"));
        }
    }
}

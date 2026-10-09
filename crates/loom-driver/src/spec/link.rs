//! Ordinary Markdown contract-to-acceptance navigation integrity.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use displaydoc::Display;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use thiserror::Error;

use super::package::Package;

/// A broken navigation destination is not a verifier binding.
#[derive(Debug, Display, Error)]
pub enum Error {
    /// failed to read contract-link document `{path}`
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// contract `{path}` links to missing acceptance destination `{destination}`
    Destination { path: PathBuf, destination: String },
    /// contract `{path}` links to missing acceptance heading `{destination}`
    Fragment { path: PathBuf, destination: String },
}

/// Check ordinary links into the owner's acceptance document and its named headings.
///
/// Verifier links, code examples, external URLs, and unrelated navigation are not mappings.
///
/// # Errors
/// Fails on missing destinations or fragments, including reference-style Markdown links.
pub fn check(package: &Package) -> Result<(), Error> {
    let source = package.contract();
    let body = read(source)?;
    let canonical = source != package.acceptance();
    let headings = headings(&read(package.acceptance())?);
    for event in Parser::new_ext(&body, Options::ENABLE_TABLES) {
        let Event::Start(Tag::Link { dest_url, .. }) = event else {
            continue;
        };
        if dest_url.contains("://") || dest_url.starts_with("mailto:") {
            continue;
        }
        let (document, fragment) = dest_url.split_once('#').map_or_else(
            || (dest_url.as_ref(), None),
            |(document, fragment)| (document, Some(fragment)),
        );
        let destination = source
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(document);
        let is_mapping = if canonical {
            Path::new(document)
                .file_name()
                .is_some_and(|name| name == "tests.md")
        } else {
            document.is_empty() || destination == source
        };
        if !is_mapping {
            continue;
        }
        let destination = if document.is_empty() {
            source.to_path_buf()
        } else {
            destination
        };
        if !destination.is_file() {
            return Err(Error::Destination {
                path: source.to_path_buf(),
                destination: dest_url.to_string(),
            });
        }
        let destination_headings = if destination == package.acceptance() {
            headings.clone()
        } else {
            headings_for(&destination)?
        };
        if let Some(fragment) = fragment
            && !destination_headings.contains(fragment)
        {
            return Err(Error::Fragment {
                path: source.to_path_buf(),
                destination: dest_url.to_string(),
            });
        }
    }
    Ok(())
}

fn read(path: &Path) -> Result<String, Error> {
    std::fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })
}

fn headings_for(path: &Path) -> Result<BTreeSet<String>, Error> {
    Ok(headings(&read(path)?))
}

fn headings(content: &str) -> BTreeSet<String> {
    let mut slugs = BTreeSet::new();
    let mut duplicates = BTreeMap::<String, usize>::new();
    let mut heading = None;
    for event in Parser::new(content) {
        match event {
            Event::Start(Tag::Heading { .. }) => heading = Some(String::new()),
            Event::Text(text) | Event::Code(text) => {
                if let Some(heading) = &mut heading {
                    heading.push_str(&text);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(text) = heading.take() {
                    let slug: String = text
                        .to_lowercase()
                        .chars()
                        .filter_map(|character| {
                            if character.is_whitespace() {
                                Some('-')
                            } else if character.is_alphanumeric()
                                || character == '-'
                                || character == '_'
                            {
                                Some(character)
                            } else {
                                None
                            }
                        })
                        .collect();
                    let mut candidate = slug.clone();
                    let count = duplicates.entry(slug.clone()).or_default();
                    while slugs.contains(&candidate) {
                        *count += 1;
                        candidate = format!("{slug}-{count}");
                    }
                    slugs.insert(candidate);
                }
            }
            _ => {}
        }
    }
    slugs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::package::discover;

    #[test]
    fn package_contract_links_resolve_to_acceptance_sections() {
        let dir = tempfile::tempdir().unwrap();
        let package_dir = dir.path().join("alpha");
        std::fs::create_dir(&package_dir).unwrap();
        std::fs::write(
            package_dir.join("tests.md"),
            "# Acceptance\n## Fresh `execution`!\n## Fresh `execution`!\n",
        )
        .unwrap();
        let source = package_dir.join("spec.md");
        std::fs::write(&source, "[Fresh](tests.md#fresh-execution)\n[again][mapping]\n\n[mapping]: tests.md#fresh-execution-1\n\n```md\n[example](tests.md#absent)\n```\n`[example](tests.md#absent)`\n").unwrap();
        let package = discover(dir.path()).unwrap().remove(0);
        check(&package).unwrap();
        std::fs::write(&source, "[Broken](tests.md#absent)").unwrap();
        assert!(matches!(check(&package), Err(Error::Fragment { .. })));
        std::fs::write(&source, "[Broken](missing/tests.md#fresh-execution)").unwrap();
        assert!(matches!(check(&package), Err(Error::Destination { .. })));
    }
}

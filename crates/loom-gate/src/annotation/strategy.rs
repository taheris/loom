//! Parsed verification policy; inventory resolution and admission are separate boundaries.

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::{Path, PathBuf};

use displaydoc::Display;
use nextest_filtering::{ParsedExpr, errors::ParseSingleError};
use serde::Deserialize;
use thiserror::Error;

use super::{LineIndex, StructuralPass, TokenHit, text_without_ranges};

/// Source of one exact `loom-verify` fence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub document: PathBuf,
    pub line: u32,
    pub bytes: Range<usize>,
}

/// A syntactically parsed declaration, not a resolved obligation or subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Declaration {
    Package {
        source: Source,
        policy: Package,
    },
    Criterion {
        source: Source,
        line: u32,
        defer_until: Deferral,
    },
}

/// Optional additions to the built-in package policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub rust: Option<Rust>,
    pub role_exceptions: Vec<RoleException>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rust {
    pub property_filters: Vec<PropertyFilter>,
}

/// A nextest expression whose grammar is valid but whose membership is unresolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyFilter {
    expression: String,
    parsed: ParsedExpr,
}

impl PropertyFilter {
    pub fn expression(&self) -> &str {
        &self.expression
    }
    pub const fn parsed(&self) -> &ParsedExpr {
        &self.parsed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleException {
    pub subject: SubjectRef,
    pub role: RoleRef,
    pub reason: String,
}

/// An open project-inventory name; no naming scheme or membership is inferred here.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SubjectRef(String);

impl SubjectRef {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An open classification reference, to be resolved by the inventory's role policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleRef(String);

impl RoleRef {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The only supported criterion-local stage exception.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Deferral {
    Publication,
}

#[derive(Debug, Display, Error)]
pub enum Error {
    /// invalid verification-strategy TOML
    Toml(#[from] toml::de::Error),
    /// invalid nextest property filter `{expression}`: {errors:?}
    Filter {
        expression: String,
        errors: Vec<ParseSingleError>,
    },
    /// `{field}` must be nonblank; subject and role references cannot contain control characters
    EmptyReference { field: &'static str },
    /// a strategy fence must declare package policy or a criterion deferral
    Empty,
    /// criterion deferral must be contained in its owning acceptance bullet directly after its sole annotation
    Attachment,
    /// package policy and criterion-local deferral must be in separate fences
    Mixed,
    /// conflicting or repeated `{field}` declaration
    Conflict { field: &'static str },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    rust: Option<RawRust>,
    role_exception: Option<Vec<RawRole>>,
    defer_until: Option<Deferral>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRust {
    property_filters: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRole {
    subject: String,
    role: String,
    reason: String,
}

pub(super) struct Fence {
    pub range: Range<usize>,
    pub body: String,
}

pub(super) fn parse(
    path: &Path,
    content: &str,
    structure: &StructuralPass,
    bindings: &[TokenHit],
) -> Result<Vec<Declaration>, super::ParseError> {
    let index = LineIndex::from(content);
    let mut declarations = Vec::new();
    let mut rust_declared = false;
    let mut subjects = BTreeSet::new();
    let mut criteria = BTreeSet::new();
    for fence in &structure.strategy_fences {
        let source = Source {
            document: path.to_path_buf(),
            line: index.line_of(fence.range.start),
            bytes: fence.range.clone(),
        };
        let declaration = parse_fence(content, structure, bindings, fence, source.clone())
            .and_then(|declaration| {
                match &declaration {
                    Declaration::Package { policy, .. } => {
                        if policy.rust.is_some() && std::mem::replace(&mut rust_declared, true) {
                            return Err(Error::Conflict { field: "rust" });
                        }
                        for exception in &policy.role_exceptions {
                            if !subjects.insert(exception.subject.clone()) {
                                return Err(Error::Conflict {
                                    field: "role_exception.subject",
                                });
                            }
                        }
                    }
                    Declaration::Criterion { line, .. } => {
                        if !criteria.insert(*line) {
                            return Err(Error::Conflict {
                                field: "defer_until",
                            });
                        }
                    }
                }
                Ok(declaration)
            })
            .map_err(|error| super::ParseError::Strategy {
                path: source.document,
                line: source.line,
                source: error,
            })?;
        declarations.push(declaration);
    }
    Ok(declarations)
}

fn parse_fence(
    content: &str,
    structure: &StructuralPass,
    bindings: &[TokenHit],
    fence: &Fence,
    source: Source,
) -> Result<Declaration, Error> {
    let raw: Raw = toml::from_str(&fence.body)?;
    let has_package_policy = raw.rust.is_some() || raw.role_exception.is_some();
    let owner = structure.innermost_item_containing(fence.range.start);
    if let Some(defer_until) = raw.defer_until {
        if has_package_policy {
            return Err(Error::Mixed);
        }
        let line = owner
            .filter(|line| structure.criterion_bullet_lines.contains(line))
            .ok_or(Error::Attachment)?;
        let item = structure
            .item_ranges
            .iter()
            .find(|(_, start)| *start == line)
            .ok_or(Error::Attachment)?;
        if fence.range.end > item.0.end {
            return Err(Error::Attachment);
        }
        let attached = bindings
            .iter()
            .filter(|binding| structure.innermost_item_containing(binding.start) == Some(line))
            .collect::<Vec<_>>();
        let [binding] = attached.as_slice() else {
            return Err(Error::Attachment);
        };
        if binding.end > fence.range.start
            || binding.end > item.0.end
            || !text_without_ranges(
                content,
                &(binding.end..fence.range.start),
                &structure.comment_ranges,
            )
            .is_empty()
        {
            return Err(Error::Attachment);
        }
        return Ok(Declaration::Criterion {
            source,
            line,
            defer_until,
        });
    }
    if !has_package_policy {
        return Err(Error::Empty);
    }
    if owner.is_some() {
        return Err(Error::Attachment);
    }
    let rust = raw
        .rust
        .map(|rust| {
            let mut seen = BTreeSet::new();
            let property_filters = rust
                .property_filters
                .into_iter()
                .map(|expression| {
                    if !seen.insert(expression.clone()) {
                        return Err(Error::Conflict {
                            field: "rust.property_filters",
                        });
                    }
                    let parsed =
                        ParsedExpr::parse(&expression).map_err(|errors| Error::Filter {
                            expression: expression.clone(),
                            errors,
                        })?;
                    Ok(PropertyFilter { expression, parsed })
                })
                .collect::<Result<Vec<_>, Error>>()?;
            Ok::<_, Error>(Rust { property_filters })
        })
        .transpose()?;
    let role_exceptions = raw
        .role_exception
        .into_iter()
        .flatten()
        .map(|raw| {
            Ok(RoleException {
                subject: SubjectRef(nonempty(raw.subject, "role_exception.subject")?),
                role: RoleRef(nonempty(raw.role, "role_exception.role")?),
                reason: if raw.reason.trim().is_empty() {
                    return Err(Error::EmptyReference {
                        field: "role_exception.reason",
                    });
                } else {
                    raw.reason
                },
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(Declaration::Package {
        source,
        policy: Package {
            rust,
            role_exceptions,
        },
    })
}

fn nonempty(value: String, field: &'static str) -> Result<String, Error> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        Err(Error::EmptyReference { field })
    } else {
        Ok(value)
    }
}

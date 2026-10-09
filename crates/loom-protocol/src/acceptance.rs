//! Parsed task references and immutable snapshot-resolved acceptance.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use displaydoc::Display;
use loom_events::identifier::{ParseSpecLabelError, SpecLabel};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::criterion::{CriterionAnnotation, CriterionId, ParseCriterionIdError};

/// A syntactically valid requirement reference, not resolved acceptance.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    spec_label: SpecLabel,
    criterion_id: CriterionId,
}

impl Reference {
    pub const fn new(spec_label: SpecLabel, criterion_id: CriterionId) -> Self {
        Self {
            spec_label,
            criterion_id,
        }
    }

    /// Parse both external identifiers without consulting a package snapshot.
    ///
    /// # Errors
    /// Rejects malformed labels and criterion identifiers.
    pub fn parse(spec_label: &str, criterion_id: &str) -> Result<Self, ParseError> {
        Ok(Self::new(
            SpecLabel::new(spec_label)?,
            CriterionId::new(criterion_id)?,
        ))
    }

    pub const fn spec_label(&self) -> &SpecLabel {
        &self.spec_label
    }

    pub const fn criterion_id(&self) -> &CriterionId {
        &self.criterion_id
    }
}

impl std::fmt::Display for Reference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} / {}", self.spec_label, self.criterion_id)
    }
}

/// Nonempty, duplicate-free candidates; resolution is a separate fallible transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<Reference>", into = "Vec<Reference>")]
pub struct References(Vec<Reference>);

impl References {
    /// Require at least one distinct typed reference.
    ///
    /// # Errors
    /// Rejects empty assignments and duplicate members.
    pub fn new(references: Vec<Reference>) -> Result<Self, ParseError> {
        if references.is_empty() {
            return Err(ParseError::Empty);
        }
        let mut seen = HashSet::new();
        for reference in &references {
            if !seen.insert(reference) {
                return Err(ParseError::Duplicate {
                    reference: reference.clone(),
                });
            }
        }
        Ok(Self(references))
    }

    /// Parse the entire external assignment, without dropping malformed members.
    ///
    /// # Errors
    /// Rejects malformed identifiers, empty assignments, and duplicates.
    pub fn parse<'a>(
        references: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Result<Self, ParseError> {
        let references = references
            .into_iter()
            .enumerate()
            .map(|(index, (label, id))| {
                Reference::parse(label, id).map_err(|source| ParseError::Member {
                    index,
                    source: Box::new(source),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(references)
    }

    pub fn as_slice(&self) -> &[Reference] {
        &self.0
    }
}

impl TryFrom<Vec<Reference>> for References {
    type Error = ParseError;

    fn try_from(value: Vec<Reference>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<References> for Vec<Reference> {
    fn from(value: References) -> Self {
        value.0
    }
}

/// Task-reference syntax and assignment-shape failures.
#[derive(Debug, Display, Error)]
pub enum ParseError {
    /// invalid task spec label
    Label(#[from] ParseSpecLabelError),
    /// invalid task criterion identifier
    Criterion(#[from] ParseCriterionIdError),
    /// invalid task reference at index {index}
    Member {
        index: usize,
        #[source]
        source: Box<ParseError>,
    },
    /// ordinary task acceptance must contain at least one reference
    Empty,
    /// duplicate task reference `{reference}`
    Duplicate { reference: Reference },
}

/// Unresolved structural parser output for one package, including packages with no criteria.
#[derive(Debug)]
pub struct PackageInput {
    pub spec_label: SpecLabel,
    pub criteria: Vec<CriterionInput>,
}

/// Unresolved requirement and all of its parsed annotations; none may be silently discarded.
#[derive(Debug)]
pub struct CriterionInput {
    pub criterion_text: String,
    pub annotations: Vec<CriterionAnnotation>,
    pub source: PathBuf,
    pub line: u32,
}

/// Immutable acceptance inventory selected by the driver for one dispatch snapshot.
#[derive(Debug)]
pub struct Snapshot {
    packages: HashMap<SpecLabel, HashMap<CriterionId, Obligation>>,
}

impl Snapshot {
    /// Construct a complete inventory from structurally parsed package acceptance.
    ///
    /// # Errors
    /// Rejects duplicate labels or requirements, empty requirements, and non-atomic bindings.
    pub fn new(packages: impl IntoIterator<Item = PackageInput>) -> Result<Self, SnapshotError> {
        let mut inventory = HashMap::new();
        for package in packages {
            if inventory.contains_key(&package.spec_label) {
                return Err(SnapshotError::DuplicatePackage {
                    spec_label: package.spec_label,
                });
            }
            let mut criteria = HashMap::new();
            for criterion in package.criteria {
                let text = criterion
                    .criterion_text
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if text.is_empty() {
                    return Err(SnapshotError::EmptyRequirement {
                        source_document: criterion.source,
                        line: criterion.line,
                    });
                }
                let id = CriterionId::for_spec_text(&package.spec_label, &text);
                let reference = Reference::new(package.spec_label.clone(), id.clone());
                if criteria.contains_key(&id) {
                    return Err(SnapshotError::DuplicateCriterion { reference });
                }
                let count = criterion.annotations.len();
                let mut annotations = criterion.annotations.into_iter();
                let Some(annotation) = annotations.next().filter(|_| count == 1) else {
                    return Err(SnapshotError::BindingCount { reference, count });
                };
                if annotation.target.as_str().trim().is_empty() {
                    return Err(SnapshotError::EmptyTarget { reference });
                }
                criteria.insert(
                    id,
                    Obligation {
                        reference,
                        criterion_text: text,
                        annotation,
                        source: criterion.source,
                        line: criterion.line,
                    },
                );
            }
            inventory.insert(package.spec_label, criteria);
        }
        Ok(Self {
            packages: inventory,
        })
    }

    /// Resolve every candidate using only this snapshot's text and verifier metadata.
    ///
    /// # Errors
    /// Rejects unknown packages and missing requirements, without returning partial acceptance.
    pub fn resolve(&self, references: &References) -> Result<Resolved, ResolveError> {
        let obligations = references
            .as_slice()
            .iter()
            .map(|reference| self.resolve_one(reference))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Resolved { obligations })
    }

    /// Describe an explicitly selected replacement for Evidence's history reconciliation.
    ///
    /// This does not install an alias, transfer results, or mutate tasks or retained history.
    ///
    /// # Errors
    /// Rejects a replacement that does not resolve in this snapshot.
    pub fn rebind(
        &self,
        previous: &Obligation,
        current: &Reference,
    ) -> Result<Rebinding, ResolveError> {
        Ok(Rebinding {
            previous: previous.clone(),
            current: self.resolve_one(current)?,
        })
    }

    fn resolve_one(&self, reference: &Reference) -> Result<Obligation, ResolveError> {
        let package = self.packages.get(reference.spec_label()).ok_or_else(|| {
            ResolveError::UnknownPackage {
                spec_label: reference.spec_label().clone(),
            }
        })?;
        package
            .get(reference.criterion_id())
            .cloned()
            .ok_or_else(|| ResolveError::MissingCriterion {
                reference: reference.clone(),
            })
    }
}

/// Broken snapshot structure blocks construction rather than hiding acceptance.
#[derive(Debug, Display, Error)]
pub enum SnapshotError {
    /// ambiguous snapshot: duplicate package `{spec_label}`
    DuplicatePackage { spec_label: SpecLabel },
    /// ambiguous snapshot: duplicate normalized requirement `{reference}`
    DuplicateCriterion { reference: Reference },
    /// empty requirement in `{source_document}` at line {line}
    EmptyRequirement { source_document: PathBuf, line: u32 },
    /// requirement `{reference}` carries {count} annotations, expected one
    BindingCount { reference: Reference, count: usize },
    /// requirement `{reference}` has an empty verifier target
    EmptyTarget { reference: Reference },
}

/// Syntactically valid references can still fail current-snapshot resolution.
#[derive(Debug, Display, Error)]
pub enum ResolveError {
    /// unknown spec package `{spec_label}`; explicitly bind the task to a current package
    UnknownPackage { spec_label: SpecLabel },
    /// requirement `{reference}` is absent; explicitly rebind the task to a current criterion
    MissingCriterion { reference: Reference },
}

/// Complete immutable task acceptance; neither external JSON nor a flag can construct it.
///
/// ```compile_fail
/// let _: loom_protocol::acceptance::Resolved = serde_json::from_str("{}").unwrap();
/// ```
/// ```compile_fail
/// let _ = loom_protocol::acceptance::Resolved { obligations: vec![] };
/// ```
/// ```compile_fail
/// use loom_protocol::acceptance::Resolved;
/// fn dispatch(_: Resolved) {}
/// dispatch(loom_protocol::acceptance::References::parse([("specs", "criterion-0123456789abcdef")]).unwrap());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    obligations: Vec<Obligation>,
}

impl Resolved {
    pub fn obligations(&self) -> &[Obligation] {
        &self.obligations
    }
}

/// One snapshot-resolved requirement and its current parsed binding, not passing evidence.
///
/// ```compile_fail
/// use loom_protocol::acceptance::Obligation;
/// fn rewrite(obligation: &mut Obligation) { obligation.criterion_text = "different".into(); }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Obligation {
    reference: Reference,
    criterion_text: String,
    annotation: CriterionAnnotation,
    source: PathBuf,
    line: u32,
}

impl Obligation {
    pub const fn reference(&self) -> &Reference {
        &self.reference
    }

    pub fn criterion_text(&self) -> &str {
        &self.criterion_text
    }

    pub const fn annotation(&self) -> &CriterionAnnotation {
        &self.annotation
    }

    pub fn source(&self) -> &Path {
        &self.source
    }

    pub const fn line(&self) -> u32 {
        self.line
    }
}

/// Explicit old/current claim endpoints; Evidence owns negative-history linkage and re-admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rebinding {
    previous: Obligation,
    current: Obligation,
}

impl Rebinding {
    pub const fn previous(&self) -> &Obligation {
        &self.previous
    }

    pub const fn current(&self) -> &Obligation {
        &self.current
    }
}

//! Load current package acceptance independently of verifier execution and cached observations.

use std::path::Path;

use displaydoc::Display;
use loom_driver::spec::package;
use loom_protocol::acceptance::{CriterionInput, PackageInput, Snapshot, SnapshotError};
use loom_protocol::criterion::{AnnotationTarget, AnnotationTier, CriterionAnnotation};
use thiserror::Error;

use crate::annotation::{self, Annotation, Tier};

/// Load one immutable, indexed package snapshot for subsequent task-reference resolution.
///
/// # Errors
/// Rejects discovery/navigation/read failures and broken or ambiguous criterion bindings.
pub fn load(workspace: &Path) -> Result<Snapshot, Error> {
    let packages = package::discover_indexed(workspace)?;
    let parsed = annotation::parse_packages(&packages)?;
    let inputs = packages.iter().map(|package| PackageInput {
        spec_label: package.label().clone(),
        criteria: parsed
            .criteria
            .iter()
            .filter(|criterion| criterion.source_spec == package.acceptance())
            .map(|criterion| CriterionInput {
                criterion_text: criterion.text.clone(),
                annotations: parsed
                    .annotations
                    .iter()
                    .filter(|annotation| {
                        (annotation.source_spec.as_path(), annotation.criterion_line)
                            == (criterion.source_spec.as_path(), criterion.line)
                    })
                    .map(binding)
                    .collect(),
                source: criterion.source_spec.clone(),
                line: criterion.line,
            })
            .collect(),
    });
    Ok(Snapshot::new(inputs)?)
}

fn binding(annotation: &Annotation) -> CriterionAnnotation {
    CriterionAnnotation {
        tier: match annotation.tier {
            Tier::Check => AnnotationTier::Check,
            Tier::Test => AnnotationTier::Test,
            Tier::System => AnnotationTier::System,
            Tier::Judge => AnnotationTier::Judge,
        },
        target: AnnotationTarget::new(annotation.target.clone()),
        pending: annotation.pending,
    }
}

/// Current-snapshot loading fails rather than producing incomplete acceptance.
#[derive(Debug, Display, Error)]
pub enum Error {
    /// failed to discover indexed task-acceptance packages
    Package(#[from] package::Error),
    /// failed to parse task-acceptance documents
    Parse(#[from] annotation::ParseError),
    /// invalid task-acceptance snapshot
    Snapshot(#[from] SnapshotError),
}

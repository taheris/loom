//! Criterion-local early-stage requirements and deferrals.

use std::collections::{BTreeMap, BTreeSet};

use crate::annotation::{Annotation, Tier, strategy};
use crate::inventory::snapshot::{ResourcePath, Snapshot};

use super::super::Error;

/// Applicability derived from the captured contracts, not caller-supplied targets.
pub struct FeedbackPolicy {
    early: BTreeMap<ResourcePath, BTreeSet<u32>>,
    deferred: BTreeMap<ResourcePath, BTreeSet<u32>>,
}

impl FeedbackPolicy {
    /// Parse stage policy from the current capture.
    ///
    /// # Errors
    /// Invalid acceptance documents or stage declarations block planning.
    pub fn capture(snapshot: &Snapshot) -> Result<Self, Error> {
        let mut policy = Self {
            early: BTreeMap::new(),
            deferred: BTreeMap::new(),
        };
        for path in
            crate::inventory::contracts(snapshot).map_err(|e| Error::Subject(Box::new(e)))?
        {
            let content = snapshot
                .text(&path)
                .map_err(|e| Error::Snapshot(Box::new(e)))?;
            let parsed = crate::annotation::parse_content(path.as_path(), content)
                .map_err(|e| Error::Strategy(Box::new(e)))?;
            for declaration in parsed.strategy {
                if let strategy::Declaration::Criterion {
                    line,
                    applicability,
                    ..
                } = declaration
                {
                    let owners = match applicability {
                        strategy::Applicability::RequireFromFeedback => &mut policy.early,
                        strategy::Applicability::DeferUntilPublication => &mut policy.deferred,
                    };
                    owners.entry(path.clone()).or_default().insert(line);
                }
            }
        }
        Ok(policy)
    }

    /// Ordinary Rust test names are bound to publication results. Owner-local
    /// executions supply the baseline feedback, not an alias of that result.
    ///
    /// # Errors
    /// The annotation source must name a valid resource in the capture.
    pub fn applies(&self, root: &std::path::Path, annotation: &Annotation) -> Result<bool, Error> {
        let path = annotation
            .source_spec
            .strip_prefix(root)
            .unwrap_or(&annotation.source_spec);
        let path =
            ResourcePath::try_from(path.to_path_buf()).map_err(|e| Error::Snapshot(Box::new(e)))?;
        if self
            .deferred
            .get(&path)
            .is_some_and(|lines| lines.contains(&annotation.criterion_line))
        {
            return Ok(false);
        }
        let native_case = annotation.tier == Tier::Test
            && !annotation.target.starts_with("nix build ")
            && !annotation.target.starts_with("nix report ");
        Ok(!native_case
            || self
                .early
                .get(&path)
                .is_some_and(|lines| lines.contains(&annotation.criterion_line)))
    }
}

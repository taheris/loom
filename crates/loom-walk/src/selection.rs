//! Closed walk selection for hermetic source-analysis producers, not whole-Gate admission.

use std::collections::BTreeSet;
use std::path::PathBuf;

use displaydoc::Display;
use thiserror::Error;

use loom_gate::{Annotation, Tier};

/// Resolve only an exact registered name or the historical Cargo spelling.
pub fn walk_name(target: &str) -> Option<&str> {
    if crate::walk::lookup(target).is_some() {
        return Some(target);
    }
    let mut words = target.split_whitespace();
    for expected in ["cargo", "run", "-p", "loom-walk", "--"] {
        if words.next()? != expected {
            return None;
        }
    }
    let name = words.next()?;
    if words.next().is_some() {
        return None;
    }
    crate::walk::lookup(name).map(|_| name)
}

/// Select referenced nonpending analyses; native producers remain independently required.
///
/// # Errors
/// Rejects unregistered nonpending Check bindings rather than executing their text.
pub fn names(annotations: &[Annotation]) -> Result<BTreeSet<String>, Error> {
    let mut names = BTreeSet::new();
    for annotation in annotations
        .iter()
        .filter(|annotation| annotation.tier == Tier::Check && !annotation.pending)
    {
        if let Some(name) = walk_name(&annotation.target) {
            names.insert(name.to_owned());
        } else if !annotation.target.starts_with("nix build ")
            && !annotation.target.starts_with("nix report ")
            && annotation.target != loom_gate::provider::PROJECT_CONTEXT_TARGET
        {
            return Err(Error::Unsupported {
                source_spec: annotation.source_spec.clone(),
                line: annotation.line,
                target: annotation.target.clone(),
            });
        }
    }
    Ok(names)
}

#[derive(Debug, Display, Error)]
pub enum Error {
    /// `{source_spec:?}:{line}`: unsupported source-analysis binding `{target}`; register a tracked walk or bind a native producer
    Unsupported {
        source_spec: PathBuf,
        line: u32,
        target: String,
    },
}

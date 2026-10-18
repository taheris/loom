//! Typed task data; stored metadata is candidate data, never workflow authority.
use std::collections::BTreeMap;

pub use loom_events::identifier::{BeadId as Id, MoleculeId as GroupId};
mod label;
mod priority;
mod record;
mod revision;
pub use revision::{ParseRevisionError, Revision};

pub use label::{Label, ParseLabelError};
pub use priority::{ParsePriorityError, Priority};
pub(crate) use record::BlockingDependent;
pub use record::{
    Bead as Record, DependencySnapshot as Graph, IssueType as Kind, MolProgress as Progress, Status,
};
pub use record::{Dependency, DependencyKind, Molecule};

#[derive(Debug, Clone, Default)]
pub struct Create {
    pub title: String,
    pub description: String,
    pub status: Option<Status>,
    pub kind: Option<Kind>,
    pub priority: Option<Priority>,
    pub labels: Vec<Label>,
    pub parent: Option<Id>,
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub notes: Option<String>,
}

/// Only declared fields change; metadata entries are targeted, not a replacement object.
#[derive(Debug, Clone, Default)]
pub struct Update {
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<Status>,
    pub priority: Option<Priority>,
    pub add_labels: Vec<Label>,
    pub remove_labels: Vec<Label>,
    pub notes: Option<String>,
    pub append_notes: Option<String>,
    /// Unsupported value representations must be refused before writing, never coerced.
    pub set_metadata: BTreeMap<String, serde_json::Value>,
    pub unset_metadata: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Query {
    pub statuses: Vec<Status>,
    pub label: Option<Label>,
    pub label_any: Vec<Label>,
    pub parent: Option<Id>,
    pub kind: Option<Kind>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct Ready {
    pub label: Option<Label>,
    pub exclude_labels: Vec<Label>,
    pub parent: Option<Id>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    Blocks,
    DiscoveredFrom,
}

/// A status predicate is not a revision check, even if the status is unchanged.
#[derive(Debug, Clone, Default)]
pub enum Guard {
    #[default]
    None,
    Status(Status),
    Revision(Revision),
}

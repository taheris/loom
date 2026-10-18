//! Beads adapter with consumer-supplied subprocess transport.
mod client;
mod failure;
mod runner;
mod tracking;

pub use crate::task::{
    Graph as DependencySnapshot, Kind as IssueType, Label, Molecule, ParseLabelError,
    ParsePriorityError, Priority, Progress as MolProgress, Record as Bead, Status,
};
pub use client::{BdClient, CreateOpts, ListOpts, ReadyOpts, UpdateOpts};
pub use failure::BdError;
pub use runner::{CommandRunner, RunOutput};

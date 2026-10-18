//! Production Beads composition over the extractable tracking adapter.
mod label;
mod runner;
pub use label::LabelExt;

pub use loom_issues::beads::{
    BdError, Bead, CommandRunner, CreateOpts, DependencySnapshot, IssueType, Label, ListOpts,
    MolProgress, Molecule, ParseLabelError, ParsePriorityError, Priority, ReadyOpts, RunOutput,
    Status, UpdateOpts,
};
pub use runner::TokioRunner;

pub type BdClient<R = TokioRunner> = loom_issues::beads::BdClient<R>;

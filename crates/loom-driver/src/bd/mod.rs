//! Typed wrapper around the `bd` CLI.
//!
//! `BdClient` invokes `bd` via `tokio::process::Command` with each argument
//! passed through `.arg()` — never shell interpolation — and parses
//! `--json` output into typed Rust structs. Subprocess execution flows
//! through the [`CommandRunner`] trait so unit tests can substitute a
//! capturing fake without spawning a real binary.
//!
//! `BdError` lives in this module (NF-5: nested module structure with
//! per-module error enums).

mod client;
mod error;
mod label;
mod models;
mod runner;

pub use client::{BdClient, CreateOpts, ListOpts, ReadyOpts, UpdateOpts};
pub use error::BdError;
pub use label::Label;
pub use models::{Bead, MolProgress, Molecule};
pub use runner::{CommandRunner, RunOutput, TokioRunner};

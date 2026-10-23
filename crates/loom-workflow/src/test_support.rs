//! Shared test-only isolated native Beads fixture.

#[cfg(test)]
pub mod acceptance;

#[cfg(test)]
pub use native::{IsolatedBeads, client};

#[cfg(test)]
mod native;

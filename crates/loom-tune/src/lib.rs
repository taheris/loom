//! Internal tuning registry, case, score, and proposal types.

pub mod case;
pub mod checker;
pub mod config;
pub mod evidence;
pub mod executor;
pub mod gate;
pub mod plan;
pub mod proposal;
pub mod score;
pub mod target;

#[cfg(test)]
mod test;

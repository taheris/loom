//! `loom-walk` — `[check]`-tier verifier binary.
//!
//! Dispatches a named walk function over the source tree (filtered by
//! `LOOM_FILES` when set) and reports the verdict per the verifier-runner
//! contract in `specs/gate.md`:
//!
//! - **argv:** the walk name as the single positional argument.
//! - **env:** `LOOM_FILES` (colon-joined paths) filters the walk's input
//!   set; absent means the walk scans its declared scope.
//! - **stdout:** one JSON line `{"pass": bool, "evidence": "<msg>"}`.
//! - **exit code:** `0` for a passing verdict, `1` for a failing verdict,
//!   `2` for usage / dispatch errors (missing walk name, unknown walk
//!   name, internal serialisation failure).
//!
//! The walks themselves live in `walk/<name>.rs` modules; this file owns
//! argv parsing and exit-code translation only.

mod walk;

use std::process::ExitCode;

use displaydoc::Display;
use thiserror::Error;

use walk::{Verdict, WalkInput};

/// Dispatch errors surfaced to stderr before the process exits with code
/// `2`. Per `specs/gate.md` a failing verdict (exit 1) is reserved
/// for walks whose verdict is `false`; usage and dispatch failures use a
/// different exit code so the gate can distinguish "verifier ran and
/// said no" from "verifier did not run".
#[derive(Debug, Display, Error)]
enum DispatchError {
    /// usage: loom-walk <walk-name>; available walks: {available}
    MissingWalkName { available: String },
    /// unknown walk `{name}`; available walks: {available}
    UnknownWalk { name: String, available: String },
    /// failed to serialise verdict: {source}
    SerialiseVerdict {
        #[source]
        source: serde_json::Error,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(verdict) => {
            if verdict.pass {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(err) => {
            eprintln!("loom-walk: {err}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<Verdict, DispatchError> {
    let mut args = std::env::args();
    let _bin = args.next();
    let name = args.next().ok_or_else(|| DispatchError::MissingWalkName {
        available: walk::names_pretty(),
    })?;
    let walk = walk::lookup(&name).ok_or_else(|| DispatchError::UnknownWalk {
        name: name.clone(),
        available: walk::names_pretty(),
    })?;
    let input = WalkInput::from_env();
    let verdict = (walk.run)(&input);
    let line = serde_json::to_string(&verdict)
        .map_err(|source| DispatchError::SerialiseVerdict { source })?;
    println!("{line}");
    Ok(verdict)
}

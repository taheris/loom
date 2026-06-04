//! `loom-walk` — `[check]`-tier verifier binary.
//!
//! Dispatches one or more named walk functions over the source tree
//! (filtered by `LOOM_FILES` when set) and reports verdicts per the
//! verifier-runner contract in `specs/gate.md`:
//!
//! - **argv:** one or more walk names as positional arguments. Batching
//!   N names into a single process invocation collapses N cargo
//!   start-up costs into one — the dominant cost on warm caches. A
//!   trailing `--print-inputs` flag switches to collect mode: the binary
//!   reports each named walk's scanned file set (the input-query protocol
//!   in `specs/gate.md` § Verifier inputs) instead of running the walks.
//! - **env:** `LOOM_FILES` (colon-joined paths) filters every walk's
//!   input set; absent means each walk scans its declared scope.
//! - **stdout:** one JSON line per walk name, in argv order:
//!   `{"target":"<name>","pass":bool,"evidence":"<msg>"}`. The
//!   `target` field lets the gate's `json-lines` runner parser map
//!   each verdict back to its annotation when the batch covers many
//!   annotations.
//! - **exit code:** `0` when every requested walk passes, `1` when any
//!   walk fails, `2` for usage / dispatch errors (no walk name,
//!   unknown walk name, internal serialisation failure).
//!
//! The walks themselves live in `walk/<name>.rs` modules; this file owns
//! argv parsing and exit-code translation only.

mod walk;

use std::process::ExitCode;

use displaydoc::Display;
use serde::Serialize;
use thiserror::Error;

use walk::{Verdict, WalkInput};

/// Collect-mode flag: report scanned inputs instead of running the walks.
const PRINT_INPUTS_FLAG: &str = "--print-inputs";

/// Dispatch errors surfaced to stderr before the process exits with code
/// `2`. Per `specs/gate.md` a failing verdict (exit 1) is reserved
/// for walks whose verdict is `false`; usage and dispatch failures use a
/// different exit code so the gate can distinguish "verifier ran and
/// said no" from "verifier did not run".
#[derive(Debug, Display, Error)]
enum DispatchError {
    /// usage: loom-walk <walk-name> [<walk-name>...]; available walks: {available}
    MissingWalkName { available: String },
    /// unknown walk `{name}`; available walks: {available}
    UnknownWalk { name: String, available: String },
    /// failed to serialise verdict: {source}
    SerialiseVerdict {
        #[source]
        source: serde_json::Error,
    },
}

/// Per-target verdict line emitted to stdout, one per requested walk
/// name. Matches the `json-lines` parser in `loom-gate`:
/// `{"target":"<name>","pass":bool,"evidence":"<msg>"}`.
#[derive(Debug, Serialize)]
struct TargetVerdict<'a> {
    target: &'a str,
    pass: bool,
    evidence: &'a str,
}

fn main() -> ExitCode {
    match run() {
        Ok(all_pass) => {
            if all_pass {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(err) => {
            eprintln!("loom-walk: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<bool, DispatchError> {
    let mut args = std::env::args();
    let _bin = args.next();
    let raw: Vec<String> = args.collect();
    // `--print-inputs` switches the binary into collect mode: instead of
    // running each named walk, report the file set it scans so the gate can
    // scope it under `--files`. The runner template places the flag after
    // the walk names (`cargo run -p loom-walk -- <name>... --print-inputs`).
    let print_inputs = raw.iter().any(|arg| arg == PRINT_INPUTS_FLAG);
    let names: Vec<String> = raw
        .into_iter()
        .filter(|arg| arg != PRINT_INPUTS_FLAG)
        .collect();
    if names.is_empty() {
        return Err(DispatchError::MissingWalkName {
            available: walk::names_pretty(),
        });
    }
    for name in &names {
        if walk::lookup(name).is_none() {
            return Err(DispatchError::UnknownWalk {
                name: name.clone(),
                available: walk::names_pretty(),
            });
        }
    }
    if print_inputs {
        let document = walk::render_print_inputs(&names)
            .map_err(|source| DispatchError::SerialiseVerdict { source })?;
        println!("{document}");
        return Ok(true);
    }
    let input = WalkInput::from_env();
    let mut all_pass = true;
    for name in &names {
        let walk = walk::lookup(name).ok_or_else(|| DispatchError::UnknownWalk {
            name: name.clone(),
            available: walk::names_pretty(),
        })?;
        let Verdict { pass, evidence } = (walk.run)(&input);
        all_pass &= pass;
        let line = serde_json::to_string(&TargetVerdict {
            target: name,
            pass,
            evidence: &evidence,
        })
        .map_err(|source| DispatchError::SerialiseVerdict { source })?;
        println!("{line}");
    }
    Ok(all_pass)
}

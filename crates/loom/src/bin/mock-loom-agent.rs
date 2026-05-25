//! Test-only mock agent. Stands in for the wrapix → pi binary chain
//! when an integration test wants to drive `loom run` end-to-end
//! against a controllable backend. Speaks the pi-mono JSONL RPC
//! protocol — enough of it to reach the marker-emission point and
//! exit cleanly — and chooses what marker text to emit based on the
//! `LOOM_TEST_AGENT_MODE` env var.
//!
//! Selection: the test points `LOOM_WRAPIX_BIN` at this binary. loom
//! invokes it with `spawn --spawn-config <path> --stdio` (the
//! production wrapix CLI shape) plus whatever spawn-config the
//! production code resolved. The mock ignores those args — there is
//! no container, no wrapper — and immediately starts speaking pi-mono
//! on its own stdin/stdout. That collapses two test-harness layers
//! into one Rust binary, replacing the previous bash `install_wrapix_shim`
//! + `tests/mock-pi/pi.sh` pair without extending either.
//!
//! Modes (set `LOOM_TEST_AGENT_MODE`):
//!
//! - `blocked-marker`  — emit `<reason>\nLOOM_BLOCKED` and `agent_end`.
//!   Drives the run gate's `AgentOutcome::Blocked` branch.
//! - `clarify-marker`  — emit `<question>\nLOOM_CLARIFY` and `agent_end`.
//!   Drives the run gate's `AgentOutcome::Clarify` branch.
//! - `complete-marker` — emit `done\nLOOM_COMPLETE` and `agent_end`.
//!   Used by the negative case in B6: the agent should also call
//!   `bd close` itself, but the test asserts the *driver* doesn't.
//! - `no-marker`       — emit a plain message and `agent_end` without
//!   any `LOOM_*` line. Exercises the swallowed-marker recovery path.
//! - `concern-marker`  — emit a single `LOOM_CONCERN: verifier-bypass -- …`
//!   line and `agent_end`. Drives the review-phase concern branch of
//!   the push gate (`PushGateRefuseCause::ReviewConcern`).
//! - `concern-then-complete` — emit `LOOM_CONCERN: … -- …` then
//!   `LOOM_COMPLETE` on a later line. The final-line parser must pick
//!   the trailing `LOOM_COMPLETE`; this is the literal May-19 sequence.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helper: loud failure on protocol violations is the desired behaviour"
)]

use std::env;
use std::io::{self, BufRead, Write};
use std::process::ExitCode;

const MODE_BLOCKED: &str = "blocked-marker";
const MODE_CLARIFY: &str = "clarify-marker";
const MODE_COMPLETE: &str = "complete-marker";
const MODE_NO_MARKER: &str = "no-marker";
const MODE_CONCERN: &str = "concern-marker";
const MODE_CONCERN_THEN_COMPLETE: &str = "concern-then-complete";

const BLOCKED_REASON: &str = "spec section is missing the schema for this bead";
const CLARIFY_QUESTION: &str = "which deploy-key path should the runner image mount?";
const CONCERN_LINE: &str =
    "LOOM_CONCERN: verifier-bypass -- the only [verify] for this bead mocks the agent backend";

fn main() -> ExitCode {
    let mode = env::var("LOOM_TEST_AGENT_MODE").unwrap_or_else(|_| MODE_COMPLETE.to_string());

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    // Step 1 — handshake. Production loom sends a `get_commands` probe
    // as the first JSONL line; the agent must respond with the full
    // command list before any further protocol traffic flows. Pi-mono's
    // unit tests in loom-agent assert this exact set of commands.
    let probe_line = match read_line(&stdin) {
        Some(l) => l,
        None => {
            eprintln!("mock-loom-agent: stdin closed before probe");
            return ExitCode::from(2);
        }
    };
    let probe_id = extract_field("id", &probe_line).unwrap_or_else(|| "probe-0".to_string());
    let probe_response = format!(
        r#"{{"type":"response","id":"{probe_id}","command":"get_commands","success":true,"data":["prompt","steer","abort","set_model","compact","get_session_stats"]}}"#,
    );
    emit(&mut stdout, &probe_response);

    // Step 2 — read the prompt line. We don't need its contents; the
    // mode env var carries the test's intent already.
    let _prompt = read_line(&stdin);

    // Step 3 — emit the marker as a message_delta. parse_exit_signal
    // scans the accumulated text for `LOOM_BLOCKED` / `LOOM_CLARIFY` /
    // `LOOM_COMPLETE` markers; emitting the reason on the prior line
    // matches `reason_for`'s "non-empty line before the marker" rule.
    match mode.as_str() {
        MODE_BLOCKED => {
            emit_message_delta(&mut stdout, BLOCKED_REASON);
            emit_message_delta(&mut stdout, "LOOM_BLOCKED");
        }
        MODE_CLARIFY => {
            emit_message_delta(&mut stdout, CLARIFY_QUESTION);
            emit_message_delta(&mut stdout, "LOOM_CLARIFY");
        }
        MODE_COMPLETE => {
            emit_message_delta(&mut stdout, "did the work");
            emit_message_delta(&mut stdout, "LOOM_COMPLETE");
        }
        MODE_NO_MARKER => {
            emit_message_delta(&mut stdout, "ran without emitting a verdict");
        }
        MODE_CONCERN => {
            emit_message_delta(&mut stdout, CONCERN_LINE);
        }
        MODE_CONCERN_THEN_COMPLETE => {
            // The May-19 sequence: a `LOOM_CONCERN: …` line followed by a
            // separate `LOOM_COMPLETE` line. `parse_exit_signal` reads
            // only the final non-empty line, so the trailing
            // `LOOM_COMPLETE` wins. Newlines are embedded in the delta
            // payload — pi-mono text_deltas concatenate verbatim, so
            // separate emits without a trailing newline collapse onto
            // one line and trigger the multi-marker swallow path.
            emit_message_delta(&mut stdout, &format!("{CONCERN_LINE}\n"));
            emit_message_delta(&mut stdout, "did the work\n");
            emit_message_delta(&mut stdout, "LOOM_COMPLETE");
        }
        other => {
            eprintln!("mock-loom-agent: unknown LOOM_TEST_AGENT_MODE {other}");
            return ExitCode::from(2);
        }
    }

    // Step 4 — close the session. agent_end terminates the pi-mono
    // event stream; the driver-side loop reads up to that marker and
    // moves on. Exit 0 unconditionally — the verdict comes from the
    // marker text, not the exit code.
    emit(&mut stdout, r#"{"type":"agent_end","messages":[]}"#);
    ExitCode::SUCCESS
}

fn read_line(stdin: &io::Stdin) -> Option<String> {
    let mut buf = String::new();
    let n = stdin.lock().read_line(&mut buf).ok()?;
    if n == 0 {
        return None;
    }
    if buf.ends_with('\n') {
        buf.pop();
        if buf.ends_with('\r') {
            buf.pop();
        }
    }
    Some(buf)
}

fn emit(stdout: &mut io::Stdout, line: &str) {
    writeln!(stdout, "{line}").expect("emit line");
    stdout.flush().expect("flush stdout");
}

fn emit_message_delta(stdout: &mut io::Stdout, text: &str) {
    let event = serde_json::json!({
        "type": "message_update",
        "assistantMessageEvent": {
            "type": "text_delta",
            "text": text,
        },
    });
    emit(stdout, &event.to_string());
}

/// Minimal JSON field extractor — pulls a string value for `field` out
/// of a flat JSON line. The probe line is shallow (`{"type":"command",
/// "id":"…","name":"get_commands"}`) so this avoids pulling serde_json's
/// parser into the read path; if the field is absent we fall back to a
/// stand-in id and the caller decides whether that's fatal.
fn extract_field(field: &str, line: &str) -> Option<String> {
    let needle = format!("\"{field}\":\"");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

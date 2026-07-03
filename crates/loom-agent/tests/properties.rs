//! Property-based tests for the Loom agent parsers.
//!
//! Per `specs/tests.md` (Architecture / Property-Based Testing), this
//! crate owns invariants for the three parsers it contains: the JSONL line
//! parser shared between backends, the Pi RPC protocol parser, and the
//! Claude stream-json parser. Each gets a `proptest!` block whose
//! invariants are stated as comments above the test.
//!
//! `PROPTEST_CASES=32` under `nix flake check`; local exhaustive runs
//! override via env var (`PROPTEST_CASES=2048+`).

use loom_agent::claude::messages::ClaudeMessage;
use loom_agent::claude::parser::ClaudeParser;
use loom_agent::pi::parser::PiParser;
use loom_driver::agent::{LineParse, MAX_LINE_BYTES, ProtocolError};
use loom_test_support::CI_PROPTEST_CASES;
use proptest::prelude::*;

fn pi_parser() -> PiParser {
    PiParser::new()
}

fn claude_parser() -> ClaudeParser {
    ClaudeParser::new(Vec::new())
}

/// JSONL framing constant is the 10 MB cap documented in the spec.
/// Pinned here so a stray edit to `MAX_LINE_BYTES` surfaces as a test
/// failure rather than silent change to the protection envelope.
#[test]
fn max_line_bytes_is_ten_megabytes() {
    assert_eq!(MAX_LINE_BYTES, 10 * 1024 * 1024);
}

// ----------------------------------------------------------------------------
// JSONL line parser invariants
//
// `parse_line` must never panic on arbitrary input. Pi defensively skips
// malformed stdout lines with no events, while Claude reports malformed
// stream-json as `Err(ProtocolError::*)`. Exercising both with the same
// generator catches divergence between the backend contracts.
// ----------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CI_PROPTEST_CASES))]

    #[test]
    fn jsonl_arbitrary_bytes_never_panic(input in ".{0,512}") {
        // PiParser::parse_line — no panic regardless of input shape.
        let _ = pi_parser().parse_line(&input);
        // ClaudeParser::parse_line — same contract.
        let _ = claude_parser().parse_line(&input);
    }

    #[test]
    fn jsonl_malformed_line_emits_no_events(input in "[^{].{0,256}") {
        // A line that does not start with `{` is never valid JSONL for these
        // parsers. Pi skips defensively; Claude errors. Neither may smuggle
        // events out from malformed bytes.
        let pi_res = pi_parser().parse_line(&input);
        if let Ok(parsed) = pi_res {
            prop_assert!(parsed.events.is_empty(), "pi parser emitted events for non-JSON line");
            prop_assert!(parsed.response.is_none(), "pi parser emitted a response for non-JSON line");
        }

        let cl_res = claude_parser().parse_line(&input);
        prop_assert!(cl_res.is_err(), "claude parser accepted non-JSON line");
    }
}

// ----------------------------------------------------------------------------
// Pi protocol parser invariants
//
// Round-trip identity: every prompt/steer/abort encoded line is valid JSON
// whose `type` and `message` fields match the input. Unknown message types
// (i.e. lines carrying a `type` and `id` that the parser does not recognise)
// must surface as `ProtocolError::UnknownMessageType`. Arbitrary bytes
// never panic.
// ----------------------------------------------------------------------------

/// Pi command-name strategy that never overlaps the parser's known set
/// (`response`, `extension_ui_request`). Any other type carrying both
/// `type` and `id` falls through to the `UnknownMessageType` arm.
fn pi_unknown_type() -> impl Strategy<Value = String> {
    "[a-z][a-z_]{0,15}".prop_filter("must not collide with known types", |s| {
        !matches!(s.as_str(), "response" | "extension_ui_request")
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CI_PROPTEST_CASES))]

    #[test]
    fn pi_encode_prompt_round_trips(msg in ".{0,128}") {
        let parser = pi_parser();
        let line = parser.encode_prompt(&msg).unwrap();
        prop_assert!(line.ends_with('\n'));
        let v: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        prop_assert_eq!(v["type"].as_str(), Some("prompt"));
        prop_assert_eq!(v["message"].as_str(), Some(msg.as_str()));
    }

    #[test]
    fn pi_encode_steer_round_trips(msg in ".{0,128}") {
        let parser = pi_parser();
        let line = parser.encode_steer(&msg).unwrap();
        let v: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        prop_assert_eq!(v["type"].as_str(), Some("steer"));
        prop_assert_eq!(v["message"].as_str(), Some(msg.as_str()));
    }

    #[test]
    fn pi_unknown_message_type_surfaces_typed_error(
        ty in pi_unknown_type(),
        id in "[a-z0-9-]{1,16}",
    ) {
        let line = format!(r#"{{"type":"{ty}","id":"{id}"}}"#);
        let err = pi_parser().parse_line(&line).err()
            .expect("unknown type with id must error");
        match err {
            ProtocolError::UnknownMessageType(t) => prop_assert_eq!(t, ty),
            other => prop_assert!(
                false,
                "expected UnknownMessageType, got {other:?}",
            ),
        }
    }

    #[test]
    fn pi_arbitrary_bytes_never_panic(input in any::<Vec<u8>>()) {
        // Lossy decode → arbitrary &str that may or may not be valid JSON.
        let s = String::from_utf8_lossy(&input);
        let _ = pi_parser().parse_line(&s);
    }
}

// ----------------------------------------------------------------------------
// Claude stream-json parser invariants
//
// Round-trip identity: `system` and `result` lines deserialize into the
// expected `ClaudeMessage` variants with their fields preserved. Unknown
// message types fall through `#[serde(other)]` to the `Unknown` variant
// without erroring. Arbitrary bytes never panic.
// ----------------------------------------------------------------------------

/// Claude type strategy whose values never collide with a known variant.
/// `#[serde(other)]` only fires when the tag does not match any known
/// variant; collisions would cause a real parse attempt that may fail.
fn claude_unknown_type() -> impl Strategy<Value = String> {
    "[a-z][a-z_]{0,15}".prop_filter("must not collide with known types", |s| {
        !matches!(
            s.as_str(),
            "system" | "assistant" | "user" | "result" | "control_request",
        )
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CI_PROPTEST_CASES))]

    #[test]
    fn claude_system_round_trips(
        subtype in "[a-z]{1,16}",
        session_id in proptest::option::of("[a-z0-9-]{1,16}"),
    ) {
        let line = match &session_id {
            Some(sid) => format!(
                r#"{{"type":"system","subtype":"{subtype}","session_id":"{sid}"}}"#
            ),
            None => format!(r#"{{"type":"system","subtype":"{subtype}"}}"#),
        };
        let msg: ClaudeMessage = serde_json::from_str(&line).unwrap();
        match msg {
            ClaudeMessage::System {
                subtype: parsed_subtype,
                session_id: parsed_sid,
            } => {
                prop_assert_eq!(parsed_subtype, subtype);
                prop_assert_eq!(parsed_sid.map(|s| s.as_str().to_string()), session_id);
            }
            other => prop_assert!(false, "expected System, got {other:?}"),
        }
    }

    #[test]
    fn claude_result_round_trips(
        is_success in any::<bool>(),
        // Cost is generated as an integer cents value and divided to give a
        // float that round-trips through serde_json without ULP drift —
        // arbitrary f64s lose precision in the JSON text round-trip and
        // would flake the assertion on otherwise-valid behavior.
        cost_cents in proptest::option::of(0_u32..1_000_000),
        duration in proptest::option::of(0_u64..100_000),
        num_turns in proptest::option::of(0_u32..1000),
    ) {
        let subtype = if is_success { "success" } else { "error" };
        let cost = cost_cents.map(|c| f64::from(c) / 100.0);
        let mut obj = serde_json::Map::new();
        obj.insert("type".into(), "result".into());
        obj.insert("subtype".into(), subtype.into());
        if let Some(c) = cost {
            obj.insert("total_cost_usd".into(), serde_json::Value::from(c));
        }
        if let Some(d) = duration {
            obj.insert("duration_ms".into(), serde_json::Value::from(d));
        }
        if let Some(n) = num_turns {
            obj.insert("num_turns".into(), serde_json::Value::from(n));
        }
        let line = serde_json::Value::Object(obj).to_string();

        let msg: ClaudeMessage = serde_json::from_str(&line).unwrap();
        match msg {
            ClaudeMessage::Result {
                subtype: parsed_subtype,
                total_cost_usd,
                duration_ms,
                num_turns: parsed_turns,
                ..
            } => {
                prop_assert_eq!(parsed_subtype, subtype);
                prop_assert_eq!(total_cost_usd, cost);
                prop_assert_eq!(duration_ms, duration);
                prop_assert_eq!(parsed_turns, num_turns);
            }
            other => prop_assert!(false, "expected Result, got {other:?}"),
        }
    }

    #[test]
    fn claude_unknown_type_falls_through_serde_other(ty in claude_unknown_type()) {
        let line = format!(r#"{{"type":"{ty}","extra":1}}"#);
        let msg: ClaudeMessage = serde_json::from_str(&line).unwrap();
        prop_assert!(matches!(msg, ClaudeMessage::Unknown));
    }

    #[test]
    fn claude_encode_prompt_round_trips(msg in ".{0,128}") {
        let parser = claude_parser();
        let line = parser.encode_prompt(&msg).unwrap();
        prop_assert!(line.ends_with('\n'));
        let v: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        prop_assert_eq!(v["type"].as_str(), Some("user"));
        prop_assert_eq!(v["message"]["role"].as_str(), Some("user"));
        prop_assert_eq!(v["message"]["content"].as_str(), Some(msg.as_str()));
    }

    #[test]
    fn claude_arbitrary_bytes_never_panic(input in any::<Vec<u8>>()) {
        let s = String::from_utf8_lossy(&input);
        let _ = claude_parser().parse_line(&s);
    }
}

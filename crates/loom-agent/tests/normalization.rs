//! Cross-backend `ParsedLine` normalization and stall-timeout machinery.
//!
//! Two contracts from `specs/tests.md` § loom-agent land here:
//!
//! - **Event normalization** — for equivalent agent behavior (a text
//!   delta, a tool call/result pair, an error, the session ending),
//!   both backends emit the same `ParsedAgentEvent` shapes through the
//!   `LineParse` surface. Per-protocol terminal markers diverge in known
//!   ways (claude pairs `result/success` into `TurnEnd` + `SessionComplete`;
//!   pi's `agent_end` lowers to a single `SessionComplete` with a
//!   synthesized `cost_usd: None`) and are pinned explicitly here so a
//!   drift surfaces as a normalization failure rather than a silent skew.
//! - **Stall watchdog** — when no JSONL line arrives within a 5-minute
//!   stall window, the warn-and-continue pattern emits a `warn!` and
//!   keeps polling rather than aborting. Driven by [`MockClock`] under
//!   `#[tokio::test(start_paused = true)]` so the entire window resolves
//!   in zero wall time. The production user of this pattern lives in
//!   `loom-workflow::run_agent_classified`; the integration test in
//!   `crates/loom/tests/spawn_dispatch.rs` exercises that path end-to-end
//!   over a real container. Here the unit-level test pins the primitives
//!   the production loop is built on.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::future::pending;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use loom_agent::claude::parser::ClaudeParser;
use loom_agent::pi::parser::PiParser;
use loom_driver::agent::{LineParse, ParsedLine};
use loom_driver::clock::{Clock, MockClock};
use loom_events::ParsedAgentEvent;
use tracing::warn;

// ----------------------------------------------------------------------------
// helpers
// ----------------------------------------------------------------------------

fn pi_parser() -> PiParser {
    PiParser::new()
}

fn claude_parser() -> ClaudeParser {
    ClaudeParser::new(Vec::new())
}

fn parse_pi(line: &str) -> ParsedLine {
    pi_parser()
        .parse_line(line)
        .expect("pi fixture should parse cleanly")
}

fn parse_claude(line: &str) -> ParsedLine {
    claude_parser()
        .parse_line(line)
        .expect("claude fixture should parse cleanly")
}

// ----------------------------------------------------------------------------
// ParsedLine::response semantics across backends
// ----------------------------------------------------------------------------

/// Claude's `control_request` is the only branch that populates
/// `response`: the parser auto-approves and hands back a `control_response`
/// JSONL line for the session to write to stdin. The `events` vec stays
/// empty — no agent-visible event surfaces from a permission probe.
#[test]
fn claude_control_request_populates_response_with_empty_events() {
    let line =
        r#"{"type":"control_request","id":"req_99","tool":"Read","input":{"path":"/etc/x"}}"#;
    let p = parse_claude(line);
    assert!(p.events.is_empty(), "control_request emits no events");
    let resp = p.response.expect("control_request must produce a response");
    assert!(
        resp.contains(r#""type":"control_response""#),
        "response must be a control_response JSONL line: {resp}",
    );
    assert!(
        resp.contains(r#""id":"req_99""#),
        "response echoes request id: {resp}"
    );
    assert!(
        resp.contains(r#""approved":true"#),
        "default policy auto-approves: {resp}"
    );
    assert!(
        resp.ends_with('\n'),
        "response must end with newline for stdin framing"
    );
}

/// Every non-`control_request` claude line keeps `response = None`:
/// `system/init`, `assistant`, `user`, `result`, and `Unknown` lines all
/// surface events (or none) without ever writing back to stdin. The
/// session layer's `next_event` only writes when `response.is_some()`,
/// so a regression here would silently inject noise into the agent's
/// stdin.
#[test]
fn claude_non_control_events_leave_response_none() {
    let lines = [
        r#"{"type":"system","subtype":"init","session_id":"s1"}"#,
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}"#,
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu","content":"ok","is_error":false}]}}"#,
        r#"{"type":"result","subtype":"success","total_cost_usd":0.1}"#,
        r#"{"type":"newfangled","extra":1}"#,
    ];
    for line in lines {
        let p = parse_claude(line);
        assert!(
            p.response.is_none(),
            "non-control line set response: {line}"
        );
    }
}

/// Pi events never populate `response`. Only the `extension_ui_request`
/// branch writes back (auto-cancel for blocking methods), and that lives
/// outside the event surface. Pin every documented event type and the
/// `response` envelope so a drift in the parser's response-writing logic
/// fails this test rather than appearing as a stuck agent in the field.
#[test]
fn pi_events_and_responses_leave_response_none() {
    let lines = [
        r#"{"type":"response","id":"r1","command":"prompt","success":true}"#,
        r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","text":"hi"}}"#,
        r#"{"type":"tool_execution_start","toolCallId":"tc-1","toolName":"Read","args":{}}"#,
        r#"{"type":"tool_execution_end","toolCallId":"tc-1","result":"ok","isError":false}"#,
        r#"{"type":"turn_end","message":{},"toolResults":[]}"#,
        r#"{"type":"agent_end","messages":[]}"#,
        r#"{"type":"compaction_start","reason":"threshold"}"#,
        r#"{"type":"compaction_end","aborted":false,"willRetry":false}"#,
    ];
    for line in lines {
        let p = parse_pi(line);
        assert!(p.response.is_none(), "pi line populated response: {line}");
    }
}

// ----------------------------------------------------------------------------
// ParsedLine::events arity per terminal-message contract
// ----------------------------------------------------------------------------

/// Claude's `result/success` ships *two* events in one `ParsedLine`:
/// `TurnEnd` followed by `SessionComplete`. The session's
/// `AgentSession::next_event` buffers the trailing event into `pending`
/// and yields the first; reordering or dropping either event would
/// invalidate `BeadOutcome::Done` accounting that keys off `exit_code`
/// inside `SessionComplete`.
#[test]
fn claude_result_success_emits_turn_end_then_session_complete() {
    let line = r#"{"type":"result","subtype":"success","total_cost_usd":0.42}"#;
    let p = parse_claude(line);
    assert_eq!(p.events.len(), 2, "result/success must yield 2 events");
    assert!(
        matches!(p.events[0], ParsedAgentEvent::TurnEnd),
        "first event must be TurnEnd, got {:?}",
        p.events[0],
    );
    match &p.events[1] {
        ParsedAgentEvent::SessionComplete {
            exit_code,
            cost_usd,
        } => {
            assert_eq!(*exit_code, 0, "success maps to exit_code 0");
            assert_eq!(*cost_usd, Some(0.42), "cost_usd flows from total_cost_usd");
        }
        other => panic!("expected SessionComplete, got {other:?}"),
    }
}

/// Claude's `result/error` ships *two* events: `Error` with the
/// `result` body as the message, followed by `SessionComplete` with
/// `exit_code: 1`. The error event arrives first so observers see the
/// failure cause before the run-complete signal.
#[test]
fn claude_result_error_emits_error_then_session_complete() {
    let line = r#"{"type":"result","subtype":"error","result":"boom","total_cost_usd":0.05}"#;
    let p = parse_claude(line);
    assert_eq!(p.events.len(), 2, "result/error must yield 2 events");
    match &p.events[0] {
        ParsedAgentEvent::Error { message } => assert_eq!(message, "boom"),
        other => panic!("first event must be Error, got {other:?}"),
    }
    match &p.events[1] {
        ParsedAgentEvent::SessionComplete {
            exit_code,
            cost_usd,
        } => {
            assert_eq!(*exit_code, 1, "error maps to exit_code 1");
            assert_eq!(*cost_usd, Some(0.05));
        }
        other => panic!("expected SessionComplete, got {other:?}"),
    }
}

/// Pi's `turn_end` lowers to exactly one `TurnEnd` event — no companion
/// `SessionComplete`. Claude pairs the two because its `result` line is
/// terminal; pi distinguishes turn from session, and the workflow loop
/// keeps draining after a `TurnEnd`.
#[test]
fn pi_turn_end_emits_single_turn_end_event() {
    let line = r#"{"type":"turn_end","message":{},"toolResults":[]}"#;
    let p = parse_pi(line);
    assert_eq!(p.events.len(), 1, "pi turn_end is a single event");
    assert!(
        matches!(p.events[0], ParsedAgentEvent::TurnEnd),
        "got {:?}",
        p.events[0],
    );
}

/// Pi's `agent_end` lowers to exactly one `SessionComplete` with a
/// synthesized `exit_code: 0` and `cost_usd: None`. Pi has no
/// equivalent of claude's `total_cost_usd`; the `None` here keeps
/// `SessionOutcome::cost_usd` truthful (cost is unknown, not zero).
#[test]
fn pi_agent_end_emits_single_session_complete_event() {
    let line = r#"{"type":"agent_end","messages":[]}"#;
    let p = parse_pi(line);
    assert_eq!(p.events.len(), 1, "pi agent_end is a single event");
    match &p.events[0] {
        ParsedAgentEvent::SessionComplete {
            exit_code,
            cost_usd,
        } => {
            assert_eq!(*exit_code, 0, "agent_end implies clean exit");
            assert!(
                cost_usd.is_none(),
                "pi does not surface cost on session end"
            );
        }
        other => panic!("expected SessionComplete, got {other:?}"),
    }
}

// ----------------------------------------------------------------------------
// Cross-backend event normalization
//
// For events with semantic equivalents in both protocols, the parsed
// shapes must be byte-identical at the `ParsedAgentEvent` surface.
// Anything that diverges between backends (claude's extra `TurnEnd`
// before `SessionComplete`; pi's synthesized `cost_usd: None`) is
// pinned in the dedicated tests above; this section asserts the
// matching subset.
// ----------------------------------------------------------------------------

/// Pi's `text_delta` and claude's assistant `text` content block both
/// surface as `ParsedAgentEvent::TextDelta { text }`. Same backend, same
/// text → same event shape.
#[test]
fn both_backends_emit_equivalent_text_delta_for_same_payload() {
    let pi_line =
        r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","text":"hello"}}"#;
    let claude_line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hello"}]}}"#;

    let pi_events = parse_pi(pi_line).events;
    let claude_events = parse_claude(claude_line).events;
    assert_eq!(pi_events, claude_events, "TextDelta normalization drift");
    assert_eq!(pi_events.len(), 1);
    assert!(matches!(
        pi_events[0],
        ParsedAgentEvent::TextDelta { ref text } if text == "hello"
    ));
}

/// Pi's `tool_execution_start` and claude's assistant `tool_use` block
/// both lower to `ParsedAgentEvent::ToolCall { id, tool, params,
/// parent_tool_call_id: None }` when no `Task` subagent is open.
/// Comparing equality across backends pins the entire shape, including
/// the JSON-equal `params` value and the absent parent id.
#[test]
fn both_backends_emit_equivalent_tool_call_for_same_payload() {
    let pi_line = r#"{"type":"tool_execution_start","toolCallId":"tc-1","toolName":"Read","args":{"path":"/tmp/x"}}"#;
    let claude_line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"tc-1","name":"Read","input":{"path":"/tmp/x"}}]}}"#;

    let pi_events = parse_pi(pi_line).events;
    let claude_events = parse_claude(claude_line).events;
    assert_eq!(
        pi_events, claude_events,
        "ToolCall normalization drift between backends",
    );
    assert_eq!(pi_events.len(), 1);
    match &pi_events[0] {
        ParsedAgentEvent::ToolCall {
            id,
            tool,
            params,
            parent_tool_call_id,
        } => {
            assert_eq!(id.as_str(), "tc-1");
            assert_eq!(tool, "Read");
            assert_eq!(params["path"], "/tmp/x");
            assert!(parent_tool_call_id.is_none());
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
}

/// Pi's `tool_execution_end` and claude's user `tool_result` block both
/// lower to `ParsedAgentEvent::ToolResult { id, output, is_error }`
/// with byte-equal `output` for a plain-string payload. The wire field
/// names diverge (`toolCallId`/`result`/`isError` vs.
/// `tool_use_id`/`content`/`is_error`) but the normalized event shape
/// is identical.
#[test]
fn both_backends_emit_equivalent_tool_result_for_same_payload() {
    let pi_line =
        r#"{"type":"tool_execution_end","toolCallId":"tc-1","result":"ok","isError":false}"#;
    let claude_line = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tc-1","content":"ok","is_error":false}]}}"#;

    let pi_events = parse_pi(pi_line).events;
    let claude_events = parse_claude(claude_line).events;
    assert_eq!(
        pi_events, claude_events,
        "ToolResult normalization drift between backends",
    );
    assert_eq!(pi_events.len(), 1);
    match &pi_events[0] {
        ParsedAgentEvent::ToolResult {
            id,
            output,
            is_error,
        } => {
            assert_eq!(id.as_str(), "tc-1");
            assert_eq!(output, "ok");
            assert!(!is_error);
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

/// Pi's `message_update.error` delta and claude's `result/error` both
/// produce a `ParsedAgentEvent::Error { message }` carrying the same
/// human-readable error text. Pi keeps the session alive after this
/// event (compaction may recover); claude pairs it with the terminal
/// `SessionComplete`. Comparing only the leading `Error` pins the
/// shared shape without conflating the terminal contract.
#[test]
fn both_backends_emit_equivalent_error_event_for_same_message() {
    let pi_line = r#"{"type":"message_update","assistantMessageEvent":{"type":"error","reason":"x","message":"upstream timed out"}}"#;
    let claude_line = r#"{"type":"result","subtype":"error","result":"upstream timed out"}"#;

    let pi_events = parse_pi(pi_line).events;
    let claude_events = parse_claude(claude_line).events;
    let pi_error = &pi_events[0];
    let claude_error = &claude_events[0];
    assert_eq!(
        pi_error, claude_error,
        "Error event normalization drift between backends",
    );
    match pi_error {
        ParsedAgentEvent::Error { message } => assert_eq!(message, "upstream timed out"),
        other => panic!("expected Error, got {other:?}"),
    }
}

/// Pi's `agent_end` and the trailing event in claude's `result/success`
/// both close the session with a `SessionComplete { exit_code: 0 }`.
/// The cost field is the documented divergence (pi: `None`; claude:
/// `Some(total_cost_usd)`); to compare apples-to-apples, build a
/// claude fixture without `total_cost_usd` so both backends report
/// `cost_usd: None`.
#[test]
fn both_backends_emit_equivalent_session_complete_when_cost_absent() {
    let pi_line = r#"{"type":"agent_end","messages":[]}"#;
    let claude_line = r#"{"type":"result","subtype":"success"}"#;

    let pi_events = parse_pi(pi_line).events;
    let claude_events = parse_claude(claude_line).events;
    let pi_complete = pi_events.last().expect("pi yields SessionComplete");
    let claude_complete = claude_events
        .last()
        .expect("claude yields SessionComplete as the trailing event");
    assert_eq!(
        pi_complete, claude_complete,
        "SessionComplete drift when cost_usd is absent in both",
    );
    match pi_complete {
        ParsedAgentEvent::SessionComplete {
            exit_code,
            cost_usd,
        } => {
            assert_eq!(*exit_code, 0);
            assert!(cost_usd.is_none());
        }
        other => panic!("expected SessionComplete, got {other:?}"),
    }
}

// ----------------------------------------------------------------------------
// Stall watchdog: drives Clock + timeout machinery deterministically
// ----------------------------------------------------------------------------

/// Thread-local writer for `tracing` events; mirrors the pattern in
/// `loom-driver/tests/logging.rs` so the global subscriber installs once
/// and each test routes its output through its own buffer.
mod log_capture {
    use std::cell::RefCell;
    use std::io;
    use std::sync::{Arc, Mutex, OnceLock};

    use tracing_subscriber::fmt::MakeWriter;

    pub type Buffer = Arc<Mutex<Vec<u8>>>;

    thread_local! {
        static SLOT: RefCell<Option<Buffer>> = const { RefCell::new(None) };
    }

    pub struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            SLOT.with(|s| *s.borrow_mut() = None);
        }
    }

    pub fn install(buf: Buffer) -> Guard {
        SLOT.with(|s| *s.borrow_mut() = Some(buf));
        Guard
    }

    pub struct ThreadWriter;
    impl io::Write for ThreadWriter {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            SLOT.with(|s| {
                if let Some(buf) = s.borrow().as_ref() {
                    buf.lock()
                        .map_err(|_| io::Error::other("poisoned"))?
                        .extend_from_slice(b);
                }
                Ok(b.len())
            })
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    pub struct Maker;
    impl<'a> MakeWriter<'a> for Maker {
        type Writer = ThreadWriter;
        fn make_writer(&'a self) -> Self::Writer {
            ThreadWriter
        }
    }

    static INIT: OnceLock<()> = OnceLock::new();

    pub fn init_global_subscriber() {
        INIT.get_or_init(|| {
            let subscriber = tracing_subscriber::fmt()
                .with_writer(Maker)
                .with_max_level(tracing::Level::WARN)
                .with_ansi(false)
                .finish();
            let _ = tracing::subscriber::set_global_default(subscriber);
        });
    }
}

/// Mirror of `loom-workflow::agent::next_event_with_stall_warn`: poll a
/// future to completion while emitting a `warn!` every `stall_window`
/// of silence. The warning does not abort. Returning the polled value
/// (or `None` for EOF) lets callers assert that the inner future was
/// not short-circuited by the stall watchdog.
async fn next_with_stall_warn<F, T>(future: F, stall_window: Duration, clock: &dyn Clock) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::pin!(future);
    loop {
        let sleep = clock.sleep(stall_window);
        tokio::select! {
            biased;
            out = &mut future => return out,
            () = sleep => warn!(
                stall_secs = stall_window.as_secs(),
                "no agent event for stall window — still waiting",
            ),
        }
    }
}

/// 5-minute stall window with a pending JSONL future: the warn-and-
/// continue loop must fire the warning multiple times (the future
/// never completes) and never abort. The test bounds itself by
/// counting iterations and breaking out after the warning has fired
/// twice, which is enough to demonstrate that:
///   1. `Clock::sleep(5min)` resolves under paused time
///   2. The warning fires per loop iteration
///   3. The outer future is not dropped or aborted
#[tokio::test(start_paused = true)]
async fn stall_window_fires_warning_after_five_minutes_without_aborting() {
    log_capture::init_global_subscriber();
    let buf: log_capture::Buffer = Arc::new(Mutex::new(Vec::new()));
    let _guard = log_capture::install(buf.clone());

    let stall_window = Duration::from_secs(5 * 60);
    let clock = MockClock::new();

    // Run the warn-and-continue loop with a deadline at 11 minutes so
    // the test fails closed if the loop accidentally aborts. The inner
    // future is `pending::<()>()`, which never completes; the deadline
    // breaks the test out via timeout, not via abort.
    let body = next_with_stall_warn(pending::<()>(), stall_window, &clock);
    let result = clock.timeout(Duration::from_secs(11 * 60), body).await;
    assert!(
        result.is_err(),
        "stall watchdog must keep polling — a returned Ok means the inner future short-circuited",
    );

    let captured = String::from_utf8(buf.lock().unwrap().clone())
        .expect("captured tracing output should be UTF-8");
    let warn_hits = captured.matches("no agent event for stall window").count();
    assert!(
        warn_hits >= 2,
        "5-minute stall window with no input over 11 minutes must fire at least twice; \
         got {warn_hits} hits. captured=\n{captured}",
    );
    assert!(
        captured.contains("WARN"),
        "tracing output must include the WARN level for the stall warning. captured=\n{captured}",
    );
}

/// When the inner future eventually completes — even after the stall
/// window has fired — the warn-and-continue loop yields the value
/// rather than treating the warning as an abort signal. Drives the
/// "warning is observability only, not control flow" contract.
#[tokio::test(start_paused = true)]
async fn stall_warning_does_not_abort_when_event_eventually_arrives() {
    log_capture::init_global_subscriber();
    let buf: log_capture::Buffer = Arc::new(Mutex::new(Vec::new()));
    let _guard = log_capture::install(buf.clone());

    let stall_window = Duration::from_secs(5 * 60);
    let clock = MockClock::new();

    let body = async {
        // Outlast one full stall window, then yield a real value.
        tokio::time::sleep(Duration::from_secs(6 * 60)).await;
        "session_complete"
    };
    let value = next_with_stall_warn(body, stall_window, &clock).await;
    assert_eq!(
        value, "session_complete",
        "loop must yield the awaited value once the inner future completes",
    );

    let captured = String::from_utf8(buf.lock().unwrap().clone())
        .expect("captured tracing output should be UTF-8");
    assert!(
        captured.contains("no agent event for stall window"),
        "the 6-minute silence must trip the 5-minute stall warning. captured=\n{captured}",
    );
}

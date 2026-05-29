//! Backend-agnostic session driver.
//!
//! [`run_agent`] is the single function the workflow modules call to drive a
//! [`SpawnConfig`] through any [`AgentBackend`]. The binary crate
//! monomorphizes one copy per concrete backend (`run_agent::<PiBackend>` and
//! `run_agent::<ClaudeBackend>`) inside a `dispatch` match and hands the
//! resulting closure into the workflow modules — keeping
//! [`run_agent`]'s `<B: AgentBackend>` parameter the only place the workflow
//! is generic over the backend.
//!
//! Every [`AgentEvent`] consumed from the typestate session is tee'd into an
//! optional [`LogSink`] (when one is supplied) so the spec contract — "the
//! terminal renderer consumes the same `AgentEvent` stream that's written to
//! disk" — is enforced through a single emission point. Sink lifecycle
//! ownership lives here: passing `Some(sink)` consumes it, and `run_agent`
//! calls [`LogSink::finish`] before returning so callers can rely on the
//! file being closed and flushed regardless of the exit path.

use std::time::Duration;

use loom_driver::agent::{
    Active, AgentBackend, AgentEvent, AgentSession, DEFAULT_STALL_WARN_SECS, Idle, ProtocolError,
    SessionOutcome, SpawnConfig,
};
use loom_driver::clock::{Clock, SystemClock};
use loom_driver::logging::{BeadOutcome, LogSink};
use loom_events::{
    DriverKind, EnvelopeBuilder, EventSink, ParsedAgentEvent, SessionCommand, Source,
};
use tracing::{info, warn};

use crate::r#loop::SessionResult;
use crate::observer::DefaultObserverChain;

/// Drive `B` through one full session: spawn, prompt, then consume events
/// until `SessionComplete` arrives. Returns the resulting [`SessionOutcome`]
/// (exit code + cost, when surfaced by the backend).
///
/// When `sink` is `Some`, every observed event is emitted into the sink in
/// arrival order — including the terminal `SessionComplete` — and the sink
/// is finished with the appropriate [`BeadOutcome`] before this function
/// returns. Sink-write failures map to [`ProtocolError::Io`] so the caller
/// surfaces a single error type regardless of which subsystem failed.
///
/// `UnexpectedEof` is returned if the agent process closes its stdout
/// without emitting a terminal event — this signals the caller that the
/// session ended abnormally and the outcome is not trustworthy.
pub async fn run_agent<B: AgentBackend>(
    config: &SpawnConfig,
    sink: Option<LogSink>,
    text_capture: Option<&mut String>,
) -> Result<SessionOutcome, ProtocolError> {
    match run_agent_classified::<B>(config, sink, None, text_capture, None).await {
        SessionResult::Complete(outcome) => Ok(outcome),
        // Callers that only accept the legacy `Result` shape (todo, plan,
        // msg, batch dispatch) treat both infra phases as a single failure
        // surface. The run-loop dispatch path in `main.rs` calls
        // `run_agent_classified` directly so it can preserve the
        // preflight/mid-session distinction the verdict gate relies on.
        SessionResult::PreflightFailed { error } | SessionResult::MidSessionFailed { error } => {
            Err(ProtocolError::Io(std::io::Error::other(error)))
        }
        SessionResult::ObserverAbort { reason } => Err(ProtocolError::Io(std::io::Error::other(
            format!("Session aborted by observer: {reason}"),
        ))),
    }
}

/// Same as [`run_agent`] but preserves the preflight vs mid-session
/// distinction in its return type. Used by the `loom loop` driver so the
/// verdict gate can route pre-flight failures to `loom:blocked` cause
/// `infra-preflight` immediately and grant mid-session failures one
/// driver-memory retry per `loom loop`.
///
/// `observer` is an optional [`DefaultObserverChain`] the driver fans
/// every event into alongside `sink`. After every non-streaming event
/// the driver calls `observer.react()` (then `sink.react()`) and applies
/// the returned [`SessionCommand`]s to the live session: `Steer` injects
/// a system message into the next turn; `Abort` terminates the session
/// and the function returns [`SessionResult::ObserverAbort`]. `Abort`
/// is terminal — subsequent commands in the same batch are ignored. The
/// driver also drains the chain's pending observability payloads
/// (`take_pending_driver_events`) and writes each one through the same
/// `sink` + `envelope_builder` as the surrounding agent events, so
/// `DriverKind::DoomLoopTripped` / `DriverKind::DuplicateToolResult`
/// land in the log alongside the events that caused them.
///
/// `envelope_builder` joins each `ParsedAgentEvent` the session yields
/// with the next per-spawn envelope (monotonic `seq`, real `bead_id`,
/// real wall-clock `ts_ms`) via `AgentEvent::from_parsed`. The session
/// layer is the sole constructor of `AgentEvent`; parsers cannot reach
/// a stamped event by any other path. When `None`, the loop falls back
/// to `phase_envelope_builder` so phase spawns (todo / plan / msg)
/// without a bead context still produce fully-valid envelopes (bead id
/// `lm-phase`).
pub async fn run_agent_classified<B: AgentBackend>(
    config: &SpawnConfig,
    mut sink: Option<LogSink>,
    mut observer: Option<&mut DefaultObserverChain>,
    mut text_capture: Option<&mut String>,
    mut envelope_builder: Option<loom_events::EnvelopeBuilder>,
) -> SessionResult {
    let stall_window = config
        .stall_warn_interval
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_STALL_WARN_SECS));
    let clock = SystemClock::new();
    let session = match B::spawn(config).await {
        Ok(session) => {
            // Container/agent process is up. Emit a `container_spawn`
            // driver_event so replay and live UX can announce the
            // boundary between pre-flight setup and the agent's own
            // event stream.
            emit_driver_event(
                sink.as_mut(),
                envelope_builder.as_mut(),
                DriverKind::ContainerSpawn,
                &format!(
                    "container spawn ok: {image} for {workspace}",
                    image = config.image_ref,
                    workspace = config.workspace.display(),
                ),
                serde_json::json!({
                    "image_ref": config.image_ref,
                    "workspace": config.workspace.to_string_lossy(),
                }),
            );
            session
        }
        Err(err) => {
            // Spec: "Pre-flight failures (image load, container start) →
            // exit immediately as blocked with cause infra-preflight —
            // there is no agent output to evaluate." `B::spawn` is the
            // boundary that owns image load + container construction; any
            // failure here lands in the preflight bucket regardless of
            // backend.
            warn!(error = %err, "agent spawn failed before session became live");
            let error_str = err.to_string();
            emit_driver_event(
                sink.as_mut(),
                envelope_builder.as_mut(),
                DriverKind::InfraFailure,
                &format!("preflight infra failure: {error_str}"),
                serde_json::json!({
                    "phase": "preflight",
                    "error": error_str,
                }),
            );
            finish_sink(sink, BeadOutcome::Failed);
            return SessionResult::PreflightFailed { error: error_str };
        }
    };
    info!(
        prompt_chars = config.initial_prompt.chars().count(),
        stall_warn_secs = stall_window.as_secs(),
        "agent spawned; sending initial prompt",
    );
    let mut session =
        match prompt_with_stall_warn(session, &config.initial_prompt, stall_window, &clock).await {
            Ok(s) => s,
            Err(err) => {
                let error_str = err.to_string();
                emit_midsession_failure_event(sink.as_mut(), envelope_builder.as_mut(), &error_str);
                finish_sink(sink, BeadOutcome::Failed);
                return SessionResult::MidSessionFailed { error: error_str };
            }
        };
    info!("prompt sent; awaiting agent events");
    loop {
        let next = next_event_with_stall_warn(&mut session, stall_window, &clock).await;
        let parsed = match next {
            Ok(Some(event)) => event,
            Ok(None) => {
                let error_str = ProtocolError::UnexpectedEof.to_string();
                emit_midsession_failure_event(sink.as_mut(), envelope_builder.as_mut(), &error_str);
                finish_sink(sink, BeadOutcome::Failed);
                return SessionResult::MidSessionFailed { error: error_str };
            }
            Err(err) => {
                let error_str = err.to_string();
                emit_midsession_failure_event(sink.as_mut(), envelope_builder.as_mut(), &error_str);
                finish_sink(sink, BeadOutcome::Failed);
                return SessionResult::MidSessionFailed { error: error_str };
            }
        };
        // RS-12: the session yields the parser's payload only; the
        // workflow layer joins it with the per-spawn envelope to
        // produce the consumer-visible `AgentEvent`.
        let envelope = match envelope_builder.as_mut() {
            Some(b) => b.build(),
            None => match phase_envelope_builder() {
                Ok(b) => envelope_builder.insert(b).build(),
                Err(err) => {
                    let error_str = format!("phase envelope builder construction failed: {err}");
                    emit_midsession_failure_event(
                        sink.as_mut(),
                        envelope_builder.as_mut(),
                        &error_str,
                    );
                    finish_sink(sink, BeadOutcome::Failed);
                    return SessionResult::MidSessionFailed { error: error_str };
                }
            },
        };
        let event = AgentEvent::from_parsed(parsed, envelope);
        info!(event = %summarize_event(&event), "agent event");
        if let AgentEvent::TextDelta { text, .. } = &event
            && let Some(buf) = text_capture.as_deref_mut()
        {
            buf.push_str(text);
        }
        if let Some(s) = sink.as_mut()
            && let Err(e) = s.emit(&event)
        {
            warn!(error = %e, "log sink emit failed");
            finish_sink(sink, BeadOutcome::Failed);
            return SessionResult::MidSessionFailed {
                error: format!("log sink emit failed: {e}"),
            };
        }
        if let Some(o) = observer.as_deref_mut() {
            o.emit(&event);
        }
        if is_non_streaming(&event) {
            if let Some(o) = observer.as_deref_mut() {
                let pending = o.take_pending_driver_events();
                for entry in pending {
                    emit_driver_event(
                        sink.as_mut(),
                        envelope_builder.as_mut(),
                        entry.kind,
                        &entry.summary,
                        entry.payload,
                    );
                }
            }
            let mut commands: Vec<SessionCommand> = Vec::new();
            if let Some(s) = sink.as_mut() {
                commands.extend(EventSink::react(s));
            }
            if let Some(o) = observer.as_deref_mut() {
                commands.extend(o.react());
            }
            match classify_react_commands(commands) {
                ReactAction::Continue { steers } => {
                    for msg in steers {
                        if let Err(e) = session.steer(&msg).await {
                            warn!(error = %e, "session steer failed");
                            finish_sink(sink, BeadOutcome::Failed);
                            return SessionResult::MidSessionFailed {
                                error: format!("session steer failed: {e}"),
                            };
                        }
                    }
                }
                ReactAction::Abort { reason } => {
                    info!(
                        reason = %reason,
                        "observer requested session abort via react()",
                    );
                    if let Err(e) = session.abort().await {
                        warn!(
                            error = %e,
                            "session abort failed during observer-driven cancel; \
                             kill_on_drop will reap the child",
                        );
                    }
                    finish_sink(sink, BeadOutcome::Failed);
                    return SessionResult::ObserverAbort { reason };
                }
            }
        }
        if matches!(event, AgentEvent::CompactionStart { .. })
            && let Err(e) = B::on_compaction_start(&mut session, config).await
        {
            warn!(error = %e, "backend compaction handler failed");
            finish_sink(sink, BeadOutcome::Failed);
            return SessionResult::MidSessionFailed {
                error: e.to_string(),
            };
        }
        if let AgentEvent::SessionComplete {
            exit_code,
            cost_usd,
            ..
        } = event
        {
            let outcome = if exit_code == 0 {
                BeadOutcome::Done
            } else {
                BeadOutcome::Failed
            };
            if let Err(e) = B::after_session_complete(session, config).await {
                warn!(error = %e, "backend shutdown hook failed");
            }
            finish_sink(sink, outcome);
            return SessionResult::Complete(SessionOutcome {
                exit_code,
                cost_usd,
            });
        }
    }
}

/// Drive [`AgentSession::prompt`] to completion while emitting a periodic
/// `warn!` every `stall_window` that the write hasn't returned. Closes the
/// visibility gap between `B::spawn` returning and the first agent event:
/// for the claude backend that window is the container starting up and
/// claude opening stdin, and a slow consumer can leave the pipe write
/// blocked with no log output. `stall_window == Duration::ZERO` disables
/// the watchdog (used by tests).
async fn prompt_with_stall_warn(
    session: AgentSession<Idle>,
    msg: &str,
    stall_window: Duration,
    clock: &dyn Clock,
) -> Result<AgentSession<Active>, ProtocolError> {
    let fut = session.prompt(msg);
    if stall_window.is_zero() {
        return fut.await;
    }
    tokio::pin!(fut);
    loop {
        let sleep = clock.sleep(stall_window);
        tokio::select! {
            biased;
            result = &mut fut => return result,
            () = sleep => warn!(
                stall_secs = stall_window.as_secs(),
                "still writing initial prompt to agent — agent stdin not draining yet",
            ),
        }
    }
}

/// Poll [`AgentSession::next_event`] while emitting a periodic `warn!`
/// every `stall_window` of silence. The warning does not abort the run —
/// claude can legitimately think for minutes — but it ends the silent
/// stare at the terminal so the operator can decide whether to intervene.
///
/// `stall_window == Duration::ZERO` disables the watchdog explicitly. A
/// fresh `clock.sleep(stall_window)` is created on every loop iteration so
/// each warning resets the silence window.
async fn next_event_with_stall_warn(
    session: &mut AgentSession<Active>,
    stall_window: Duration,
    clock: &dyn Clock,
) -> Result<Option<ParsedAgentEvent>, ProtocolError> {
    let next = session.next_event();
    if stall_window.is_zero() {
        return next.await;
    }
    tokio::pin!(next);
    loop {
        let sleep = clock.sleep(stall_window);
        tokio::select! {
            biased;
            result = &mut next => return result,
            () = sleep => warn!(
                stall_secs = stall_window.as_secs(),
                "no agent event for stall window — still waiting",
            ),
        }
    }
}

/// Fallback `EnvelopeBuilder` for phase-level spawns (todo/check/msg)
/// that do not own a per-bead context yet. Stamps events with the
/// synthetic but fully-valid `lm-phase` bead id; replay tools that key
/// on `bead_id` see it as a distinct stream rather than an invalid
/// sentinel. The `ts_ms` closure samples the wall clock so events stay
/// monotonic. Returns the parser error if BeadId rules drift to reject
/// `lm-phase`; the [`phase_bead_id_parses`] test catches that drift
/// before it can reach a live session.
fn phase_envelope_builder() -> Result<EnvelopeBuilder, loom_events::identifier::ParseBeadIdError> {
    let bead = loom_events::identifier::BeadId::new("lm-phase")?;
    let clock = SystemClock::new();
    Ok(EnvelopeBuilder::new(
        bead,
        None,
        0,
        Source::Agent,
        move || {
            clock
                .wall_now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64
        },
    ))
}

fn finish_sink(sink: Option<LogSink>, outcome: BeadOutcome) {
    if let Some(mut s) = sink
        && let Err(e) = s.finish(outcome)
    {
        warn!(error = %e, "log sink finish failed");
    }
}

/// Emit a single `driver_event` into `sink` carrying `source: driver`.
///
/// Pulled out so the three failure-handling sites in `run_agent_classified`
/// (preflight, prompt failure, event-loop failure) share one code path
/// and write through the same envelope-builder seq counter as the agent
/// events that surround them. Silent no-op when either the sink or the
/// envelope builder is absent — tests and the legacy `run_agent` wrapper
/// pass `None` and must not be required to wire driver events.
fn emit_driver_event(
    sink: Option<&mut LogSink>,
    builder: Option<&mut EnvelopeBuilder>,
    kind: DriverKind,
    summary: &str,
    payload: serde_json::Value,
) {
    let (Some(sink), Some(builder)) = (sink, builder) else {
        return;
    };
    let envelope = builder.build_with_source(Source::Driver);
    let wire = kind.as_wire().to_string();
    let event = AgentEvent::DriverEvent {
        envelope,
        driver_kind: kind,
        summary: summary.to_string(),
        payload,
    };
    if let Err(e) = sink.emit(&event) {
        warn!(error = %e, kind = %wire, "driver event emit failed");
    }
}

/// Classify a mid-session failure string into a [`DriverKind`]. OOM signals
/// (exit 137 from the container's SIGKILL, "Killed" / "OOM" in the
/// rendered error) surface as `ContainerOom`; everything else lands in
/// the generic `InfraFailure` bucket per the verdict-gate spec's
/// container-lifecycle row.
fn midsession_failure_kind(error: &str) -> DriverKind {
    if is_oom_error(error) {
        DriverKind::ContainerOom
    } else {
        DriverKind::InfraFailure
    }
}

fn is_oom_error(error: &str) -> bool {
    // The Display impl of `ProtocolError::ProcessExit(137)` renders as
    // "agent process exited with code 137"; podman / kernel OOM logs use
    // "Killed" / "OOM". Case-insensitive match keeps backend phrasing
    // tolerant.
    let lower = error.to_ascii_lowercase();
    error.contains("code 137")
        || lower.contains("killed")
        || lower.contains("oom")
        || lower.contains("out of memory")
}

/// Action the event loop should take after collecting `react()` commands
/// from every sink in the chain. `Steer` commands are batched in
/// registration order; the first `Abort` short-circuits the batch and
/// becomes terminal — per `specs/harness.md` §"EventSink and
/// SessionCommand · react() priority".
#[derive(Debug, Clone, PartialEq, Eq)]
enum ReactAction {
    Continue { steers: Vec<String> },
    Abort { reason: String },
}

/// Pure classifier for the [`SessionCommand`] batch returned from
/// `react()`. Pulled out of the event loop so the priority rule (Abort is
/// terminal; subsequent commands in the same batch are dropped) can be
/// tested without driving a real session.
fn classify_react_commands(commands: Vec<SessionCommand>) -> ReactAction {
    let mut steers = Vec::new();
    for cmd in commands {
        match cmd {
            SessionCommand::Steer(msg) => steers.push(msg),
            SessionCommand::Abort(reason) => return ReactAction::Abort { reason },
        }
    }
    ReactAction::Continue { steers }
}

/// Streaming events (`text_delta`, `thinking_delta`, `toolcall_delta`) do
/// not trigger `react()`; observer state does not change on text bytes
/// and polling them every fragment would be pure overhead. Spec contract
/// (`specs/harness.md` §"EventSink and SessionCommand").
fn is_non_streaming(event: &AgentEvent) -> bool {
    !matches!(
        event,
        AgentEvent::TextDelta { .. }
            | AgentEvent::ThinkingDelta { .. }
            | AgentEvent::ToolcallDelta { .. }
    )
}

fn emit_midsession_failure_event(
    sink: Option<&mut LogSink>,
    builder: Option<&mut EnvelopeBuilder>,
    error: &str,
) {
    let kind = midsession_failure_kind(error);
    let summary = format!("mid-session {kind}: {error}");
    emit_driver_event(
        sink,
        builder,
        kind,
        &summary,
        serde_json::json!({
            "phase": "midsession",
            "error": error,
        }),
    );
}

fn summarize_event(event: &AgentEvent) -> String {
    match event {
        AgentEvent::AgentStart { title, profile, .. } => {
            format!("agent_start ({title}, profile={profile})")
        }
        AgentEvent::TextDelta { text, .. } => {
            format!("message_delta ({} chars)", text.chars().count())
        }
        AgentEvent::ToolCall { id, tool, .. } => format!("tool_call {tool} (id={id})"),
        AgentEvent::ToolResult {
            id,
            output,
            is_error,
            ..
        } => format!(
            "tool_result (id={id}, is_error={is_error}, {} chars)",
            output.chars().count(),
        ),
        AgentEvent::TurnEnd { .. } => "turn_end".to_string(),
        AgentEvent::SessionComplete {
            exit_code,
            cost_usd,
            ..
        } => format!("session_complete (exit_code={exit_code}, cost_usd={cost_usd:?})",),
        AgentEvent::CompactionStart { reason, .. } => format!("compaction_start ({reason:?})"),
        AgentEvent::CompactionEnd { aborted, .. } => {
            format!("compaction_end (aborted={aborted})")
        }
        AgentEvent::Error { message, .. } => format!("error: {message}"),
        AgentEvent::AgentEnd { .. } => "agent_end".to_string(),
        AgentEvent::TurnStart { .. } => "turn_start".to_string(),
        AgentEvent::TextEnd { .. } => "text_end".to_string(),
        AgentEvent::ThinkingDelta { text, .. } => {
            format!("thinking_delta ({} chars)", text.chars().count())
        }
        AgentEvent::ThinkingEnd { .. } => "thinking_end".to_string(),
        AgentEvent::ToolcallDelta { id, delta, .. } => {
            format!("toolcall_delta (id={id}, {} chars)", delta.chars().count())
        }
        AgentEvent::ToolProgress { id, text, .. } => {
            format!("tool_progress (id={id}, {} chars)", text.chars().count())
        }
        AgentEvent::AutoRetry {
            attempt,
            max_attempts,
            ..
        } => format!("auto_retry (attempt={attempt}/{max_attempts})"),
        AgentEvent::DriverEvent {
            driver_kind,
            summary,
            ..
        } => format!(
            "driver_event {kind}: {summary}",
            kind = driver_kind.as_wire()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::agent::RePinContent;
    use loom_driver::logging::LogSink;
    use loom_events::identifier::{BeadId, SpecLabel};
    use std::path::PathBuf;
    use std::time::SystemTime;

    #[test]
    fn phase_bead_id_parses() {
        loom_events::identifier::BeadId::new("lm-phase")
            .expect("`lm-phase` must parse as a BeadId — phase_envelope_builder depends on it");
    }

    #[test]
    fn is_oom_error_matches_exit_137_and_killed_phrasings() {
        assert!(is_oom_error("agent process exited with code 137"));
        assert!(is_oom_error("io failure on agent stdio: process Killed"));
        assert!(is_oom_error("OOM killer claimed agent"));
        assert!(is_oom_error("out of memory"));
        assert!(!is_oom_error("agent process exited with code 1"));
        assert!(!is_oom_error("io timeout"));
        assert!(!is_oom_error("unexpected end of agent event stream"));
    }

    #[test]
    fn midsession_failure_kind_routes_oom_versus_generic_infra() {
        assert_eq!(
            midsession_failure_kind("agent process exited with code 137"),
            DriverKind::ContainerOom,
        );
        assert_eq!(
            midsession_failure_kind("unexpected end of agent event stream"),
            DriverKind::InfraFailure,
        );
    }

    fn builder() -> EnvelopeBuilder {
        let bead = BeadId::new("lm-emit").expect("bead id");
        let mut clock = 0_i64;
        EnvelopeBuilder::new(bead, None, 0, Source::Agent, move || {
            clock += 1;
            clock
        })
    }

    fn read_jsonl(path: &std::path::Path) -> Vec<serde_json::Value> {
        let body = std::fs::read_to_string(path).expect("read log");
        body.lines()
            .map(|l| serde_json::from_str(l).expect("json line"))
            .collect()
    }

    fn open_test_sink(dir: &std::path::Path) -> (LogSink, std::path::PathBuf) {
        let label = SpecLabel::new("emit-test");
        let bead = BeadId::new("lm-emit").expect("bead id");
        let sink = LogSink::open_in_at(dir, &label, &bead, None, SystemTime::UNIX_EPOCH)
            .expect("open sink");
        let path = sink.log_path().to_path_buf();
        (sink, path)
    }

    #[test]
    fn emit_driver_event_writes_one_jsonl_line_with_source_driver() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut sink, path) = open_test_sink(dir.path());
        let mut b = builder();
        emit_driver_event(
            Some(&mut sink),
            Some(&mut b),
            DriverKind::ContainerSpawn,
            "container spawn ok: img:tag",
            serde_json::json!({"image_ref": "img:tag"}),
        );
        sink.finish(BeadOutcome::Done).expect("finish");
        let events = read_jsonl(&path);
        assert_eq!(events.len(), 1, "exactly one driver event emitted");
        assert_eq!(events[0]["kind"], "driver_event");
        assert_eq!(events[0]["driver_kind"], "container_spawn");
        assert_eq!(events[0]["source"], "driver");
        assert_eq!(events[0]["payload"]["image_ref"], "img:tag");
    }

    #[test]
    fn emit_driver_event_is_silent_noop_when_sink_or_builder_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut sink, path) = open_test_sink(dir.path());
        let mut b = builder();
        emit_driver_event(
            None,
            Some(&mut b),
            DriverKind::ContainerSpawn,
            "no sink",
            serde_json::json!({}),
        );
        emit_driver_event(
            Some(&mut sink),
            None,
            DriverKind::ContainerSpawn,
            "no builder",
            serde_json::json!({}),
        );
        sink.finish(BeadOutcome::Done).expect("finish");
        assert!(
            read_jsonl(&path).is_empty(),
            "no events should land in the sink when either dep is missing",
        );
        // Builder seq must NOT advance when emission is suppressed.
        assert_eq!(
            b.current_seq(),
            0,
            "seq counter must not advance on suppressed emissions",
        );
    }

    fn sample_envelope() -> loom_events::EventEnvelope {
        loom_events::EventEnvelope {
            bead_id: BeadId::new("lm-react").expect("bead id"),
            molecule_id: None,
            iteration: 1,
            source: Source::Agent,
            ts_ms: 0,
            seq: 0,
        }
    }

    fn tool_call_event() -> AgentEvent {
        AgentEvent::ToolCall {
            envelope: sample_envelope(),
            id: loom_events::identifier::ToolCallId::new("tc-1"),
            tool: "bash".to_string(),
            params: serde_json::json!({}),
            parent_tool_call_id: None,
        }
    }

    fn text_delta_event() -> AgentEvent {
        AgentEvent::TextDelta {
            envelope: sample_envelope(),
            text: "hi".into(),
        }
    }

    #[test]
    fn classify_react_commands_collects_steers_in_registration_order() {
        let action = classify_react_commands(vec![
            SessionCommand::Steer("first".into()),
            SessionCommand::Steer("second".into()),
        ]);
        match action {
            ReactAction::Continue { steers } => {
                assert_eq!(steers, vec!["first".to_string(), "second".to_string()]);
            }
            other => panic!("expected Continue, got {other:?}"),
        }
    }

    #[test]
    fn classify_react_commands_empty_batch_is_continue_no_steers() {
        match classify_react_commands(vec![]) {
            ReactAction::Continue { steers } => assert!(steers.is_empty()),
            other => panic!("expected Continue, got {other:?}"),
        }
    }

    /// Spec criterion: "Driver applies `react()` after every non-streaming
    /// event (not after `text_delta` / `thinking_delta` /
    /// `toolcall_delta`)" (`specs/harness.md` Success Criteria §
    /// "EventSink and SessionCommand"). The driver gates its
    /// `react()` poll on [`is_non_streaming`]; this verifies the delta
    /// trio is the only set excluded.
    #[test]
    fn react_invoked_after_non_streaming_events_only() {
        assert!(!is_non_streaming(&text_delta_event()));
        assert!(!is_non_streaming(&AgentEvent::ThinkingDelta {
            envelope: sample_envelope(),
            text: "x".into(),
        }));
        assert!(!is_non_streaming(&AgentEvent::ToolcallDelta {
            envelope: sample_envelope(),
            id: loom_events::identifier::ToolCallId::new("tc-1"),
            delta: "x".into(),
        }));
        assert!(is_non_streaming(&tool_call_event()));
        assert!(is_non_streaming(&AgentEvent::ToolResult {
            envelope: sample_envelope(),
            id: loom_events::identifier::ToolCallId::new("tc-1"),
            output: "ok".into(),
            is_error: false,
        }));
        assert!(is_non_streaming(&AgentEvent::DriverEvent {
            envelope: sample_envelope(),
            driver_kind: DriverKind::ContainerSpawn,
            summary: "spawned".into(),
            payload: serde_json::json!({}),
        }));
        assert!(is_non_streaming(&AgentEvent::TurnEnd {
            envelope: sample_envelope()
        }));
    }

    /// Spec criterion: "Driver treats any `SessionCommand::Abort`
    /// returned from `react()` as terminal: subsequent commands in the
    /// same batch are not applied, session is cancelled, recovery cause
    /// is `observer-abort`" (`specs/harness.md` Success Criteria §
    /// "EventSink and SessionCommand"). Drives a mock observer that
    /// returns `Abort` on the third `tool_call`; verifies (a) `Abort`
    /// short-circuits subsequent `Steer`s in the same batch, and (b)
    /// the cause classifier maps a session aborted by an observer to
    /// `RecoveryCause::ObserverAbort` (label `"observer-abort"`) rather
    /// than `swallowed-marker`.
    #[test]
    fn abort_command_short_circuits_remaining_commands_and_classifies_observer_abort() {
        struct CountingAbortObserver {
            tool_calls: u32,
            abort_at: u32,
            abort_reason: String,
        }
        impl EventSink for CountingAbortObserver {
            fn emit(&mut self, event: &AgentEvent) {
                if matches!(event, AgentEvent::ToolCall { .. }) {
                    self.tool_calls += 1;
                }
            }
            fn react(&mut self) -> Vec<SessionCommand> {
                if self.tool_calls >= self.abort_at {
                    vec![
                        SessionCommand::Abort(self.abort_reason.clone()),
                        // Subsequent commands in the same batch MUST be
                        // dropped per the spec's react() priority rule.
                        SessionCommand::Steer("post-abort-steer".into()),
                    ]
                } else {
                    Vec::new()
                }
            }
        }

        let mut observer = CountingAbortObserver {
            tool_calls: 0,
            abort_at: 3,
            abort_reason: "doom-loop: 3 identical tool calls".into(),
        };

        for _ in 0..2 {
            observer.emit(&tool_call_event());
            assert!(observer.react().is_empty(), "no abort before threshold");
        }
        observer.emit(&tool_call_event());
        let commands = observer.react();
        assert_eq!(
            commands.len(),
            2,
            "observer emits Abort + a trailing Steer in the same batch",
        );

        match classify_react_commands(commands) {
            ReactAction::Abort { reason } => {
                assert_eq!(
                    reason, "doom-loop: 3 identical tool calls",
                    "Abort's reason must round-trip verbatim",
                );
            }
            other => panic!(
                "Abort must short-circuit the batch; got {other:?} — the trailing Steer leaked through",
            ),
        }

        // The recovery cause label is the spec's `observer-abort`
        // identifier, not `swallowed-marker`.
        assert_eq!(
            crate::review::RecoveryCause::ObserverAbort {
                reason: "doom-loop: 3 identical tool calls".into(),
            }
            .as_str(),
            "observer-abort",
        );
    }

    /// `B::spawn` returning `Err` is the preflight failure path. The
    /// driver must emit a `driver_event { kind: infra_failure }` into
    /// the sink BEFORE finishing it, so a replay can show the cause
    /// rather than just the empty log + closing line.
    #[tokio::test]
    async fn preflight_failure_emits_infra_failure_driver_event() {
        struct FailingBackend;
        impl AgentBackend for FailingBackend {
            async fn spawn(_config: &SpawnConfig) -> Result<AgentSession<Idle>, ProtocolError> {
                Err(ProtocolError::Io(std::io::Error::other(
                    "podman load failed: image archive missing",
                )))
            }
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let (sink, path) = open_test_sink(dir.path());
        let b = builder();
        let cfg = SpawnConfig {
            image_ref: "localhost/img:tag".into(),
            image_source: PathBuf::from("/nix/store/none.tar"),
            workspace: PathBuf::from("/workspace"),
            env: vec![],
            extra_mounts: vec![],
            initial_prompt: String::new(),
            agent_args: vec![],
            repin: RePinContent {
                orientation: String::new(),
                pinned_context: String::new(),
                partial_bodies: vec![],
            },
            scratch_dir: dir.path().join("scratch"),
            model: None,
            thinking_level: None,
            shutdown_grace: None,
            handshake_timeout: None,
            stall_warn_interval: Some(Duration::ZERO),
        };
        let result =
            run_agent_classified::<FailingBackend>(&cfg, Some(sink), None, None, Some(b)).await;
        match result {
            crate::r#loop::SessionResult::PreflightFailed { error } => {
                assert!(
                    error.contains("io failure"),
                    "preflight error must carry the ProtocolError display: {error}",
                );
            }
            other => panic!("expected PreflightFailed, got {other:?}"),
        }
        let events = read_jsonl(&path);
        assert_eq!(
            events.len(),
            1,
            "preflight path emits exactly one driver event: {events:?}",
        );
        assert_eq!(events[0]["kind"], "driver_event");
        assert_eq!(events[0]["driver_kind"], "infra_failure");
        assert_eq!(events[0]["source"], "driver");
        assert_eq!(events[0]["payload"]["phase"], "preflight");
        assert!(
            events[0]["payload"]["error"]
                .as_str()
                .is_some_and(|s| s.contains("io failure")),
            "payload error body must carry the ProtocolError display: {:?}",
            events[0]["payload"],
        );
    }
}

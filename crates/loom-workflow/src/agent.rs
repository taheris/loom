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
use tracing::{info, trace, warn};

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
        // inbox, batch dispatch) treat both infra phases as a single failure
        // surface. The run-loop dispatch path in `main.rs` calls
        // `run_agent_classified` directly so it can preserve the
        // pre-stream/interrupted distinction the verdict gate relies on.
        SessionResult::PreflightFailed { error } | SessionResult::MidSessionFailed { error } => {
            Err(ProtocolError::Io(std::io::Error::other(error)))
        }
        SessionResult::ObserverAbort { reason } => Err(ProtocolError::Io(std::io::Error::other(
            format!("Session aborted by observer: {reason}"),
        ))),
    }
}

/// Same as [`run_agent`] but preserves the pre-stream vs interrupted
/// infrastructure split in its return type. Used by the `loom loop` driver so
/// failures before the first canonical agent-sourced event route as
/// `infra-preflight`, while failures after agent output but before
/// `session_complete` route as interrupted infra.
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
/// to `phase_envelope_builder` so phase spawns (todo / plan / inbox)
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
    let mut first_event_seen = false;
    let session = match B::spawn(config).await {
        Ok(session) => {
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
            warn!(error = %err, "agent spawn failed before session became live");
            let error_str = err.to_string();
            emit_spawn_failure_event(sink.as_mut(), envelope_builder.as_mut(), &err, &error_str);
            finish_sink(sink, BeadOutcome::Failed);
            return SessionResult::PreflightFailed { error: error_str };
        }
    };
    info!(
        prompt_chars = config.initial_prompt.chars().count(),
        stall_warn_secs = stall_window.as_secs(),
        "agent spawned; sending initial prompt",
    );
    let mut session = match prompt_with_stall_warn(
        session,
        &config.initial_prompt,
        stall_window,
        &clock,
        &mut sink,
        &mut envelope_builder,
    )
    .await
    {
        Ok(s) => s,
        Err(err) => {
            let error_str = err.to_string();
            emit_protocol_failure_event(
                sink.as_mut(),
                envelope_builder.as_mut(),
                first_event_seen,
                &err,
                &error_str,
            );
            finish_sink(sink, BeadOutcome::Failed);
            return infra_session_result(first_event_seen, error_str);
        }
    };
    info!("prompt sent; awaiting agent events");
    loop {
        let next = next_event_with_stall_warn(
            &mut session,
            stall_window,
            &clock,
            &mut sink,
            &mut envelope_builder,
        )
        .await;
        let parsed = match next {
            Ok(Some(event)) => event,
            Ok(None) => {
                let err = ProtocolError::UnexpectedEof;
                let error_str = err.to_string();
                emit_protocol_failure_event(
                    sink.as_mut(),
                    envelope_builder.as_mut(),
                    first_event_seen,
                    &err,
                    &error_str,
                );
                finish_sink(sink, BeadOutcome::Failed);
                return infra_session_result(first_event_seen, error_str);
            }
            Err(err) => {
                let error_str = err.to_string();
                emit_protocol_failure_event(
                    sink.as_mut(),
                    envelope_builder.as_mut(),
                    first_event_seen,
                    &err,
                    &error_str,
                );
                finish_sink(sink, BeadOutcome::Failed);
                return infra_session_result(first_event_seen, error_str);
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
                    emit_infra_failure_event(
                        sink.as_mut(),
                        envelope_builder.as_mut(),
                        stream_infra_phase(first_event_seen),
                        InfraCause::DriverEnvelope,
                        &error_str,
                        None,
                        None,
                    );
                    finish_sink(sink, BeadOutcome::Failed);
                    return infra_session_result(first_event_seen, error_str);
                }
            },
        };
        let event = AgentEvent::from_parsed(parsed, envelope);
        if event.envelope().source == Source::Agent {
            first_event_seen = true;
        }
        log_agent_event(&event);
        if let AgentEvent::TextDelta { text, .. } = &event
            && let Some(buf) = text_capture.as_deref_mut()
        {
            buf.push_str(text);
        }
        if let Some(s) = sink.as_mut()
            && let Err(e) = s.emit(&event)
        {
            warn!(error = %e, "log sink emit failed");
            let error_str = format!("log sink emit failed: {e}");
            emit_infra_failure_event(
                sink.as_mut(),
                envelope_builder.as_mut(),
                stream_infra_phase(first_event_seen),
                InfraCause::SinkFailure,
                &error_str,
                None,
                None,
            );
            finish_sink(sink, BeadOutcome::Failed);
            return infra_session_result(first_event_seen, error_str);
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
                            let error_str = format!("session steer failed: {e}");
                            emit_protocol_failure_event(
                                sink.as_mut(),
                                envelope_builder.as_mut(),
                                first_event_seen,
                                &e,
                                &error_str,
                            );
                            finish_sink(sink, BeadOutcome::Failed);
                            return infra_session_result(first_event_seen, error_str);
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
            let error_str = e.to_string();
            emit_protocol_failure_event(
                sink.as_mut(),
                envelope_builder.as_mut(),
                first_event_seen,
                &e,
                &error_str,
            );
            finish_sink(sink, BeadOutcome::Failed);
            return infra_session_result(first_event_seen, error_str);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriverSeverity {
    Warning,
}

impl DriverSeverity {
    fn as_wire(self) -> &'static str {
        match self {
            DriverSeverity::Warning => "warning",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StallPhase {
    PromptWrite,
    AwaitEvent,
}

impl StallPhase {
    fn as_wire(self) -> &'static str {
        match self {
            StallPhase::PromptWrite => "prompt_write",
            StallPhase::AwaitEvent => "await_event",
        }
    }

    fn summary(self) -> &'static str {
        match self {
            StallPhase::PromptWrite => {
                "still writing initial prompt to agent — agent stdin not draining yet"
            }
            StallPhase::AwaitEvent => "no agent event for stall window — still waiting",
        }
    }
}

/// Drive [`AgentSession::prompt`] to completion while surfacing one
/// coalesced stall watchdog event when the write remains pending past
/// `stall_window`. Closes the visibility gap between `B::spawn` returning
/// and the first agent event: for the claude backend that window is the
/// container starting up and claude opening stdin, and a slow consumer can
/// leave the pipe write blocked with no log output. `stall_window ==
/// Duration::ZERO` disables the watchdog (used by tests).
async fn prompt_with_stall_warn(
    session: AgentSession<Idle>,
    msg: &str,
    stall_window: Duration,
    clock: &dyn Clock,
    sink: &mut Option<LogSink>,
    envelope_builder: &mut Option<EnvelopeBuilder>,
) -> Result<AgentSession<Active>, ProtocolError> {
    let fut = session.prompt(msg);
    if stall_window.is_zero() {
        return fut.await;
    }
    tokio::pin!(fut);
    let mut stall_event_emitted = false;
    loop {
        let sleep = clock.sleep(stall_window);
        tokio::select! {
            biased;
            result = &mut fut => return result,
            () = sleep => record_stall_watchdog_tick(
                sink,
                envelope_builder,
                StallPhase::PromptWrite,
                stall_window,
                &mut stall_event_emitted,
            ),
        }
    }
}

/// Poll [`AgentSession::next_event`] while surfacing one coalesced stall
/// watchdog event for a silence window. The warning does not abort the
/// run — claude can legitimately think for minutes — but it ends the
/// silent stare at the terminal so the operator can decide whether to
/// intervene.
///
/// `stall_window == Duration::ZERO` disables the watchdog explicitly. A
/// fresh `clock.sleep(stall_window)` is created on every loop iteration so
/// the trace-only diagnostic still tracks repeated watchdog ticks without
/// emitting repeated renderer rows.
async fn next_event_with_stall_warn(
    session: &mut AgentSession<Active>,
    stall_window: Duration,
    clock: &dyn Clock,
    sink: &mut Option<LogSink>,
    envelope_builder: &mut Option<EnvelopeBuilder>,
) -> Result<Option<ParsedAgentEvent>, ProtocolError> {
    let next = session.next_event();
    if stall_window.is_zero() {
        return next.await;
    }
    tokio::pin!(next);
    let mut stall_event_emitted = false;
    loop {
        let sleep = clock.sleep(stall_window);
        tokio::select! {
            biased;
            result = &mut next => return result,
            () = sleep => record_stall_watchdog_tick(
                sink,
                envelope_builder,
                StallPhase::AwaitEvent,
                stall_window,
                &mut stall_event_emitted,
            ),
        }
    }
}

fn record_stall_watchdog_tick(
    sink: &mut Option<LogSink>,
    envelope_builder: &mut Option<EnvelopeBuilder>,
    phase: StallPhase,
    stall_window: Duration,
    stall_event_emitted: &mut bool,
) {
    let stall_secs = stall_window.as_secs();
    if *stall_event_emitted {
        trace!(
            phase = phase.as_wire(),
            stall_secs, "stall watchdog tick coalesced into prior driver event",
        );
        return;
    }
    *stall_event_emitted = true;
    match phase {
        StallPhase::PromptWrite => warn!(
            stall_secs,
            "still writing initial prompt to agent — agent stdin not draining yet",
        ),
        StallPhase::AwaitEvent => warn!(
            stall_secs,
            "no agent event for stall window — still waiting",
        ),
    }
    emit_stall_watchdog_event(sink, envelope_builder, phase, stall_window);
}

fn emit_stall_watchdog_event(
    sink: &mut Option<LogSink>,
    envelope_builder: &mut Option<EnvelopeBuilder>,
    phase: StallPhase,
    stall_window: Duration,
) {
    emit_driver_event(
        sink.as_mut(),
        envelope_builder.as_mut(),
        DriverKind::StallWatchdog,
        phase.summary(),
        serde_json::json!({
            "severity": DriverSeverity::Warning.as_wire(),
            "phase": phase.as_wire(),
            "stall_secs": stall_window.as_secs(),
        }),
    );
}

/// Fallback `EnvelopeBuilder` for phase-level spawns (todo/check/inbox)
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
/// Pulled out so driver-authored events in `run_agent_classified` share one
/// code path and write through the same envelope-builder seq counter as the agent
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InfraClass {
    Preflight,
    Interrupted,
}

impl InfraClass {
    const fn as_wire(self) -> &'static str {
        match self {
            InfraClass::Preflight => "infra-preflight",
            InfraClass::Interrupted => "infra-interrupted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InfraPhase {
    Preflight,
    PreStream,
    Interrupted,
}

impl InfraPhase {
    const fn as_wire(self) -> &'static str {
        match self {
            InfraPhase::Preflight => "preflight",
            InfraPhase::PreStream => "pre-stream",
            InfraPhase::Interrupted => "interrupted",
        }
    }

    const fn first_event_seen(self) -> bool {
        matches!(self, InfraPhase::Interrupted)
    }

    const fn infra_class(self) -> InfraClass {
        match self {
            InfraPhase::Interrupted => InfraClass::Interrupted,
            InfraPhase::Preflight | InfraPhase::PreStream => InfraClass::Preflight,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InfraCause {
    UnexpectedEof,
    MalformedFraming,
    UnknownMessageType,
    HandshakeTimeout,
    ProcessExit,
    ContainerOom,
    Io,
    SinkFailure,
    DriverEnvelope,
    BackendHandler,
}

impl InfraCause {
    const fn as_wire(self) -> &'static str {
        match self {
            InfraCause::UnexpectedEof => "unexpected_eof",
            InfraCause::MalformedFraming => "malformed_framing",
            InfraCause::UnknownMessageType => "unknown_message_type",
            InfraCause::HandshakeTimeout => "handshake_timeout",
            InfraCause::ProcessExit => "process_exit",
            InfraCause::ContainerOom => "container_oom",
            InfraCause::Io => "io",
            InfraCause::SinkFailure => "sink_failure",
            InfraCause::DriverEnvelope => "driver_envelope",
            InfraCause::BackendHandler => "backend_handler",
        }
    }
}

fn stream_infra_phase(first_event_seen: bool) -> InfraPhase {
    if first_event_seen {
        InfraPhase::Interrupted
    } else {
        InfraPhase::PreStream
    }
}

fn infra_session_result(first_event_seen: bool, error: String) -> SessionResult {
    if first_event_seen {
        SessionResult::MidSessionFailed { error }
    } else {
        SessionResult::PreflightFailed { error }
    }
}

fn emit_spawn_failure_event(
    sink: Option<&mut LogSink>,
    builder: Option<&mut EnvelopeBuilder>,
    err: &ProtocolError,
    error: &str,
) {
    emit_infra_failure_event(
        sink,
        builder,
        InfraPhase::Preflight,
        protocol_infra_cause(err),
        error,
        Some(error),
        protocol_exit_status(err),
    );
}

fn emit_protocol_failure_event(
    sink: Option<&mut LogSink>,
    builder: Option<&mut EnvelopeBuilder>,
    first_event_seen: bool,
    err: &ProtocolError,
    error: &str,
) {
    emit_infra_failure_event(
        sink,
        builder,
        stream_infra_phase(first_event_seen),
        protocol_infra_cause(err),
        error,
        None,
        protocol_exit_status(err),
    );
}

fn emit_infra_failure_event(
    sink: Option<&mut LogSink>,
    builder: Option<&mut EnvelopeBuilder>,
    phase: InfraPhase,
    cause: InfraCause,
    error: &str,
    spawn_error: Option<&str>,
    exit_status: Option<i32>,
) {
    let summary = infra_failure_summary(phase, cause, error);
    let mut payload = serde_json::Map::new();
    payload.insert("phase".to_string(), serde_json::json!(phase.as_wire()));
    payload.insert(
        "first_event_seen".to_string(),
        serde_json::json!(phase.first_event_seen()),
    );
    payload.insert(
        "infra_class".to_string(),
        serde_json::json!(phase.infra_class().as_wire()),
    );
    payload.insert("cause".to_string(), serde_json::json!(cause.as_wire()));
    payload.insert("error".to_string(), serde_json::json!(error));
    if let Some(status) = exit_status {
        payload.insert("exit_status".to_string(), serde_json::json!(status));
    }
    if let Some(spawn_error) = spawn_error {
        payload.insert("spawn_error".to_string(), serde_json::json!(spawn_error));
    }
    emit_driver_event(
        sink,
        builder,
        DriverKind::InfraFailure,
        &summary,
        serde_json::Value::Object(payload),
    );
}

fn infra_failure_summary(phase: InfraPhase, cause: InfraCause, error: &str) -> String {
    let class = phase.infra_class().as_wire();
    match (phase, cause) {
        (InfraPhase::Preflight, _) => format!("{class} spawn failure: {error}"),
        (InfraPhase::PreStream, InfraCause::UnexpectedEof) => {
            format!("{class} pre-stream EOF: {error}")
        }
        (InfraPhase::Interrupted, InfraCause::UnexpectedEof) => {
            format!("{class} interrupted EOF after agent output: {error}")
        }
        (InfraPhase::PreStream, InfraCause::ProcessExit) => {
            format!("{class} pre-stream process exit: {error}")
        }
        (InfraPhase::Interrupted, InfraCause::ProcessExit) => {
            format!("{class} process exit after agent output: {error}")
        }
        (_, InfraCause::ContainerOom) => format!("{class} container OOM: {error}"),
        _ => format!("{class} {}: {error}", cause.as_wire()),
    }
}

fn protocol_infra_cause(err: &ProtocolError) -> InfraCause {
    match err {
        ProtocolError::InvalidJson(_)
        | ProtocolError::InvalidProtocolLine { .. }
        | ProtocolError::LineTooLong { .. } => InfraCause::MalformedFraming,
        ProtocolError::UnknownMessageType(_) => InfraCause::UnknownMessageType,
        ProtocolError::Io(_) if is_oom_error(&err.to_string()) => InfraCause::ContainerOom,
        ProtocolError::Io(_) => InfraCause::Io,
        ProtocolError::ProcessExit(code) if *code == 137 => InfraCause::ContainerOom,
        ProtocolError::ProcessExit(_) => InfraCause::ProcessExit,
        ProtocolError::UnexpectedEof => InfraCause::UnexpectedEof,
        ProtocolError::HandshakeTimeout { .. } => InfraCause::HandshakeTimeout,
        ProtocolError::Unsupported | ProtocolError::LockPoisoned => InfraCause::BackendHandler,
    }
}

fn protocol_exit_status(err: &ProtocolError) -> Option<i32> {
    match err {
        ProtocolError::ProcessExit(code) => Some(*code),
        _ => None,
    }
}

fn is_oom_error(error: &str) -> bool {
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

fn log_agent_event(event: &AgentEvent) {
    let summary = summarize_event(event);
    trace!(event = %summary, "agent event");
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
    use loom_driver::agent::{JsonlReader, LineParse, ParsedLine, RePinContent};
    use loom_driver::logging::LogSink;
    use loom_events::identifier::{BeadId, SpecLabel};
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::SystemTime;
    use tokio::io::BufWriter;

    #[derive(Clone)]
    struct SharedBufferWriter {
        inner: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }

    impl std::io::Write for SharedBufferWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.inner
                .lock()
                .map_err(|_| std::io::Error::other("poisoned"))?
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

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
    fn protocol_infra_cause_routes_oom_exit_and_eof() {
        assert_eq!(
            protocol_infra_cause(&ProtocolError::ProcessExit(137)),
            InfraCause::ContainerOom,
        );
        assert_eq!(
            protocol_infra_cause(&ProtocolError::UnexpectedEof),
            InfraCause::UnexpectedEof,
        );
        assert_eq!(
            protocol_infra_cause(&ProtocolError::ProcessExit(1)),
            InfraCause::ProcessExit,
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

    fn sample_spawn_config(scratch: &Path) -> SpawnConfig {
        SpawnConfig {
            image_ref: "localhost/img:tag".into(),
            image_source: PathBuf::from("/nix/store/none.tar"),
            image_source_kind: Some(loom_driver::agent::ImageSourceKind::NixDescriptor),
            profile_config: None,
            workspace: PathBuf::from("/workspace"),
            env: vec![],
            mounts: vec![],
            initial_prompt: "prompt".to_string(),
            agent_args: vec![],
            repin: RePinContent {
                orientation: String::new(),
                pinned_context: String::new(),
                partial_bodies: vec![],
            },
            skills: None,
            scratch_dir: scratch.join("scratch"),
            model: None,
            thinking_level: None,
            output_limits: None,
            shutdown_grace: None,
            handshake_timeout: None,
            stall_warn_interval: Some(Duration::ZERO),
            launcher_env: Vec::new(),
        }
    }

    struct TestParser;

    impl LineParse for TestParser {
        fn parse_line(&self, line: &str) -> Result<ParsedLine, ProtocolError> {
            match line {
                "text" => Ok(ParsedLine {
                    events: vec![ParsedAgentEvent::TextDelta {
                        text: "agent output".to_string(),
                    }],
                    response: None,
                }),
                "complete" => Ok(ParsedLine {
                    events: vec![ParsedAgentEvent::SessionComplete {
                        exit_code: 0,
                        cost_usd: None,
                    }],
                    response: None,
                }),
                "exit7" => Err(ProtocolError::ProcessExit(7)),
                other => Err(ProtocolError::invalid_protocol_line(
                    other,
                    serde_json::from_str::<serde_json::Value>(other).expect_err("invalid JSON"),
                )),
            }
        }

        fn encode_prompt(&self, _msg: &str) -> Result<String, ProtocolError> {
            Ok("prompt\n".to_string())
        }

        fn encode_steer(&self, msg: &str) -> Result<String, ProtocolError> {
            Ok(format!("{msg}\n"))
        }

        fn encode_abort(&self) -> Result<Option<String>, ProtocolError> {
            Ok(None)
        }
    }

    fn spawn_script(script: &str) -> Result<AgentSession<Idle>, ProtocolError> {
        let mut child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ProtocolError::Io(std::io::Error::other("child stdin not piped")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ProtocolError::Io(std::io::Error::other("child stdout not piped")))?;
        Ok(AgentSession::new(
            child,
            BufWriter::new(stdin),
            JsonlReader::new(stdout),
            Box::new(TestParser),
        ))
    }

    fn infra_event(events: &[serde_json::Value]) -> &serde_json::Value {
        events
            .iter()
            .find(|event| event["driver_kind"] == "infra_failure")
            .expect("infra failure event")
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

    fn capture_agent_event_log(filter: &str, event: &AgentEvent) -> String {
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = SharedBufferWriter {
            inner: buffer.clone(),
        };
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
            .with_writer(move || writer.clone())
            .without_time()
            .finish();
        let dispatch = tracing::Dispatch::new(subscriber);
        tracing::dispatcher::with_default(&dispatch, || log_agent_event(event));
        let bytes = buffer.lock().expect("not poisoned").clone();
        String::from_utf8(bytes).expect("utf-8 log output")
    }

    #[test]
    fn agent_event_bookkeeping_uses_trace_level() {
        let event = text_delta_event();
        let debug_output = capture_agent_event_log("debug", &event);
        assert!(
            debug_output.is_empty(),
            "agent event bookkeeping must not be debug/info output: {debug_output:?}",
        );
        let trace_output = capture_agent_event_log("trace", &event);
        assert!(trace_output.contains("agent event"), "{trace_output:?}");
        assert!(trace_output.contains("message_delta"), "{trace_output:?}");
    }

    #[test]
    fn stall_watchdog_renders_coalesced_warning_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let label = SpecLabel::new("emit-test");
        let bead = BeadId::new("lm-emit").expect("bead id");
        let render_buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let renderer = Box::new(loom_driver::logging::TerminalRenderer::new(
            Box::new(SharedBufferWriter {
                inner: render_buffer.clone(),
            }),
            loom_driver::logging::RenderMode::Default,
            bead.clone(),
            false,
            false,
        ));
        let sink = LogSink::open_in_at(
            dir.path(),
            &label,
            &bead,
            Some(renderer),
            SystemTime::UNIX_EPOCH,
        )
        .expect("open sink");
        let path = sink.log_path().to_path_buf();
        let mut sink = Some(sink);
        let mut b = Some(builder());
        let mut emitted = false;
        record_stall_watchdog_tick(
            &mut sink,
            &mut b,
            StallPhase::AwaitEvent,
            Duration::from_secs(30),
            &mut emitted,
        );
        record_stall_watchdog_tick(
            &mut sink,
            &mut b,
            StallPhase::AwaitEvent,
            Duration::from_secs(30),
            &mut emitted,
        );
        finish_sink(sink, BeadOutcome::Failed);

        let events = read_jsonl(&path);
        assert_eq!(
            events.len(),
            1,
            "repeated stall ticks in one silence window coalesce into one row",
        );
        assert_eq!(events[0]["kind"], "driver_event");
        assert_eq!(events[0]["driver_kind"], "stall_watchdog");
        assert_eq!(events[0]["source"], "driver");
        assert_eq!(events[0]["payload"]["severity"], "warning");
        assert_eq!(events[0]["payload"]["phase"], "await_event");
        assert_eq!(events[0]["payload"]["stall_secs"], 30);

        let render = String::from_utf8(render_buffer.lock().expect("not poisoned").clone())
            .expect("utf-8 render");
        assert!(render.contains('⚠'), "{render:?}");
        assert!(render.contains("stall_watchdog"), "{render:?}");
        assert!(
            render.contains("no agent event for stall window"),
            "{render:?}",
        );
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
        let cfg = sample_spawn_config(dir.path());
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
        assert_eq!(events[0]["payload"]["first_event_seen"], false);
        assert_eq!(events[0]["payload"]["infra_class"], "infra-preflight");
        assert_eq!(events[0]["payload"]["cause"], "io");
        assert!(
            events[0]["payload"]["error"]
                .as_str()
                .is_some_and(|s| s.contains("io failure")),
            "payload error body must carry the ProtocolError display: {:?}",
            events[0]["payload"],
        );
        assert!(
            events[0]["payload"]["spawn_error"]
                .as_str()
                .is_some_and(|s| s.contains("io failure")),
            "spawn failures must carry spawn_error: {:?}",
            events[0]["payload"],
        );
    }

    #[tokio::test]
    async fn pre_stream_eof_emits_infra_preflight_driver_event() {
        struct EofBackend;
        impl AgentBackend for EofBackend {
            async fn spawn(_config: &SpawnConfig) -> Result<AgentSession<Idle>, ProtocolError> {
                spawn_script("IFS= read -r _; exit 0")
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let (sink, path) = open_test_sink(dir.path());
        let cfg = sample_spawn_config(dir.path());
        let result =
            run_agent_classified::<EofBackend>(&cfg, Some(sink), None, None, Some(builder())).await;

        match result {
            SessionResult::PreflightFailed { error } => assert!(
                error.contains("unexpected end"),
                "pre-stream EOF must return the EOF detail: {error}",
            ),
            other => panic!("expected PreflightFailed, got {other:?}"),
        }
        let events = read_jsonl(&path);
        assert_eq!(events[0]["driver_kind"], "container_spawn");
        let infra = infra_event(&events);
        assert_eq!(infra["payload"]["phase"], "pre-stream");
        assert_eq!(infra["payload"]["first_event_seen"], false);
        assert_eq!(infra["payload"]["infra_class"], "infra-preflight");
        assert_eq!(infra["payload"]["cause"], "unexpected_eof");
        assert!(
            infra["summary"]
                .as_str()
                .is_some_and(|summary| summary.contains("pre-stream EOF")),
            "summary must distinguish pre-stream EOF: {infra:?}",
        );
    }

    #[tokio::test]
    async fn partial_stream_eof_emits_infra_interrupted_driver_event() {
        struct PartialBackend;
        impl AgentBackend for PartialBackend {
            async fn spawn(_config: &SpawnConfig) -> Result<AgentSession<Idle>, ProtocolError> {
                spawn_script("IFS= read -r _; printf '%s\\n' text; exit 0")
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let (sink, path) = open_test_sink(dir.path());
        let cfg = sample_spawn_config(dir.path());
        let result =
            run_agent_classified::<PartialBackend>(&cfg, Some(sink), None, None, Some(builder()))
                .await;

        match result {
            SessionResult::MidSessionFailed { error } => assert!(
                error.contains("unexpected end"),
                "interrupted EOF must return the EOF detail: {error}",
            ),
            other => panic!("expected MidSessionFailed, got {other:?}"),
        }
        let events = read_jsonl(&path);
        assert!(
            events.iter().any(|event| event["kind"] == "text_delta"),
            "partial stream must log the agent event before infra: {events:?}",
        );
        let infra = infra_event(&events);
        assert_eq!(infra["payload"]["phase"], "interrupted");
        assert_eq!(infra["payload"]["first_event_seen"], true);
        assert_eq!(infra["payload"]["infra_class"], "infra-interrupted");
        assert_eq!(infra["payload"]["cause"], "unexpected_eof");
        assert!(
            infra["summary"]
                .as_str()
                .is_some_and(|summary| summary.contains("interrupted EOF")),
            "summary must distinguish interrupted EOF: {infra:?}",
        );
    }

    #[tokio::test]
    async fn pre_stream_process_exit_payload_carries_exit_status() {
        struct ExitBackend;
        impl AgentBackend for ExitBackend {
            async fn spawn(_config: &SpawnConfig) -> Result<AgentSession<Idle>, ProtocolError> {
                spawn_script("IFS= read -r _; printf '%s\\n' exit7; exit 0")
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let (sink, path) = open_test_sink(dir.path());
        let cfg = sample_spawn_config(dir.path());
        let result =
            run_agent_classified::<ExitBackend>(&cfg, Some(sink), None, None, Some(builder()))
                .await;

        match result {
            SessionResult::PreflightFailed { error } => assert!(
                error.contains("code 7"),
                "pre-stream process exit must return status detail: {error}",
            ),
            other => panic!("expected PreflightFailed, got {other:?}"),
        }
        let events = read_jsonl(&path);
        let infra = infra_event(&events);
        assert_eq!(infra["payload"]["phase"], "pre-stream");
        assert_eq!(infra["payload"]["first_event_seen"], false);
        assert_eq!(infra["payload"]["cause"], "process_exit");
        assert_eq!(infra["payload"]["exit_status"], 7);
    }

    #[tokio::test]
    async fn session_complete_is_not_overwritten_by_later_stream_noise() {
        struct CompleteThenNoiseBackend;
        impl AgentBackend for CompleteThenNoiseBackend {
            async fn spawn(_config: &SpawnConfig) -> Result<AgentSession<Idle>, ProtocolError> {
                spawn_script("IFS= read -r _; printf '%s\\n' complete; printf '%s\\n' bad; exit 7")
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let (sink, path) = open_test_sink(dir.path());
        let cfg = sample_spawn_config(dir.path());
        let result = run_agent_classified::<CompleteThenNoiseBackend>(
            &cfg,
            Some(sink),
            None,
            None,
            Some(builder()),
        )
        .await;

        match result {
            SessionResult::Complete(outcome) => assert_eq!(outcome.exit_code, 0),
            other => panic!("expected Complete, got {other:?}"),
        }
        let events = read_jsonl(&path);
        assert!(
            events
                .iter()
                .any(|event| event["kind"] == "session_complete"),
            "session_complete must be logged: {events:?}",
        );
        assert!(
            events
                .iter()
                .all(|event| event["driver_kind"] != "infra_failure"),
            "later stream noise must not append infra diagnostics: {events:?}",
        );
    }
}

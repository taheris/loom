use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use loom_driver::clock::{Clock, SystemClock};
use loom_events::identifier::SessionId;
use loom_events::{AgentEvent, EnvelopeBuilder, SessionScope, Source};

#[derive(Debug, Clone, Copy)]
pub(crate) enum RoutePhase {
    Review,
    Todo,
}

impl RoutePhase {
    const fn as_wire(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Todo => "todo",
        }
    }
}

pub(crate) fn resume_phase_driver_envelope(
    path: &Path,
    phase: RoutePhase,
    when: SystemTime,
) -> EnvelopeBuilder {
    let phase_name = phase.as_wire();
    let mut resume_seq = 0;
    let mut session_id = None;
    match std::fs::read_to_string(path) {
        Ok(body) => {
            for (line_index, line) in body.lines().enumerate() {
                match serde_json::from_str::<AgentEvent>(line) {
                    Ok(event) => {
                        resume_seq = resume_seq.max(event.envelope().seq.saturating_add(1));
                        if session_id.is_none() {
                            session_id = Some(event.envelope().session_id.clone());
                        }
                    }
                    Err(error) => tracing::warn!(
                        error = ?error,
                        path = %path.display(),
                        phase = phase_name,
                        line = line_index.saturating_add(1),
                        "phase event log contains malformed JSONL",
                    ),
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            error = ?error,
            path = %path.display(),
            phase = phase_name,
            "phase event log could not be read for envelope resume",
        ),
    }
    let started_ms = when
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    let session_id = session_id.unwrap_or_else(|| {
        SessionId::new(format!("{phase_name}-{}-{started_ms}", std::process::id()))
    });
    let clock = SystemClock::new();
    EnvelopeBuilder::with_seq_start(
        SessionScope::phase(session_id, None),
        Source::Driver,
        resume_seq,
        move || {
            clock
                .wall_now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_millis() as i64)
        },
    )
}

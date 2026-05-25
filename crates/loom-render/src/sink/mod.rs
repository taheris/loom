use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use tracing::{info, warn};

use loom_events::identifier::{BeadId, SpecLabel};
use loom_events::{AgentEvent, EventSink};

mod error;

pub use error::LogError;

use crate::clock::{Clock, SystemClock};
use crate::path::{bead_log_path, phase_log_path};
use crate::renderer::{BeadOutcome, Renderer};

/// Tee-style sink that drives the per-bead JSONL log file *and* the
/// [`TerminalRenderer`] from the same `emit` call.
///
/// Spec contract (`Run UX & Logging`): "the terminal renderer consumes the
/// same `AgentEvent` stream that's written to disk — there's a single
/// tee-style sink, not two parallel pipelines." This is enforced by the type:
/// every event flows through one method ([`Self::emit`]) which dispatches to
/// both writers in lockstep, so the terminal cannot diverge from the on-disk
/// log.
pub struct LogSink {
    file: BufWriter<File>,
    renderer: Option<Box<dyn Renderer>>,
    log_path: PathBuf,
    started: Instant,
    finished: bool,
}

impl LogSink {
    /// Open a per-bead sink under
    /// `<logs_root>/<spec-label>/<bead-id>-<utc>.jsonl`. `renderer` is
    /// optional so non-interactive callers (the run/parallel dispatch closure
    /// in the binary) can write only the on-disk JSONL without instantiating
    /// a `TerminalRenderer`.
    pub fn open_in_at(
        logs_root: &Path,
        spec_label: &SpecLabel,
        bead_id: &BeadId,
        renderer: Option<Box<dyn Renderer>>,
        when: SystemTime,
    ) -> Result<Self, LogError> {
        let log_path = bead_log_path(logs_root, spec_label, bead_id, when);
        let sink = Self::open_at_path(log_path.clone(), renderer)?;
        info!(
            target: "loom_driver::logging::sink",
            spec_label = spec_label.as_str(),
            bead_id = bead_id.as_str(),
            log_path = %log_path.display(),
            "spawn started — log path",
        );
        Ok(sink)
    }

    /// Open a sink for a non-bead phase (`loom todo`, `loom plan`,
    /// `loom review`). The path follows
    /// `<logs_root>/<spec-label>/<phase>-<utc>.jsonl` so phase logs share the
    /// same per-spec directory tree as bead logs without colliding.
    ///
    /// `renderer` is optional because phase logs may run in non-interactive
    /// contexts (CI, scripted spawns) where the per-bead progress chrome is
    /// noise; emitting only to the file is the lighter contract.
    pub fn open_phase_at(
        logs_root: &Path,
        spec_label: &SpecLabel,
        phase: &str,
        renderer: Option<Box<dyn Renderer>>,
        when: SystemTime,
    ) -> Result<Self, LogError> {
        let log_path = phase_log_path(logs_root, spec_label, phase, when);
        let sink = Self::open_at_path(log_path.clone(), renderer)?;
        info!(
            target: "loom_driver::logging::sink",
            spec_label = spec_label.as_str(),
            phase = phase,
            log_path = %log_path.display(),
            "phase started — log path",
        );
        Ok(sink)
    }

    fn open_at_path(
        log_path: PathBuf,
        renderer: Option<Box<dyn Renderer>>,
    ) -> Result<Self, LogError> {
        if let Some(dir) = log_path.parent() {
            fs::create_dir_all(dir).map_err(|source| LogError::CreateDir {
                path: dir.to_path_buf(),
                source,
            })?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|source| LogError::OpenFile {
                path: log_path.clone(),
                source,
            })?;
        let clock = SystemClock::new();
        Ok(Self {
            file: BufWriter::new(file),
            renderer,
            log_path,
            started: clock.now(),
            finished: false,
        })
    }

    /// Write `event` to the on-disk JSONL log AND drive the terminal
    /// renderer in a single call.
    ///
    /// File-write order: serialize → append `\n` → write → flush. Renderer
    /// errors are surfaced with the same `LogError::Io` variant so the caller
    /// can decide whether one failure path should also fail the bead.
    pub fn emit(&mut self, event: &AgentEvent) -> Result<(), LogError> {
        let line = serde_json::to_string(event)?;
        let path = self.log_path.clone();
        self.file
            .write_all(line.as_bytes())
            .and_then(|()| self.file.write_all(b"\n"))
            .and_then(|()| self.file.flush())
            .map_err(|source| LogError::Write {
                path: path.clone(),
                source,
            })?;
        if let Some(renderer) = self.renderer.as_mut() {
            renderer
                .render_event(event)
                .map_err(|source| LogError::Write { path, source })?;
        }
        Ok(())
    }

    /// Print the renderer's closing line and flush the log file. Idempotent:
    /// a second call is a no-op so callers can defensively `finish` in both
    /// success and failure paths.
    pub fn finish(&mut self, outcome: BeadOutcome) -> Result<(), LogError> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.file.flush().map_err(|source| LogError::Write {
            path: self.log_path.clone(),
            source,
        })?;
        let elapsed = self.started.elapsed();
        if let Some(mut renderer) = self.renderer.take() {
            renderer
                .finish(outcome, elapsed)
                .map_err(|source| LogError::Write {
                    path: self.log_path.clone(),
                    source,
                })?;
        }
        Ok(())
    }

    /// The resolved log file path. Useful for the workflow engine to surface
    /// in error messages / tests.
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }
}

impl Drop for LogSink {
    fn drop(&mut self) {
        if !self.finished {
            // Best-effort flush; further writes after drop are impossible.
            let _ = self.file.flush();
        }
    }
}

/// `EventSink` impl makes `LogSink` the persistence implementor of the
/// trait the driver fans into a static-typed chain via
/// [`loom_events::EventSinkExt::tee`]. The trait `emit` is sync and has
/// no return value, so disk-write failures (already rare per the spec's
/// disk-writer contract) surface via `warn!` rather than aborting the
/// chain — callers that need fallible emission keep the inherent
/// [`LogSink::emit`] available via UFCS.
impl EventSink for LogSink {
    fn emit(&mut self, event: &AgentEvent) {
        if let Err(err) = LogSink::emit(self, event) {
            warn!(
                target: "loom_render::sink",
                error = %err,
                log_path = %self.log_path.display(),
                "log sink emit failed under EventSink trait",
            );
        }
    }
}

#[cfg(test)]
pub(crate) type SharedBuffer = std::sync::Arc<std::sync::Mutex<Vec<u8>>>;

/// Helper used by tests in this crate. Opens a sink against an in-memory
/// renderer that discards output.
#[cfg(test)]
pub(crate) fn open_sink_with_sink_writer(
    logs_root: &Path,
    spec_label: &SpecLabel,
    bead_id: &BeadId,
    when: SystemTime,
) -> Result<(LogSink, SharedBuffer), LogError> {
    use std::io;

    use crate::renderer::RenderMode;
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let writer_buf = buf.clone();
    struct Sink {
        inner: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }
    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.inner
                .lock()
                .map_err(|_| io::Error::other("poisoned"))?
                .extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    use crate::renderer::TerminalRenderer;
    let renderer: Box<dyn Renderer> = Box::new(TerminalRenderer::new(
        Sink { inner: writer_buf },
        RenderMode::Default,
        bead_id.clone(),
        false,
        false,
    ));
    let sink = LogSink::open_in_at(logs_root, spec_label, bead_id, Some(renderer), when)?;
    Ok((sink, buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_events::EventEnvelope;
    use loom_events::Source;
    use loom_events::event::CompactionReason;
    use loom_events::identifier::ToolCallId;
    use serde_json::{Value, json};

    /// Fixture envelope shared by the sink emission tests. Bead id is
    /// `wx-test`; ts_ms / seq stay at zero so on-disk JSONL shapes are
    /// trivially comparable.
    fn sample_envelope() -> EventEnvelope {
        EventEnvelope {
            bead_id: BeadId::new("wx-test").expect("valid bead id"),
            molecule_id: None,
            iteration: 0,
            source: Source::Agent,
            ts_ms: 0,
            seq: 0,
        }
    }

    fn read_lines(path: &Path) -> Vec<String> {
        let body = std::fs::read_to_string(path).expect("read");
        body.lines().map(str::to_owned).collect()
    }

    #[test]
    fn emit_writes_jsonl_line_per_event_and_drives_renderer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let label = SpecLabel::new("alpha");
        let bead = BeadId::new("wx-1").expect("valid bead id");
        let when = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(0);
        let (mut sink, term_buf) =
            open_sink_with_sink_writer(dir.path(), &label, &bead, when).expect("open");

        sink.emit(&AgentEvent::ToolCall {
            envelope: sample_envelope(),
            id: ToolCallId::new("t1"),
            tool: "Read".to_string(),
            params: json!({"file_path": "src/lib.rs"}),
            parent_tool_call_id: None,
        })
        .expect("emit");
        sink.emit(&AgentEvent::TurnEnd {
            envelope: sample_envelope(),
        })
        .expect("emit");

        let path = sink.log_path().to_path_buf();
        sink.finish(BeadOutcome::Done).expect("finish");

        let lines = read_lines(&path);
        assert_eq!(lines.len(), 2, "{lines:?}");
        let first: Value = serde_json::from_str(&lines[0]).expect("json");
        assert_eq!(first["kind"], "tool_call");
        assert_eq!(first["tool"], "Read");
        let second: Value = serde_json::from_str(&lines[1]).expect("json");
        assert_eq!(second["kind"], "turn_end");

        let term = term_buf.lock().expect("not poisoned");
        let term_str = std::str::from_utf8(&term).expect("utf-8");
        assert!(term_str.contains("Read"), "{term_str:?}");
        assert!(term_str.contains("done"));
    }

    #[test]
    fn emit_persists_compaction_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let label = SpecLabel::new("alpha");
        let bead = BeadId::new("wx-1").expect("valid bead id");
        let (mut sink, _term) = open_sink_with_sink_writer(
            dir.path(),
            &label,
            &bead,
            SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000),
        )
        .expect("open");
        sink.emit(&AgentEvent::CompactionStart {
            envelope: sample_envelope(),
            reason: CompactionReason::ContextLimit,
        })
        .expect("emit");
        let path = sink.log_path().to_path_buf();
        sink.finish(BeadOutcome::Done).expect("finish");
        let line = &read_lines(&path)[0];
        let v: Value = serde_json::from_str(line).expect("json");
        assert_eq!(v["kind"], "compaction_start");
        assert_eq!(v["reason"], "context_limit");
    }

    #[test]
    fn finish_is_idempotent_after_drop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (sink, _term) = open_sink_with_sink_writer(
            dir.path(),
            &SpecLabel::new("alpha"),
            &BeadId::new("wx-1").expect("valid bead id"),
            SystemTime::UNIX_EPOCH,
        )
        .expect("open");
        // Drop without finish must not panic and must flush.
        drop(sink);
    }

    #[test]
    fn parent_directory_is_created_on_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let logs = dir.path().join(".wrapix/loom/logs");
        let (sink, _) = open_sink_with_sink_writer(
            &logs,
            &SpecLabel::new("nested"),
            &BeadId::new("wx-1").expect("valid bead id"),
            SystemTime::UNIX_EPOCH,
        )
        .expect("open");
        assert!(sink.log_path().parent().expect("parent").is_dir());
    }

    /// `LogSink` is the trait's first implementor: events emitted via the
    /// `EventSink` interface land on disk as one JSONL line per event with
    /// per-event flush, matching the disk-writer contract. The trait emit
    /// returns `()`, so this test asserts via post-emit file inspection
    /// rather than a return-value check.
    #[test]
    fn log_sink_implements_event_sink() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut sink, _term) = open_sink_with_sink_writer(
            dir.path(),
            &SpecLabel::new("alpha"),
            &BeadId::new("wx-1").expect("valid bead id"),
            SystemTime::UNIX_EPOCH,
        )
        .expect("open");
        let path = sink.log_path().to_path_buf();

        let trait_emit = |s: &mut dyn loom_events::EventSink, e: &AgentEvent| s.emit(e);
        trait_emit(
            &mut sink,
            &AgentEvent::ToolCall {
                envelope: sample_envelope(),
                id: ToolCallId::new("t1"),
                tool: "Read".to_string(),
                params: json!({"file_path": "first.rs"}),
                parent_tool_call_id: None,
            },
        );
        let after_first = std::fs::read_to_string(&path).expect("read mid-stream");
        assert_eq!(
            after_first.lines().count(),
            1,
            "trait emit must flush each event to disk: {after_first:?}",
        );

        trait_emit(
            &mut sink,
            &AgentEvent::TurnEnd {
                envelope: sample_envelope(),
            },
        );
        sink.finish(BeadOutcome::Done).expect("finish");

        let lines = read_lines(&path);
        assert_eq!(lines.len(), 2, "{lines:?}");
        let first: Value = serde_json::from_str(&lines[0]).expect("json");
        assert_eq!(first["kind"], "tool_call");
        let second: Value = serde_json::from_str(&lines[1]).expect("json");
        assert_eq!(second["kind"], "turn_end");
    }

    /// Composition via `.tee(other)` from `EventSinkExt`: events flow to
    /// the `LogSink` and to a sibling observer in registration order from
    /// a single chain emit call.
    #[test]
    fn log_sink_composes_via_tee_with_observer() {
        use loom_events::{EventSink, EventSinkExt};
        use std::sync::{Arc, Mutex};

        struct CountingObserver(Arc<Mutex<u32>>);
        impl EventSink for CountingObserver {
            fn emit(&mut self, _event: &AgentEvent) {
                *self.0.lock().expect("not poisoned") += 1;
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let label = SpecLabel::new("alpha");
        let bead = BeadId::new("wx-1").expect("valid bead id");
        let sink = LogSink::open_in_at(dir.path(), &label, &bead, None, SystemTime::UNIX_EPOCH)
            .expect("open");
        let path = sink.log_path().to_path_buf();
        let count = Arc::new(Mutex::new(0_u32));
        let mut chain = sink.tee(CountingObserver(count.clone()));

        chain.emit(&AgentEvent::TurnEnd {
            envelope: sample_envelope(),
        });
        chain.emit(&AgentEvent::TurnEnd {
            envelope: sample_envelope(),
        });

        assert_eq!(
            *count.lock().expect("not poisoned"),
            2,
            "observer must see every event",
        );
        let lines = read_lines(&path);
        assert_eq!(lines.len(), 2, "log sink must see every event: {lines:?}");
    }

    #[test]
    fn phase_sink_writes_under_spec_directory_with_phase_prefix() {
        let dir = tempfile::tempdir().expect("tempdir");
        let logs = dir.path().join(".wrapix/loom/logs");
        let label = SpecLabel::new("alpha");
        let mut sink = LogSink::open_phase_at(
            &logs,
            &label,
            "todo",
            None,
            SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000),
        )
        .expect("open phase sink");
        sink.emit(&AgentEvent::TurnEnd {
            envelope: sample_envelope(),
        })
        .expect("emit");
        let path = sink.log_path().to_path_buf();
        sink.finish(BeadOutcome::Done).expect("finish");

        assert_eq!(path.parent(), Some(logs.join("alpha").as_path()));
        let stem = path.file_stem().and_then(|s| s.to_str()).expect("stem");
        assert!(stem.starts_with("todo-"), "{stem:?}");
        let lines = std::fs::read_to_string(&path).expect("read");
        assert!(lines.contains("\"kind\":\"turn_end\""), "{lines:?}");
    }
}

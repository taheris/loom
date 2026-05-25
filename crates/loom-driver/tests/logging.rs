//! Integration tests for `loom_driver::logging`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Result, anyhow};
use loom_driver::agent::AgentEvent;
use loom_driver::identifier::{BeadId, SpecLabel, ToolCallId};
use loom_driver::logging::{
    BeadOutcome, LogSink, RenderMode, TerminalRenderer, sweep_retention_at,
};
use loom_events::{EventEnvelope, Source};
use serde_json::{Value, json};

/// Fixture envelope shared by every emission test. Bead id `wx-test`
/// is fully valid; the sink rewrites bead-keyed paths from its own
/// constructor arg, so `bead_id` on the envelope is observed only by
/// the JSONL writer.
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

/// `Write` adapter that pushes into a shared in-memory buffer so tests can
/// inspect what the renderer printed.
struct SharedSink(Arc<Mutex<Vec<u8>>>);

impl Write for SharedSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn captured() -> (Arc<Mutex<Vec<u8>>>, SharedSink) {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let sink = SharedSink(buf.clone());
    (buf, sink)
}

fn captured_str(buf: &Arc<Mutex<Vec<u8>>>) -> String {
    let g = buf.lock().expect("not poisoned");
    String::from_utf8(g.clone()).expect("utf-8")
}

fn open_sink(
    logs_root: &Path,
    spec: &str,
    bead: &str,
    when_secs: u64,
    mode: RenderMode,
    parallel: bool,
) -> Result<(LogSink, Arc<Mutex<Vec<u8>>>)> {
    let label = SpecLabel::new(spec);
    let id = BeadId::new(bead)?;
    let (buf, sink) = captured();
    let renderer: Box<dyn loom_render::Renderer> = Box::new(TerminalRenderer::new(
        sink,
        mode,
        id.clone(),
        parallel,
        false,
    ));
    let when = SystemTime::UNIX_EPOCH + Duration::from_secs(when_secs);
    let s = LogSink::open_in_at(logs_root, &label, &id, Some(renderer), when)?;
    Ok((s, buf))
}

/// Open a sink whose `TerminalRenderer` has `color=true` so the
/// in-place running indicator is active. Used by the cancellation /
/// running-indicator tests that need to assert `\r` + clear-to-EOL
/// sequences appear in the captured buffer.
fn open_sink_with_indicator(
    logs_root: &Path,
    spec: &str,
    bead: &str,
    when_secs: u64,
    mode: RenderMode,
) -> Result<(LogSink, Arc<Mutex<Vec<u8>>>)> {
    let label = SpecLabel::new(spec);
    let id = BeadId::new(bead)?;
    let (buf, sink) = captured();
    let renderer: Box<dyn loom_render::Renderer> =
        Box::new(TerminalRenderer::new(sink, mode, id.clone(), false, true));
    let when = SystemTime::UNIX_EPOCH + Duration::from_secs(when_secs);
    let s = LogSink::open_in_at(logs_root, &label, &id, Some(renderer), when)?;
    Ok((s, buf))
}

fn touch(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, body).expect("write");
}

fn set_mtime(path: &Path, when: SystemTime) {
    let f = fs::File::options()
        .write(true)
        .open(path)
        .expect("open for mtime");
    f.set_modified(when).expect("set_modified");
}

//---------------------------------------------------------------------------
// test_run_default_output_shape — default render mode prints exactly one
// header line per bead and one short line per tool call; assistant text
// deltas are suppressed. A closing line carries tool count + duration.
//---------------------------------------------------------------------------
#[test]
fn run_default_output_shape() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (mut sink, buf) = open_sink(
        dir.path(),
        "alpha",
        "wx-1",
        1_700_000_000,
        RenderMode::Default,
        false,
    )?;

    sink.emit(&AgentEvent::ToolCall {
        envelope: sample_envelope(),
        id: ToolCallId::new("t1"),
        tool: "Read".to_string(),
        params: json!({"file_path": "src/lib.rs"}),
        parent_tool_call_id: None,
    })?;
    sink.emit(&AgentEvent::TextDelta {
        envelope: sample_envelope(),
        text: "I will not appear on the terminal".to_string(),
    })?;
    sink.emit(&AgentEvent::ToolCall {
        envelope: sample_envelope(),
        id: ToolCallId::new("t2"),
        tool: "Bash".to_string(),
        params: json!({"command": "cargo build"}),
        parent_tool_call_id: None,
    })?;
    sink.finish(BeadOutcome::Done)?;

    let term = captured_str(&buf);
    let lines: Vec<&str> = term.lines().collect();
    if lines.len() != 3 {
        return Err(anyhow!(
            "expected 2 tool lines + 1 finish line, got {}: {term:?}",
            lines.len()
        ));
    }
    if !lines[0].contains("Read") || !lines[0].contains("src/lib.rs") {
        return Err(anyhow!("first tool line missing Read+path: {term:?}"));
    }
    if !lines[1].contains("Bash") || !lines[1].contains("cargo build") {
        return Err(anyhow!("second tool line missing Bash+cmd: {term:?}"));
    }
    if !lines[2].contains("done") || !lines[2].contains("2 tool calls") {
        return Err(anyhow!("finish line missing done/count: {term:?}"));
    }
    if term.contains("I will not appear on the terminal") {
        return Err(anyhow!("default mode leaked assistant text: {term:?}"));
    }
    Ok(())
}

//---------------------------------------------------------------------------
// test_run_verbose_streams_text — verbose mode streams MessageDelta verbatim.
//---------------------------------------------------------------------------
#[test]
fn run_verbose_streams_text() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (mut sink, buf) = open_sink(
        dir.path(),
        "alpha",
        "wx-1",
        1_700_000_000,
        RenderMode::Verbose,
        false,
    )?;

    sink.emit(&AgentEvent::TextDelta {
        envelope: sample_envelope(),
        text: "Hello, ".to_string(),
    })?;
    sink.emit(&AgentEvent::TextDelta {
        envelope: sample_envelope(),
        text: "world!\n".to_string(),
    })?;
    sink.finish(BeadOutcome::Done)?;

    let term = captured_str(&buf);
    if !term.contains("Hello, world!") {
        return Err(anyhow!("verbose mode dropped assistant text: {term:?}"));
    }
    Ok(())
}

//---------------------------------------------------------------------------
// test_run_writes_per_bead_jsonl_log — full event stream is persisted to
// `<logs_root>/<spec>/<bead>-<utc>.jsonl`, one JSON object per line.
//---------------------------------------------------------------------------
#[test]
fn run_writes_per_bead_jsonl_log() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (mut sink, _) = open_sink(
        dir.path(),
        "alpha",
        "wx-1",
        1_700_000_000,
        RenderMode::Default,
        false,
    )?;
    let log_path = sink.log_path().to_path_buf();

    sink.emit(&AgentEvent::ToolCall {
        envelope: sample_envelope(),
        id: ToolCallId::new("t1"),
        tool: "Read".to_string(),
        params: json!({"file_path": "a"}),
        parent_tool_call_id: None,
    })?;
    // MessageDelta is suppressed by the default renderer, but it MUST appear
    // in the on-disk log (the file gets the full raw event stream regardless
    // of terminal verbosity).
    sink.emit(&AgentEvent::TextDelta {
        envelope: sample_envelope(),
        text: "secret thoughts".to_string(),
    })?;
    sink.emit(&AgentEvent::TurnEnd {
        envelope: sample_envelope(),
    })?;
    sink.finish(BeadOutcome::Done)?;

    let parent = log_path
        .parent()
        .ok_or_else(|| anyhow!("log path has no parent"))?;
    if !parent.ends_with("alpha") {
        return Err(anyhow!(
            "log path not nested under spec label: {}",
            log_path.display()
        ));
    }
    let body = fs::read_to_string(&log_path)?;
    let lines: Vec<&str> = body.lines().collect();
    if lines.len() != 3 {
        return Err(anyhow!(
            "expected 3 JSONL lines, got {}: {body:?}",
            lines.len()
        ));
    }
    let kinds: Vec<String> = lines
        .iter()
        .map(|l| -> Result<String> {
            let v: Value = serde_json::from_str(l)?;
            Ok(v["kind"].as_str().unwrap_or("").to_string())
        })
        .collect::<Result<_>>()?;
    if kinds != vec!["tool_call", "text_delta", "turn_end"] {
        return Err(anyhow!("unexpected event kinds in log: {kinds:?}"));
    }
    if !body.contains("secret thoughts") {
        return Err(anyhow!(
            "MessageDelta text missing from on-disk log even though terminal suppressed it"
        ));
    }
    Ok(())
}

//---------------------------------------------------------------------------
// test_run_logs_log_path — opening a sink emits an `info!` event that
// carries the resolved log path.
//
// Implementation note: tracing's callsite-interest cache is computed against
// whichever subscriber is current the *first* time the callsite fires. With
// `cargo test`'s parallel runner, a sibling test on the same thread can fire
// the callsite under the no-op global default before this test installs its
// own thread-local subscriber, after which the callsite stays "disabled" on
// that thread. We sidestep the cache by installing exactly one global
// subscriber for the test process and routing per-test via a thread-local
// writer — every thread sees the same enabled subscriber, so the cache
// agrees, and isolation comes from the writer instead.
//---------------------------------------------------------------------------
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

    pub fn install_thread_writer(buf: Buffer) -> Guard {
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
                .with_max_level(tracing::Level::INFO)
                .with_ansi(false)
                .finish();
            // If another test crate already set a global default we silently
            // ignore — our writer just won't capture, and the assertion below
            // will give a clear error.
            let _ = tracing::subscriber::set_global_default(subscriber);
        });
    }
}

#[test]
fn run_logs_log_path() -> Result<()> {
    log_capture::init_global_subscriber();
    let buf: log_capture::Buffer = Arc::new(Mutex::new(Vec::new()));
    let _guard = log_capture::install_thread_writer(buf.clone());

    let dir = tempfile::tempdir()?;
    let (sink, _) = open_sink(
        dir.path(),
        "alpha",
        "wx-1",
        1_700_000_000,
        RenderMode::Default,
        false,
    )?;
    let log_path_str = sink.log_path().display().to_string();

    let logs = String::from_utf8(buf.lock().expect("poisoned").clone())?;
    if !logs.contains("INFO") {
        return Err(anyhow!("expected an info-level event: {logs:?}"));
    }
    if !logs.contains(&log_path_str) {
        return Err(anyhow!(
            "spawn log line did not include resolved path {log_path_str:?}: {logs:?}"
        ));
    }
    Ok(())
}

//---------------------------------------------------------------------------
// test_parallel_logs_are_per_bead — running two beads against the same logs
// root and spec writes two separate files, never interleaved.
//---------------------------------------------------------------------------
#[test]
fn parallel_logs_are_per_bead() -> Result<()> {
    let dir = tempfile::tempdir()?;
    // Same spec, two beads, distinct timestamps so the file names differ.
    let (mut a, _) = open_sink(
        dir.path(),
        "alpha",
        "wx-1",
        1_700_000_000,
        RenderMode::Default,
        true,
    )?;
    let (mut b, _) = open_sink(
        dir.path(),
        "alpha",
        "wx-2",
        1_700_000_001,
        RenderMode::Default,
        true,
    )?;

    let path_a = a.log_path().to_path_buf();
    let path_b = b.log_path().to_path_buf();
    if path_a == path_b {
        return Err(anyhow!(
            "parallel beads should write to distinct files; both got {}",
            path_a.display()
        ));
    }

    // Interleave emits and verify each file only carries its own bead's events.
    a.emit(&AgentEvent::ToolCall {
        envelope: sample_envelope(),
        id: ToolCallId::new("ta"),
        tool: "Read".to_string(),
        params: json!({"file_path": "a-only"}),
        parent_tool_call_id: None,
    })?;
    b.emit(&AgentEvent::ToolCall {
        envelope: sample_envelope(),
        id: ToolCallId::new("tb"),
        tool: "Read".to_string(),
        params: json!({"file_path": "b-only"}),
        parent_tool_call_id: None,
    })?;
    a.emit(&AgentEvent::TurnEnd {
        envelope: sample_envelope(),
    })?;
    b.emit(&AgentEvent::TurnEnd {
        envelope: sample_envelope(),
    })?;
    a.finish(BeadOutcome::Done)?;
    b.finish(BeadOutcome::Done)?;

    let body_a = fs::read_to_string(&path_a)?;
    let body_b = fs::read_to_string(&path_b)?;
    if body_a.contains("b-only") || body_b.contains("a-only") {
        return Err(anyhow!(
            "parallel logs cross-contaminated:\nA={body_a:?}\nB={body_b:?}"
        ));
    }
    Ok(())
}

//---------------------------------------------------------------------------
// test_log_retention_sweep — files older than retention_days are deleted;
// recent files are preserved.
//---------------------------------------------------------------------------
#[test]
fn log_retention_sweep() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let stale = now - Duration::from_secs(20 * 86_400);
    let recent = now - Duration::from_secs(2 * 86_400);

    let p_stale = dir.path().join("alpha/wx-stale.jsonl");
    let p_recent = dir.path().join("alpha/wx-recent.jsonl");
    touch(&p_stale, "stale");
    touch(&p_recent, "recent");
    set_mtime(&p_stale, stale);
    set_mtime(&p_recent, recent);

    let report = sweep_retention_at(dir.path(), 14, now);
    if report.deleted.len() != 1 {
        return Err(anyhow!("expected 1 delete, got {report:?}"));
    }
    if p_stale.exists() {
        return Err(anyhow!("stale file should have been deleted"));
    }
    if !p_recent.exists() {
        return Err(anyhow!("recent file must survive"));
    }
    Ok(())
}

//---------------------------------------------------------------------------
// test_log_retention_disabled — retention_days=0 disables the sweep entirely.
//---------------------------------------------------------------------------
#[test]
fn log_retention_disabled() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let very_old = now - Duration::from_secs(365 * 86_400);
    let p = dir.path().join("alpha/wx-1.jsonl");
    touch(&p, "ancient");
    set_mtime(&p, very_old);

    let report = sweep_retention_at(dir.path(), 0, now);
    if !report.deleted.is_empty() {
        return Err(anyhow!("retention_days=0 must skip sweep; got {report:?}"));
    }
    if !p.exists() {
        return Err(anyhow!("file should not have been deleted with sweep off"));
    }
    Ok(())
}

//---------------------------------------------------------------------------
// test_log_retention_failure_tolerance — when an entry cannot be deleted
// (here: a read-only directory blocking removal of one file), the sweep
// still completes for the rest and surfaces the failure in the report.
//---------------------------------------------------------------------------
#[test]
fn log_retention_failure_tolerance() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let stale = now - Duration::from_secs(60 * 86_400);

    // Two files in two subdirs. We'll lock the first subdir read-only so
    // unlink fails for that file; the second subdir remains writable.
    let locked_dir = dir.path().join("alpha");
    let writable_dir = dir.path().join("beta");
    let locked_file = locked_dir.join("wx-locked.jsonl");
    let writable_file = writable_dir.join("wx-free.jsonl");
    touch(&locked_file, "x");
    touch(&writable_file, "y");
    set_mtime(&locked_file, stale);
    set_mtime(&writable_file, stale);

    // chmod 0o555 — inhibits unlink in the locked subdir.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&locked_dir)?.permissions();
        perms.set_mode(0o555);
        fs::set_permissions(&locked_dir, perms)?;
    }

    let report = sweep_retention_at(dir.path(), 14, now);

    // Restore perms so tempdir cleanup works regardless of test outcome.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&locked_dir)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&locked_dir, perms)?;
    }

    if writable_file.exists() {
        return Err(anyhow!(
            "writable file should have been deleted by the sweep"
        ));
    }
    // Sweep must NOT abort: the writable file got deleted even though the
    // locked one may have failed (on platforms without enforced dir perms,
    // both succeed — either way the sweep returned without aborting).
    if report.deleted.is_empty() {
        return Err(anyhow!("expected at least one delete; got {report:?}"));
    }
    Ok(())
}

//---------------------------------------------------------------------------
// test_log_sink_per_event_flush — every `LogSink::emit` call must flush the
// underlying file so `tail -f` and file-watching SSE consumers see events at
// emit time, not at the BufWriter's natural cadence. Proof: open an
// independent read handle after each emit (with no `finish`/drop) and verify
// the event is already on disk.
//---------------------------------------------------------------------------
#[test]
fn log_sink_per_event_flush() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (mut sink, _) = open_sink(
        dir.path(),
        "alpha",
        "wx-1",
        1_700_000_000,
        RenderMode::Default,
        false,
    )?;
    let log_path = sink.log_path().to_path_buf();

    sink.emit(&AgentEvent::ToolCall {
        envelope: sample_envelope(),
        id: ToolCallId::new("t1"),
        tool: "Read".to_string(),
        params: json!({"file_path": "first.rs"}),
        parent_tool_call_id: None,
    })?;

    let after_first = fs::read_to_string(&log_path)?;
    let first_lines: Vec<&str> = after_first.lines().collect();
    if first_lines.len() != 1 {
        return Err(anyhow!(
            "expected exactly 1 line flushed mid-stream, got {}: {after_first:?}",
            first_lines.len()
        ));
    }
    let first: Value = serde_json::from_str(first_lines[0])?;
    if first["kind"] != "tool_call" || first["tool"] != "Read" {
        return Err(anyhow!(
            "first flushed line missing tool_call/Read content: {after_first:?}"
        ));
    }

    sink.emit(&AgentEvent::TurnEnd {
        envelope: sample_envelope(),
    })?;

    let after_second = fs::read_to_string(&log_path)?;
    let second_lines: Vec<&str> = after_second.lines().collect();
    if second_lines.len() != 2 {
        return Err(anyhow!(
            "expected 2 lines flushed mid-stream, got {}: {after_second:?}",
            second_lines.len()
        ));
    }
    let second: Value = serde_json::from_str(second_lines[1])?;
    if second["kind"] != "turn_end" {
        return Err(anyhow!(
            "second flushed line missing turn_end content: {after_second:?}"
        ));
    }

    sink.finish(BeadOutcome::Done)?;
    Ok(())
}

//---------------------------------------------------------------------------
// test_run_single_event_sink_property — the LogSink::emit method drives both
// the file writer and the terminal renderer from the same call. There is no
// independent path through which one writer could observe an event the other
// did not. Asserted by emitting events through one method and verifying both
// sinks see the same sequence.
//---------------------------------------------------------------------------
#[test]
fn run_single_event_sink_property() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (mut sink, buf) = open_sink(
        dir.path(),
        "alpha",
        "wx-1",
        1_700_000_000,
        RenderMode::Default,
        false,
    )?;
    let log_path = sink.log_path().to_path_buf();

    let events = [
        AgentEvent::ToolCall {
            envelope: sample_envelope(),
            id: ToolCallId::new("t1"),
            tool: "Read".to_string(),
            params: json!({"file_path": "src/a.rs"}),
            parent_tool_call_id: None,
        },
        AgentEvent::ToolCall {
            envelope: sample_envelope(),
            id: ToolCallId::new("t2"),
            tool: "Edit".to_string(),
            params: json!({"file_path": "src/b.rs"}),
            parent_tool_call_id: None,
        },
    ];
    for e in &events {
        sink.emit(e)?;
    }
    sink.finish(BeadOutcome::Done)?;

    let on_disk = fs::read_to_string(&log_path)?;
    let on_disk_kinds: Vec<String> = on_disk
        .lines()
        .map(|l| -> Result<String> {
            let v: Value = serde_json::from_str(l)?;
            Ok(v["kind"].as_str().unwrap_or("").to_string())
        })
        .collect::<Result<_>>()?;
    let terminal = captured_str(&buf);

    if on_disk_kinds.len() != events.len() {
        return Err(anyhow!(
            "on-disk events ({}) diverged from emit count ({})",
            on_disk_kinds.len(),
            events.len()
        ));
    }
    let term_tool_lines = terminal
        .lines()
        .filter(|l| l.contains("Read") || l.contains("Edit"))
        .count();
    if term_tool_lines != events.len() {
        return Err(anyhow!(
            "terminal tool lines ({term_tool_lines}) diverged from emit count ({}): {terminal:?}",
            events.len()
        ));
    }
    Ok(())
}

//---------------------------------------------------------------------------
// Read + Edit + Bash drive through `LogSink::emit` and the rendered
// terminal carries the per-tool summary cell shape: `Read <path>:range`,
// `Edit <path> +N -M diff↓`, and `Bash <cmd-truncated>`. Plus: a large
// `ToolResult` body in Verbose mode is capped and the `[N more lines —
// loom logs -b … --tool …]` recovery hint shows up.
//---------------------------------------------------------------------------
#[test]
fn run_default_renders_per_tool_summary_cells() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (mut sink, buf) = open_sink(
        dir.path(),
        "alpha",
        "wx-1",
        1_700_000_000,
        RenderMode::Default,
        false,
    )?;

    sink.emit(&AgentEvent::ToolCall {
        envelope: sample_envelope(),
        id: ToolCallId::new("r1"),
        tool: "Read".to_string(),
        params: json!({
            "file_path": "src/lib.rs",
            "offset": 10,
            "limit": 20,
        }),
        parent_tool_call_id: None,
    })?;
    sink.emit(&AgentEvent::ToolCall {
        envelope: sample_envelope(),
        id: ToolCallId::new("e1"),
        tool: "Edit".to_string(),
        params: json!({
            "file_path": "src/lib.rs",
            "old_string": "old line\n",
            "new_string": "new line A\nnew line B\n",
        }),
        parent_tool_call_id: None,
    })?;
    sink.emit(&AgentEvent::ToolCall {
        envelope: sample_envelope(),
        id: ToolCallId::new("b1"),
        tool: "Bash".to_string(),
        params: json!({"command": "cargo build --release --all-features"}),
        parent_tool_call_id: None,
    })?;
    sink.finish(BeadOutcome::Done)?;

    let term = captured_str(&buf);
    if !term.contains("Read") || !term.contains("src/lib.rs:10-30") {
        return Err(anyhow!(
            "Read line missing summary cell shape `Read <path>:<offset-end>`: {term:?}",
        ));
    }
    if !term.contains("Edit") || !term.contains("+2") || !term.contains("-1") {
        return Err(anyhow!(
            "Edit line missing summary cell shape `Edit <path> +N -M diff↓`: {term:?}",
        ));
    }
    if !term.contains("diff") {
        return Err(anyhow!("Edit cell must carry `diff↓` marker: {term:?}"));
    }
    if !term.contains("Bash") || !term.contains("cargo build") {
        return Err(anyhow!(
            "Bash line missing summary cell with truncated cmd: {term:?}",
        ));
    }
    Ok(())
}

//---------------------------------------------------------------------------
// `loom run`'s default mode opens an in-place running indicator for
// top-level tool calls. ToolCall writes `<summary> running...` without a
// newline; the matching ToolResult overwrites with `\r` + clear-to-EOL
// + `<summary> ✓ Ns\n`. Pin this end-to-end through LogSink.
//---------------------------------------------------------------------------
#[test]
fn run_default_indicator_emits_overwrite_pattern_on_tool_result() -> Result<()> {
    use loom_render::in_place::CLEAR_TO_EOL;

    let dir = tempfile::tempdir()?;
    let (mut sink, buf) = open_sink_with_indicator(
        dir.path(),
        "alpha",
        "wx-r3a",
        1_700_000_000,
        RenderMode::Default,
    )?;

    sink.emit(&AgentEvent::ToolCall {
        envelope: sample_envelope(),
        id: ToolCallId::new("b1"),
        tool: "Bash".to_string(),
        params: json!({"command": "cargo test"}),
        parent_tool_call_id: None,
    })?;
    sink.emit(&AgentEvent::ToolResult {
        envelope: sample_envelope(),
        id: ToolCallId::new("b1"),
        output: "ok".to_string(),
        is_error: false,
    })?;
    sink.finish(BeadOutcome::Done)?;

    let term = captured_str(&buf);
    if !term.contains("running...") {
        return Err(anyhow!(
            "indicator must open with `running...` text: {term:?}",
        ));
    }
    if !term.contains('\r') {
        return Err(anyhow!(
            "ToolResult must overwrite running line with `\\r`: {term:?}",
        ));
    }
    if !term.contains(CLEAR_TO_EOL) {
        return Err(anyhow!(
            "ToolResult overwrite must include CLEAR_TO_EOL escape: {term:?}",
        ));
    }
    if !term.contains("✓") {
        return Err(anyhow!(
            "successful ToolResult must finalize with ✓: {term:?}",
        ));
    }
    Ok(())
}

//---------------------------------------------------------------------------
// `test_cancellation_clean_close` — the sink can be finalized mid-tool
// (cancel / panic / signal). Renderer must NOT leave a dangling `\r`
// region — `finish` finalizes any open running line with
// a glyph that matches the outcome, and the captured buffer ends with a
// closing newline so the next renderer (if any) starts on a fresh line.
//---------------------------------------------------------------------------
#[test]
fn run_finish_finalizes_dangling_running_indicator() -> Result<()> {
    use loom_render::in_place::CLEAR_TO_EOL;

    let dir = tempfile::tempdir()?;
    let (mut sink, buf) = open_sink_with_indicator(
        dir.path(),
        "alpha",
        "wx-r3b",
        1_700_000_000,
        RenderMode::Default,
    )?;

    sink.emit(&AgentEvent::ToolCall {
        envelope: sample_envelope(),
        id: ToolCallId::new("b1"),
        tool: "Bash".to_string(),
        params: json!({"command": "sleep 60"}),
        parent_tool_call_id: None,
    })?;
    // No ToolResult arrives — simulate cancellation. The bead is being
    // closed mid-tool: `finish` must clean up the indicator region.
    sink.finish(BeadOutcome::Failed)?;

    let term = captured_str(&buf);
    if !term.ends_with('\n') {
        return Err(anyhow!(
            "captured buffer must end with `\\n` so the next renderer starts on a fresh \
             line — got: {term:?}",
        ));
    }
    if !term.contains(CLEAR_TO_EOL) {
        return Err(anyhow!(
            "indicator cleanup must emit CLEAR_TO_EOL before the closing line: {term:?}",
        ));
    }
    // The finish line marks the bead as failed; the running indicator's
    // own finalization glyph (✗) appears on its own line above.
    if !term.contains("failed") {
        return Err(anyhow!(
            "bead closing line must read `failed` on cancelled finish: {term:?}",
        ));
    }
    if !term.contains("✗") {
        return Err(anyhow!(
            "cancelled tool's running line must finalize with ✗: {term:?}",
        ));
    }
    Ok(())
}

#[test]
fn run_verbose_caps_tool_result_body_with_recovery_hint() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (mut sink, buf) = open_sink(
        dir.path(),
        "alpha",
        "wx-2",
        1_700_000_000,
        RenderMode::Verbose,
        false,
    )?;

    sink.emit(&AgentEvent::ToolCall {
        envelope: sample_envelope(),
        id: ToolCallId::new("b1"),
        tool: "Bash".to_string(),
        params: json!({"command": "ls -la"}),
        parent_tool_call_id: None,
    })?;
    let big_output: String = (1..=15)
        .map(|i| format!("file-{i:03}"))
        .collect::<Vec<_>>()
        .join("\n");
    sink.emit(&AgentEvent::ToolResult {
        envelope: sample_envelope(),
        id: ToolCallId::new("b1"),
        output: big_output,
        is_error: false,
    })?;
    sink.finish(BeadOutcome::Done)?;

    let term = captured_str(&buf);
    if !term.contains("file-001") || !term.contains("file-010") {
        return Err(anyhow!("body cap dropped early lines: {term:?}"));
    }
    if term.contains("file-011") {
        return Err(anyhow!("body cap should drop line 11+: {term:?}"));
    }
    if !term.contains("5 more lines") {
        return Err(anyhow!("missing `<N> more lines` recovery hint: {term:?}"));
    }
    if !term.contains("loom logs -b wx-2 --tool b1") {
        return Err(anyhow!(
            "recovery hint must reference bead id + tool call id: {term:?}",
        ));
    }
    Ok(())
}

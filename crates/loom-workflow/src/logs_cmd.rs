//! `loom logs` — locate and replay per-bead JSONL logs.
//!
//! Read-only, no lock acquired (per the lock matrix in
//! `specs/harness.md`). [`select_log`] walks
//! `<workspace>/.loom/logs/` for `*.jsonl` files and returns the
//! path with the largest mtime; with `--bead <id>` set, only files whose
//! stem starts with `<id>-` are considered.
//!
//! [`replay`] reads a selected log, parses each line into an
//! [`AgentEvent`], and drives the same [`loom_render::Renderer`] impls
//! `loom loop` uses for live output — so the spec criterion "logs and
//! run share a single renderer" is enforced by the type, not by
//! convention. `--raw` skips parsing and copies the file's bytes
//! verbatim; `--follow` polls for additional bytes / lines after EOF.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use displaydoc::Display;
use thiserror::Error;

use loom_driver::clock::Clock;
use loom_driver::identifier::BeadId;
use loom_events::AgentEvent;
use loom_render::{BeadOutcome, RenderMode, build_renderer_with_tool_body_limit};

const LOG_EXTENSION: &str = "jsonl";

/// Default polling interval when [`ReplayOpts::follow_poll`] is unset.
const DEFAULT_FOLLOW_POLL: Duration = Duration::from_millis(100);

/// Options for [`select_log`].
#[derive(Debug, Clone, Default)]
pub struct LogsOpts<'a> {
    /// Restrict the search to files belonging to this bead. When `None`,
    /// the most recent log across every bead in every spec is returned.
    pub bead: Option<&'a BeadId>,
}

/// Failures raised by [`select_log`] and [`replay`].
#[derive(Debug, Display, Error)]
pub enum LogsError {
    /// io failure while walking logs directory
    Io(#[from] std::io::Error),

    /// no logs found under {root}
    NoLogs { root: PathBuf },

    /// no logs found for bead {bead} under {root}
    NoLogsForBead { bead: String, root: PathBuf },

    /// failed to parse log line {line_no} in {path}: {source}
    ParseLine {
        path: PathBuf,
        line_no: usize,
        #[source]
        source: serde_json::Error,
    },
}

/// Walk `logs_root` (typically `<workspace>/.loom/logs/`) and return
/// the most recent `*.jsonl` log. The traversal is two levels deep —
/// `<root>/<spec-label>/<bead-id>-<utc>.jsonl` per the path layout in
/// `specs/harness.md` *Run UX & Logging*.
pub fn select_log(logs_root: &Path, opts: LogsOpts<'_>) -> Result<PathBuf, LogsError> {
    let bead_filter = opts.bead.map(|b| b.as_str().to_string());
    let mut candidates: Vec<(SystemTime, PathBuf)> = Vec::new();
    if !logs_root.exists() {
        return missing(logs_root, opts.bead);
    }
    for spec_entry in std::fs::read_dir(logs_root)? {
        let spec_entry = spec_entry?;
        let spec_path = spec_entry.path();
        if !spec_path.is_dir() {
            continue;
        }
        for bead_entry in std::fs::read_dir(&spec_path)? {
            let bead_entry = bead_entry?;
            let path = bead_entry.path();
            if !path.is_file() {
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some(LOG_EXTENSION) {
                continue;
            }
            if let Some(prefix) = &bead_filter
                && !file_stem_belongs_to(&path, prefix)
            {
                continue;
            }
            let mtime = bead_entry
                .metadata()?
                .modified()
                .unwrap_or(SystemTime::UNIX_EPOCH);
            candidates.push((mtime, path));
        }
    }
    candidates.sort_by_key(|c| std::cmp::Reverse(c.0));
    match candidates.into_iter().next() {
        Some((_, path)) => Ok(path),
        None => missing(logs_root, opts.bead),
    }
}

fn file_stem_belongs_to(path: &Path, bead: &str) -> bool {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    // Stems look like `<bead>-<utc>` per `bead_log_path`. Match the prefix
    // exactly so `lm-1` does not also match `lm-10`.
    stem == bead || stem.starts_with(&format!("{bead}-"))
}

fn missing(root: &Path, bead: Option<&BeadId>) -> Result<PathBuf, LogsError> {
    match bead {
        Some(b) => Err(LogsError::NoLogsForBead {
            bead: b.to_string(),
            root: root.to_path_buf(),
        }),
        None => Err(LogsError::NoLogs {
            root: root.to_path_buf(),
        }),
    }
}

/// Replay mode dispatched by [`replay`]. `Render(_)` parses each JSONL
/// line into an [`AgentEvent`] and drives a [`Renderer`]; `Raw` copies
/// the file's bytes verbatim without parsing.
#[derive(Debug, Clone, Copy)]
pub enum ReplayMode {
    /// Parse + render through a `Box<dyn Renderer>`. The wrapped
    /// [`RenderMode`] picks between Pretty/Plain/Verbose etc. — the
    /// same enum `loom loop` consumes.
    Render(RenderMode),
    /// Copy bytes from the file verbatim (no JSON parsing). Drives the
    /// `--raw` path; pairs with `follow=true` to tail raw JSONL.
    Raw,
}

/// Parameters for [`replay`].
pub struct ReplayOpts<'a> {
    /// Path to the JSONL log file.
    pub path: &'a Path,
    /// Bead id used to seed the renderer (header line, indicator
    /// attribution). For replay/`Raw` it is informational only.
    pub bead_id: BeadId,
    /// Render-vs-raw + format dimension.
    pub mode: ReplayMode,
    /// Tail mode — block on EOF rather than return.
    pub follow: bool,
    /// Polling interval when `follow=true`. Defaults to
    /// [`DEFAULT_FOLLOW_POLL`] when `None`.
    pub follow_poll: Option<Duration>,
    /// Optional poll-iteration cap for `follow`. Tests set this so the
    /// tail loop returns deterministically (after `N` sleep cycles);
    /// production calls pass `None` (block until the process is
    /// interrupted). Counted as the number of post-EOF sleep cycles,
    /// not the number of bytes consumed.
    pub follow_max_polls: Option<u32>,
}

/// Replay (and optionally tail) `opts.path` into `writer`.
///
/// In `Render` mode each non-empty line is deserialized to an
/// [`AgentEvent`] and forwarded to the renderer chosen by
/// [`loom_render::build_renderer`] — the same code path `loom loop`
/// uses. In `Raw` mode bytes are copied verbatim without parsing.
///
/// `follow=true` waits for the file to grow after the initial read.
/// The poll loop honors `follow_max_polls` for tests; production sets
/// `None` so the call blocks until the process is interrupted.
pub async fn replay(
    opts: ReplayOpts<'_>,
    writer: Box<dyn Write + Send>,
    clock: Arc<dyn Clock>,
) -> Result<(), LogsError> {
    replay_with_tool_body_limit(opts, writer, clock, loom_render::tool_body::BODY_CAP_BYTES).await
}

pub async fn replay_with_tool_body_limit(
    opts: ReplayOpts<'_>,
    writer: Box<dyn Write + Send>,
    clock: Arc<dyn Clock>,
    max_inline_bytes: usize,
) -> Result<(), LogsError> {
    match opts.mode {
        ReplayMode::Render(mode) => {
            replay_rendered(opts, mode, writer, clock, max_inline_bytes).await
        }
        ReplayMode::Raw => replay_raw(opts, writer, clock).await,
    }
}

async fn replay_rendered(
    opts: ReplayOpts<'_>,
    mode: RenderMode,
    writer: Box<dyn Write + Send>,
    clock: Arc<dyn Clock>,
    max_inline_bytes: usize,
) -> Result<(), LogsError> {
    let mut renderer = build_renderer_with_tool_body_limit(
        mode,
        writer,
        opts.bead_id.clone(),
        false,
        false,
        max_inline_bytes,
    );
    let file = File::open(opts.path)?;
    let mut reader = BufReader::new(file);
    let mut line_no = 0_usize;
    let mut first_ts_ms: Option<i64> = None;
    let mut last_ts_ms: Option<i64> = None;
    let poll = opts.follow_poll.unwrap_or(DEFAULT_FOLLOW_POLL);
    let mut polls_remaining = opts.follow_max_polls;
    loop {
        drain_lines(&mut reader, opts.path, &mut line_no, &mut |event| {
            if first_ts_ms.is_none() {
                first_ts_ms = Some(event_ts(event));
            }
            last_ts_ms = Some(event_ts(event));
            renderer.render_event(event)
        })?;
        if !opts.follow {
            break;
        }
        if let Some(remaining) = polls_remaining.as_mut() {
            if *remaining == 0 {
                break;
            }
            *remaining -= 1;
        }
        clock.sleep(poll).await;
    }
    let elapsed = match (first_ts_ms, last_ts_ms) {
        (Some(a), Some(b)) if b >= a => Duration::from_millis((b - a) as u64),
        _ => Duration::ZERO,
    };
    renderer
        .finish(BeadOutcome::Done, elapsed)
        .map_err(LogsError::Io)?;
    Ok(())
}

async fn replay_raw(
    opts: ReplayOpts<'_>,
    mut writer: Box<dyn Write + Send>,
    clock: Arc<dyn Clock>,
) -> Result<(), LogsError> {
    let mut file = File::open(opts.path)?;
    let mut buf = [0_u8; 8192];
    let poll = opts.follow_poll.unwrap_or(DEFAULT_FOLLOW_POLL);
    let mut polls_remaining = opts.follow_max_polls;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            if !opts.follow {
                break;
            }
            if let Some(remaining) = polls_remaining.as_mut() {
                if *remaining == 0 {
                    break;
                }
                *remaining -= 1;
            }
            clock.sleep(poll).await;
            // Re-anchoring to the current position via `Seek::Current(0)`
            // ensures any caller-side metadata sync is observed before
            // the next `read`; the call is a no-op on most platforms.
            let _ = file.stream_position()?;
            continue;
        }
        writer.write_all(&buf[..n])?;
        writer.flush()?;
    }
    Ok(())
}

fn drain_lines<F>(
    reader: &mut BufReader<File>,
    path: &Path,
    line_no: &mut usize,
    on_event: &mut F,
) -> Result<usize, LogsError>
where
    F: FnMut(&AgentEvent) -> std::io::Result<()>,
{
    let mut consumed = 0_usize;
    let mut buf = String::new();
    loop {
        buf.clear();
        let n = reader.read_line(&mut buf)?;
        if n == 0 {
            return Ok(consumed);
        }
        // Strip newline + optional trailing CR. Empty lines are
        // skipped silently so a trailing blank line at EOF doesn't
        // cause a parse error.
        let trimmed = buf.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            *line_no += 1;
            continue;
        }
        *line_no += 1;
        let event: AgentEvent =
            serde_json::from_str(trimmed).map_err(|source| LogsError::ParseLine {
                path: path.to_path_buf(),
                line_no: *line_no,
                source,
            })?;
        on_event(&event).map_err(LogsError::Io)?;
        consumed += 1;
    }
}

fn event_ts(event: &AgentEvent) -> i64 {
    event.envelope().ts_ms
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use loom_events::EventEnvelope;
    use loom_events::Source;
    use loom_events::identifier::{SessionId, ToolCallId};
    use serde_json::json;
    use std::io;
    use std::sync::{Arc, Mutex};

    /// Fixture envelope for `loom logs` replay tests. Bead id `lm-1`
    /// matches the path-builder fixtures so the replay code paths see
    /// the expected on-disk shape.
    fn sample_envelope() -> EventEnvelope {
        EventEnvelope {
            session_id: SessionId::new("sess-logs"),
            bead_id: Some(BeadId::new("lm-1").expect("valid bead id")),
            molecule_id: None,
            iteration: Some(0),
            source: Source::Agent,
            ts_ms: 0,
            seq: 0,
        }
    }

    /// Pinned reference time used by mtime-driven tests so they don't read
    /// wall clock — matches the rest of the loom test suite, which routes
    /// every wall-time read through the `SystemClock` impl.
    fn reference_now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    fn touch(path: &Path, mtime: SystemTime) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, b"event\n")?;
        let f = std::fs::File::options().write(true).open(path)?;
        f.set_modified(mtime)?;
        Ok(())
    }

    #[test]
    fn empty_root_returns_no_logs() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let err = select_log(&dir.path().join(".loom/logs"), LogsOpts::default())
            .err()
            .ok_or_else(|| anyhow::anyhow!("expected error"))?;
        assert!(matches!(err, LogsError::NoLogs { .. }));
        Ok(())
    }

    #[test]
    fn returns_most_recent_log_across_specs() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join(".loom/logs");
        let older = reference_now() - Duration::from_secs(120);
        touch(&root.join("alpha/lm-1-old.jsonl"), older)?;
        touch(&root.join("beta/lm-2-newer.jsonl"), reference_now())?;
        let path = select_log(&root, LogsOpts::default())?;
        assert!(path.ends_with("lm-2-newer.jsonl"), "{path:?}");
        Ok(())
    }

    #[test]
    fn bead_filter_matches_prefix_exactly() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join(".loom/logs");
        // lm-1 and lm-10 are distinct beads — the filter must not collapse
        // them.
        touch(&root.join("alpha/lm-10-newer.jsonl"), reference_now())?;
        touch(
            &root.join("alpha/lm-1-older.jsonl"),
            reference_now() - Duration::from_secs(60),
        )?;
        let path = select_log(
            &root,
            LogsOpts {
                bead: Some(&BeadId::new("lm-1")?),
            },
        )?;
        assert!(path.ends_with("lm-1-older.jsonl"), "{path:?}");
        Ok(())
    }

    #[test]
    fn missing_bead_filter_returns_typed_error() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join(".loom/logs");
        touch(&root.join("alpha/lm-1-x.jsonl"), reference_now())?;
        let err = select_log(
            &root,
            LogsOpts {
                bead: Some(&BeadId::new("lm-2")?),
            },
        )
        .err()
        .ok_or_else(|| anyhow::anyhow!("expected error"))?;
        assert!(matches!(err, LogsError::NoLogsForBead { .. }));
        Ok(())
    }

    #[test]
    fn ignores_non_jsonl_files() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join(".loom/logs");
        touch(&root.join("alpha/lm-1-x.txt"), reference_now())?;
        let err = select_log(&root, LogsOpts::default())
            .err()
            .ok_or_else(|| anyhow::anyhow!("expected error"))?;
        assert!(matches!(err, LogsError::NoLogs { .. }));
        Ok(())
    }

    fn shared_buf() -> (Arc<Mutex<Vec<u8>>>, Box<dyn Write + Send>) {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        struct Sink(Arc<Mutex<Vec<u8>>>);
        impl Write for Sink {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                self.0
                    .lock()
                    .map_err(|_| io::Error::other("poisoned"))?
                    .extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let sink: Box<dyn Write + Send> = Box::new(Sink(buf.clone()));
        (buf, sink)
    }

    fn write_jsonl(path: &Path, events: &[AgentEvent]) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut body = String::new();
        for ev in events {
            body.push_str(&serde_json::to_string(ev)?);
            body.push('\n');
        }
        std::fs::write(path, body.as_bytes())?;
        Ok(())
    }

    fn sample_tool_pair() -> Vec<AgentEvent> {
        let mut call_env = sample_envelope();
        call_env.ts_ms = 1_000;
        let mut result_env = sample_envelope();
        result_env.ts_ms = 4_000;
        vec![
            AgentEvent::ToolCall {
                envelope: call_env,
                id: ToolCallId::new("t1"),
                tool: "Bash".into(),
                params: json!({"command": "echo hi"}),
                parent_tool_call_id: None,
            },
            AgentEvent::ToolResult {
                envelope: result_env,
                id: ToolCallId::new("t1"),
                output: "hi".into(),
                is_error: false,
            },
        ]
    }

    /// `replay` builds the renderer via `loom_render::build_renderer` —
    /// the same selection path `loom loop` uses. The rendered output
    /// shows the tool summary plus a duration computed from the
    /// events' `ts_ms` deltas, which is the marker for the
    /// `loom logs` ↔ `loom loop` shared-renderer contract.
    #[tokio::test(start_paused = true)]
    async fn replay_renders_via_shared_renderer() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("alpha/lm-1-x.jsonl");
        write_jsonl(&path, &sample_tool_pair())?;
        let (buf, sink) = shared_buf();
        replay(
            ReplayOpts {
                path: &path,
                bead_id: BeadId::new("lm-1")?,
                mode: ReplayMode::Render(RenderMode::Plain),
                follow: false,
                follow_poll: None,
                follow_max_polls: None,
            },
            sink,
            Arc::new(loom_driver::clock::MockClock::new()),
        )
        .await?;
        let out = String::from_utf8(buf.lock().map_err(|_| anyhow::anyhow!("poisoned"))?.clone())?;
        assert!(out.contains("Bash"), "{out:?}");
        // ts_ms delta 3s → "3.0s" pair duration via the renderer.
        assert!(out.contains("3.0s"), "{out:?}");
        Ok(())
    }

    /// `--raw` mode copies file bytes verbatim. Compact JSON object
    /// per line, no pretty-printing, no closing renderer chrome.
    #[tokio::test(start_paused = true)]
    async fn replay_raw_copies_bytes_verbatim() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("alpha/lm-1-x.jsonl");
        let body = "{\"kind\":\"turn_end\",\"session_id\":\"sess-logs\",\"bead_id\":\"lm-1\",\"molecule_id\":null,\"iteration\":0,\"source\":\"agent\",\"ts_ms\":0,\"seq\":0}\n";
        std::fs::create_dir_all(path.parent().ok_or_else(|| anyhow::anyhow!("parent"))?)?;
        std::fs::write(&path, body.as_bytes())?;
        let (buf, sink) = shared_buf();
        replay(
            ReplayOpts {
                path: &path,
                bead_id: BeadId::new("lm-1")?,
                mode: ReplayMode::Raw,
                follow: false,
                follow_poll: None,
                follow_max_polls: None,
            },
            sink,
            Arc::new(loom_driver::clock::MockClock::new()),
        )
        .await?;
        let out = String::from_utf8(buf.lock().map_err(|_| anyhow::anyhow!("poisoned"))?.clone())?;
        assert_eq!(out, body);
        Ok(())
    }

    /// `Render(Verbose)` streams `TextDelta` text verbatim — pin the
    /// shape so `-v` keeps that wiring.
    #[tokio::test(start_paused = true)]
    async fn replay_verbose_streams_text_deltas() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("alpha/lm-1-x.jsonl");
        let events = vec![
            AgentEvent::TextDelta {
                envelope: sample_envelope(),
                text: "hel".into(),
            },
            AgentEvent::TextDelta {
                envelope: sample_envelope(),
                text: "lo".into(),
            },
        ];
        write_jsonl(&path, &events)?;
        let (buf, sink) = shared_buf();
        replay(
            ReplayOpts {
                path: &path,
                bead_id: BeadId::new("lm-1")?,
                mode: ReplayMode::Render(RenderMode::Verbose),
                follow: false,
                follow_poll: None,
                follow_max_polls: None,
            },
            sink,
            Arc::new(loom_driver::clock::MockClock::new()),
        )
        .await?;
        let out = String::from_utf8(buf.lock().map_err(|_| anyhow::anyhow!("poisoned"))?.clone())?;
        assert!(out.contains("hello"), "{out:?}");
        Ok(())
    }

    /// `follow=true` keeps the read loop alive past EOF until the
    /// poll budget runs out. This pins the "blocks on EOF" behavior
    /// the `-f` flag promises: a non-following call would return
    /// immediately, but with `follow_max_polls` set the loop sleeps
    /// for each post-EOF poll before returning.
    #[tokio::test(start_paused = true)]
    async fn follow_blocks_past_eof_until_budget_expires() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("alpha/lm-1-x.jsonl");
        write_jsonl(&path, &sample_tool_pair())?;
        let (_buf, sink) = shared_buf();
        let clock: Arc<dyn Clock> = Arc::new(loom_driver::clock::MockClock::new());
        let started = clock.now();
        replay(
            ReplayOpts {
                path: &path,
                bead_id: BeadId::new("lm-1")?,
                mode: ReplayMode::Render(RenderMode::Plain),
                follow: true,
                follow_poll: Some(Duration::from_millis(20)),
                follow_max_polls: Some(3),
            },
            sink,
            Arc::clone(&clock),
        )
        .await?;
        let elapsed = clock.now().duration_since(started);
        assert!(
            elapsed >= Duration::from_millis(60),
            "follow returned too quickly: {elapsed:?}",
        );
        Ok(())
    }

    /// `-f --raw` composes — the raw byte loop also polls past EOF.
    /// Same budget contract as the rendered path.
    #[tokio::test(start_paused = true)]
    async fn follow_raw_blocks_past_eof_until_budget_expires() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("alpha/lm-1-x.jsonl");
        std::fs::create_dir_all(path.parent().ok_or_else(|| anyhow::anyhow!("parent"))?)?;
        std::fs::write(&path, b"line1\n")?;
        let (buf, sink) = shared_buf();
        let clock: Arc<dyn Clock> = Arc::new(loom_driver::clock::MockClock::new());
        let started = clock.now();
        replay(
            ReplayOpts {
                path: &path,
                bead_id: BeadId::new("lm-1")?,
                mode: ReplayMode::Raw,
                follow: true,
                follow_poll: Some(Duration::from_millis(20)),
                follow_max_polls: Some(3),
            },
            sink,
            Arc::clone(&clock),
        )
        .await?;
        let out = String::from_utf8(buf.lock().map_err(|_| anyhow::anyhow!("poisoned"))?.clone())?;
        assert_eq!(out, "line1\n");
        assert!(clock.now().duration_since(started) >= Duration::from_millis(60));
        Ok(())
    }
}

use std::io::{self, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::clock::{Clock, SystemClock};
use crate::in_place::CLEAR_TO_EOL;
use crate::tool_body;
use loom_events::identifier::{BeadId, ProfileName, ToolCallId};
use loom_events::{AgentEvent, DriverKind};

/// Final outcome of a bead spawn — drives the closing line color and glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeadOutcome {
    /// Agent finished cleanly; closing line uses green ✓.
    Done,
    /// Agent failed; closing line uses red ✗.
    Failed,
    /// Run is being retried with previous-error context; line uses yellow.
    Retry,
}

/// Output mode for the renderer trait. Drives the per-impl selection
/// in [`select_renderer`] and the CLI flag wiring in `loom run` /
/// `loom logs`. Mode is the format dimension; verbosity is orthogonal.
///
/// Selection logic:
/// - TTY + no `--plain`/`--json`/`--raw` + no `NO_COLOR` → `Pretty`
/// - non-TTY OR `NO_COLOR` set OR `--plain` → `Plain`
/// - `--json` → `Json`
/// - `--raw` → `Raw`
///
/// `--json` and `--raw` are mutually exclusive; `--raw` is mutually
/// exclusive with `-v/--verbose`. The CLI surface enforces these via
/// clap `conflicts_with`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    /// Internal "verbose pretty" — kept so the existing per-bead
    /// `TerminalRenderer` path stays compatible. New code should
    /// consume the [`Renderer`] trait, not match on this.
    Default,
    /// Internal "verbose pretty" — same caveat.
    Verbose,
    /// Colored, glyph-decorated output for an interactive terminal.
    Pretty,
    /// ASCII glyphs, no color, no OSC 8 — pipe-safe.
    Plain,
    /// One pretty-printed JSON object per line; pure data, no chrome.
    Json,
    /// Passthrough JSONL: each event serialized as one compact JSON
    /// line. Used by `loom logs --raw` to reproduce the on-disk shape.
    Raw,
}

impl RenderMode {
    /// Auto-select the right mode for a given (TTY, flags, env) state.
    /// Pure function — every input is a parameter so tests can pin
    /// behavior without env mutation.
    pub fn select(
        tty: bool,
        no_color_env: bool,
        flag_plain: bool,
        flag_json: bool,
        flag_raw: bool,
    ) -> RenderMode {
        if flag_raw {
            RenderMode::Raw
        } else if flag_json {
            RenderMode::Json
        } else if flag_plain || !tty || no_color_env {
            RenderMode::Plain
        } else {
            RenderMode::Pretty
        }
    }
}

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_RED: &str = "\x1b[31m";
const ANSI_YELLOW: &str = "\x1b[33m";

/// Three-space gap between left-edge summary content and the right-edge
/// duration / status column on a single-line summary cell.
const SUMMARY_SEPARATOR: &str = "   ";

/// Worst-case display width reserved for the right-edge column when
/// deciding whether a single-line summary cell fits. Covers the longest
/// in-flight indicator text we ever emit (`"running... 999.9s"` = 17
/// columns) so an indicator that opens single-line stays single-line
/// for the entire duration without wrapping at the terminal edge.
const RIGHT_COL_RESERVE: usize = 17;

/// Fallback terminal width used when `crossterm::terminal::size` fails
/// or returns 0 — both legitimate "no real width" states (pipes, CI
/// runners without a pty). 80 keeps the math sensible without aborting
/// rendering.
const FALLBACK_TERM_WIDTH: usize = 80;

/// Convert a `ts_ms` delta (signed millis) to a non-negative
/// [`Duration`]. Negative deltas (clock skew across producers, malformed
/// logs) saturate to zero so the renderer never panics on `from_millis`.
fn ts_ms_delta(start: i64, end: i64) -> Duration {
    let delta = end.saturating_sub(start);
    Duration::from_millis(u64::try_from(delta).unwrap_or(0))
}

/// Detect terminal width via crossterm. Returns the column count of the
/// underlying tty or [`FALLBACK_TERM_WIDTH`] when no real width is
/// available.
fn detect_term_width() -> usize {
    crossterm::terminal::size()
        .ok()
        .and_then(|(cols, _)| (cols > 0).then_some(usize::from(cols)))
        .unwrap_or(FALLBACK_TERM_WIDTH)
}

/// Visible width of `s` in terminal columns. Skips ANSI CSI escapes
/// (`\x1b[...<alpha>`) and OSC 8 hyperlink envelopes
/// (`\x1b]8;;url\x07display\x1b]8;;\x07`) so the count reflects what the
/// user sees, not the raw byte length. Multi-column characters are
/// counted as one column — sufficient for the ASCII-and-symbol summary
/// cells the renderer produces today.
fn display_width(s: &str) -> usize {
    let mut width = 0usize;
    let mut iter = s.chars().peekable();
    while let Some(c) = iter.next() {
        match c {
            '\x1b' => match iter.peek() {
                Some(']') => {
                    iter.next();
                    while let Some(c2) = iter.next() {
                        if c2 == '\x07' {
                            break;
                        }
                        if c2 == '\x1b' && iter.peek() == Some(&'\\') {
                            iter.next();
                            break;
                        }
                    }
                }
                Some('[') => {
                    iter.next();
                    for c2 in iter.by_ref() {
                        if c2.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\x07' => {}
            _ => width += 1,
        }
    }
    width
}

/// State captured at `ToolCall` time so the matching `ToolResult` can
/// finalize the pair with the same context. The spec calls this out
/// explicitly as the `HashMap<ToolCallId, PendingToolCall>` that lets a
/// `tool_call` + `tool_result` collapse into one rendered block with a
/// duration computed from `envelope.ts_ms` deltas.
#[derive(Debug, Clone)]
struct PendingToolCall {
    /// Builtin tool name (e.g. `"Bash"`, `"Edit"`). Drives per-tool body
    /// rendering policy at `ToolResult` time — the spec's per-tool body
    /// table varies the default-mode body by tool (e.g. Bash hides on
    /// `exit == 0` but renders the first 10 lines on `exit != 0`).
    tool: String,
    #[expect(dead_code, reason = "reserved for per-tool verbose body formatting")]
    params: Value,
    /// `envelope.ts_ms` of the originating `ToolCall`. Used to compute
    /// the pair's duration on `ToolResult` without consulting the wall
    /// clock — same code path for live and replay.
    ts_ms: i64,
}

/// Per-bead terminal renderer.
///
/// One renderer is constructed per bead spawn. The driver creates it via
/// [`TerminalRenderer::new`], emits the header, drives [`render_event`] for
/// every [`AgentEvent`], and finally emits the closing line via
/// [`finish`]. With `--parallel N > 1`, the renderer is constructed in
/// `Parallel` mode so tool-call lines carry a `[bead-id]` prefix for
/// attribution.
pub struct TerminalRenderer {
    out: Box<dyn Write + Send>,
    mode: RenderMode,
    bead_id: BeadId,
    parallel: bool,
    color: bool,
    clock: Arc<dyn Clock>,
    started: Instant,
    tool_count: u32,
    /// Per-tool-call nesting depth. Populated when a `ToolCall` arrives
    /// — depth = parent's depth + 1 when `parent_tool_call_id` is set,
    /// 0 otherwise. The renderer indents the rendered line by
    /// `depth * 2` spaces. Cleared lazily; entries only matter while
    /// the corresponding tool is in flight.
    indent_by_tool: std::collections::HashMap<loom_events::identifier::ToolCallId, usize>,
    /// Per-tool-call state captured at `ToolCall` time so the matching
    /// `ToolResult` can finalize the pair. Stores `(tool, params,
    /// ts_ms)` — params drives `cap_body` formatting in verbose mode,
    /// `ts_ms` lets the result line render its duration from the event
    /// envelope rather than wall clock.
    pending_calls: std::collections::HashMap<loom_events::identifier::ToolCallId, PendingToolCall>,
    /// In-place "running indicator" state for the top-level tool call
    /// currently in-flight. When `Some((id, started, summary, indent))`,
    /// a `\r`-overwritable line has been written for that call and the
    /// matching `ToolResult` should finalize it.
    /// Any other event must clear/finalize the line first so it does
    /// not interleave with running-state text.
    running: Option<(ToolCallId, Instant, String, String)>,
    /// Top-level call whose summary cell would not fit on the same line
    /// as the right-edge column at the current terminal width. The
    /// renderer suppresses the in-place indicator for the call entirely
    /// — emitting nothing at `ToolCall` time — and writes a two-line
    /// layout (right-edge column right-aligned on line 1, full summary
    /// on a body-indented continuation line on line 2) when the matching
    /// `ToolResult` arrives. Mutually exclusive with [`running`]: each
    /// top-level call lands in one bucket or the other.
    buffered_overflow: Option<(ToolCallId, Instant, String, String)>,
    /// `true` for `loom run` (events arrive in real time) and `false`
    /// for `loom logs` replay. Replay mode suppresses the in-place
    /// running indicator entirely — every event already has its
    /// matching `ToolResult` on disk so the spinner has nothing to
    /// represent — and pair durations come from `ts_ms` deltas instead
    /// of the local clock.
    live: bool,
    /// `true` when the renderer should drive the in-place running
    /// indicator. Disabled in `Plain`/`Json`/`Raw` modes, when running
    /// in parallel (multiple `\r` regions don't compose), and when
    /// the writer is known to be non-TTY. The CLI surface decides this
    /// and wires it through `RenderMode::select`.
    indicator_enabled: bool,
    /// OSC 8 hyperlink wrapping for path-bearing summary cells.
    /// Default is disabled; callers in supported terminals enable via
    /// [`TerminalRenderer::with_osc8`].
    osc8: tool_body::Osc8Context,
    /// Override for the detected terminal width. Tests inject a fixed
    /// width via [`TerminalRenderer::with_term_width`] so the overflow
    /// behavior pins deterministically without a real terminal. `None`
    /// in production — each emission queries crossterm so SIGWINCH
    /// resizes take effect immediately.
    term_width_override: Option<usize>,
    header_printed: bool,
    closed: bool,
}

impl TerminalRenderer {
    /// Build a renderer that writes to `out` using a [`SystemClock`] for the
    /// elapsed-time line at finish. Defaults to `live = true` — events
    /// stream from a running agent. Replay callers (`loom logs`) use
    /// [`TerminalRenderer::new_replay`] to flip the `live` bool.
    pub fn new(
        out: impl Write + Send + 'static,
        mode: RenderMode,
        bead_id: BeadId,
        parallel: bool,
        color: bool,
    ) -> Self {
        Self::with_clock(
            out,
            mode,
            bead_id,
            parallel,
            color,
            true,
            SystemClock::new(),
        )
    }

    /// Build a renderer in replay mode — events are read from a saved
    /// JSONL log rather than a running agent, so the in-place running
    /// indicator is suppressed and pair durations come from `ts_ms`
    /// deltas. The "live: bool" parameter lives here.
    pub fn new_replay(
        out: impl Write + Send + 'static,
        mode: RenderMode,
        bead_id: BeadId,
        parallel: bool,
        color: bool,
    ) -> Self {
        Self::with_clock(
            out,
            mode,
            bead_id,
            parallel,
            color,
            false,
            SystemClock::new(),
        )
    }

    /// Build a renderer with an explicit clock. Tests inject
    /// [`crate::clock::MockClock`] so the elapsed-time output is deterministic
    /// without wall-clock dependence.
    pub fn with_clock(
        out: impl Write + Send + 'static,
        mode: RenderMode,
        bead_id: BeadId,
        parallel: bool,
        color: bool,
        live: bool,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let started = clock.now();
        Self {
            out: Box::new(out),
            mode,
            bead_id,
            parallel,
            color,
            clock,
            started,
            tool_count: 0,
            indent_by_tool: std::collections::HashMap::new(),
            pending_calls: std::collections::HashMap::new(),
            running: None,
            buffered_overflow: None,
            // Indicator is enabled by default when color is on, we're
            // not in parallel mode, and events arrive live. Each
            // condition is a proxy for a real constraint: color → TTY,
            // !parallel → single \r-region on screen, live → there's
            // something to spin while we wait for. Tests pass
            // `color=false` to disable the indicator so captured-buffer
            // assertions stay stable.
            indicator_enabled: color && !parallel && live,
            live,
            osc8: tool_body::Osc8Context::disabled(),
            term_width_override: None,
            header_printed: false,
            closed: false,
        }
    }

    /// Pin the terminal width used for summary-cell overflow decisions.
    /// Production callers omit this — the renderer queries crossterm on
    /// every emission so the layout adapts to SIGWINCH. Tests use it to
    /// reproduce a narrow terminal (e.g. 40 columns) without spawning a
    /// real pty.
    pub fn with_term_width(mut self, width: usize) -> Self {
        self.term_width_override = Some(width);
        self
    }

    /// Resolve the terminal width for overflow decisions. Honors the
    /// test override when set; otherwise queries crossterm with the
    /// 80-column fallback for non-TTY paths.
    fn term_width(&self) -> usize {
        self.term_width_override.unwrap_or_else(detect_term_width)
    }

    /// Line prefix shared by every tool-summary line. `Parallel` mode
    /// adds the bead-id bracket for cross-bead attribution; otherwise
    /// just the two-space base indent.
    fn prefix_str(&self) -> String {
        if self.parallel {
            format!("  [{}] ", self.bead_id.as_str())
        } else {
            "  ".to_string()
        }
    }

    /// Enable OSC 8 hyperlink wrapping for path-bearing summary cells.
    /// Production callers in `loom run` probe `TERM_PROGRAM` / `TERM`
    /// via [`crate::osc8::supports_osc8`] and pass the workspace root
    /// as `cwd`. Tests pin both branches without env mutation by
    /// passing the boolean directly. Chainable; returns the renderer.
    pub fn with_osc8(mut self, ctx: tool_body::Osc8Context) -> Self {
        self.osc8 = ctx;
        self
    }

    /// Print the per-bead header line.
    ///
    /// `▸ <bead-id>  <title>    [profile:<name>]`
    ///
    /// Atomically printed (single write call) so parallel renderers do not
    /// interleave the header itself.
    pub fn header(&mut self, title: &str, profile: &ProfileName) -> io::Result<()> {
        let line = format!(
            "▸ {id}  {title}    [profile:{profile}]\n",
            id = self.bead_id.as_str(),
            title = title,
            profile = profile.as_str(),
        );
        self.out.write_all(line.as_bytes())?;
        self.out.flush()?;
        self.header_printed = true;
        Ok(())
    }

    /// Render one [`AgentEvent`].
    ///
    /// In `Default` mode: prints a one-line summary for `ToolCall` (name +
    /// truncated path/range/cmd-prefix). Suppresses `MessageDelta`,
    /// `ToolResult`, `TurnEnd`, and the compaction events.
    ///
    /// In `Verbose` mode: also writes `MessageDelta.text` straight through
    /// (no newline injection) and includes the tool-call args inline.
    pub fn render_event(&mut self, event: &AgentEvent) -> io::Result<()> {
        match event {
            AgentEvent::ToolCall {
                envelope,
                id,
                tool,
                params,
                parent_tool_call_id,
            } => {
                // If another tool's running line (or buffered overflow)
                // is in flight, finalize it before laying down a nested
                // or sibling call.
                self.close_in_flight("…")?;
                self.tool_count += 1;
                let depth = match parent_tool_call_id {
                    Some(parent) => self.indent_by_tool.get(parent).copied().unwrap_or(0) + 1,
                    None => 0,
                };
                self.indent_by_tool.insert(id.clone(), depth);
                self.pending_calls.insert(
                    id.clone(),
                    PendingToolCall {
                        tool: tool.clone(),
                        params: params.clone(),
                        ts_ms: envelope.ts_ms,
                    },
                );
                let summary = tool_body::summary_cell(tool, params, &self.osc8);
                let indent: String = "  ".repeat(depth);
                let use_indicator = self.indicator_enabled
                    && parent_tool_call_id.is_none()
                    && !matches!(self.mode, RenderMode::Verbose);
                if use_indicator {
                    // Decide between the single-line indicator (summary
                    // shares its row with the right-edge column) and the
                    // buffered overflow case (right-edge column gets its
                    // own row at result time, summary wraps below).
                    let prefix = self.prefix_str();
                    let single_width = display_width(&prefix)
                        + display_width(&indent)
                        + display_width(&summary)
                        + SUMMARY_SEPARATOR.len()
                        + RIGHT_COL_RESERVE;
                    if single_width <= self.term_width() {
                        let running_line =
                            format!("{prefix}{indent}{summary}{SUMMARY_SEPARATOR}running... 0.0s");
                        self.out.write_all(running_line.as_bytes())?;
                        self.out.flush()?;
                        self.running = Some((id.clone(), self.clock.now(), summary, indent));
                    } else {
                        // Suppress the indicator: writing `running...` on
                        // the same line would mangle the \r-overwrite
                        // region once the terminal wrapped the long
                        // summary. Defer the entire summary cell until
                        // result time and emit the spec'd two-line layout
                        // there (right column first, indented continuation
                        // beneath).
                        self.buffered_overflow =
                            Some((id.clone(), self.clock.now(), summary, indent));
                    }
                } else {
                    let prefix = self.prefix_str();
                    let line = format!("{prefix}{indent}{summary}\n");
                    self.out.write_all(line.as_bytes())?;
                    self.out.flush()?;
                }
            }
            AgentEvent::ToolResult {
                envelope,
                id,
                output,
                is_error,
            } => {
                // Finalize the running indicator if this result matches
                // the open call. Non-matching results (which shouldn't
                // happen in practice but might under nesting) close any
                // open running line first to avoid orphaned `\r`
                // regions.
                let matches_running = self
                    .running
                    .as_ref()
                    .is_some_and(|(running_id, _, _, _)| running_id == id);
                let matches_buffered = self
                    .buffered_overflow
                    .as_ref()
                    .is_some_and(|(buffered_id, _, _, _)| buffered_id == id);
                let pending = self.pending_calls.remove(id);
                let pair_duration = pending
                    .as_ref()
                    .map(|p| ts_ms_delta(p.ts_ms, envelope.ts_ms));
                let tool_name = pending.as_ref().map(|p| p.tool.as_str()).unwrap_or("");
                if matches_running {
                    let glyph = if *is_error { "✗" } else { "✓" };
                    self.finalize_running_with_glyph_dur(glyph, pair_duration)?;
                } else if matches_buffered {
                    let glyph = if *is_error { "✗" } else { "✓" };
                    self.finalize_buffered_with_glyph_dur(glyph, pair_duration)?;
                } else {
                    self.close_in_flight("…")?;
                    // Replay (non-indicator) path still needs to
                    // surface the pair's final glyph + duration so
                    // `loom logs` shows the same information as `loom
                    // run`. Emit one indented closer line under the
                    // already-printed tool-call summary.
                    if !self.live {
                        let glyph = if *is_error { "✗" } else { "✓" };
                        let depth = self.indent_by_tool.get(id).copied().unwrap_or(0);
                        let indent: String = "  ".repeat(depth + 1);
                        let secs = pair_duration.unwrap_or(Duration::ZERO).as_secs_f64();
                        let line = if self.parallel {
                            format!("  [{}] {indent}{glyph} {secs:.1}s\n", self.bead_id.as_str())
                        } else {
                            format!("  {indent}{glyph} {secs:.1}s\n")
                        };
                        self.out.write_all(line.as_bytes())?;
                        self.out.flush()?;
                    }
                }
                let render_body = match self.mode {
                    RenderMode::Verbose => true,
                    // Bash: hide on exit == 0 (is_error=false), render
                    // first 10 lines on exit != 0 (is_error=true). All
                    // other tools default to hidden in non-verbose mode.
                    RenderMode::Default => tool_name == "Bash" && *is_error,
                    _ => false,
                };
                if render_body {
                    let depth = self.indent_by_tool.get(id).copied().unwrap_or(0);
                    let indent: String = "  ".repeat(depth + 1);
                    let capped = tool_body::cap_body(output, self.bead_id.as_str(), id.as_str());
                    for body_line in capped.lines() {
                        let line = if self.parallel {
                            format!("  [{}] {indent}{body_line}\n", self.bead_id.as_str(),)
                        } else {
                            format!("{indent}{body_line}\n")
                        };
                        self.out.write_all(line.as_bytes())?;
                    }
                    if *is_error && matches!(self.mode, RenderMode::Verbose) {
                        let marker = format!("{indent}[tool error]\n");
                        self.out.write_all(marker.as_bytes())?;
                    }
                    self.out.flush()?;
                }
            }
            AgentEvent::TextDelta { text, .. } if matches!(self.mode, RenderMode::Verbose) => {
                self.close_in_flight("…")?;
                self.out.write_all(text.as_bytes())?;
                self.out.flush()?;
            }
            AgentEvent::Error { message, .. } => {
                self.close_in_flight("✗")?;
                let line = if self.parallel {
                    format!("  [{}] error: {message}\n", self.bead_id.as_str())
                } else {
                    format!("  error: {message}\n")
                };
                self.out.write_all(line.as_bytes())?;
                self.out.flush()?;
            }
            AgentEvent::DriverEvent {
                driver_kind,
                summary,
                ..
            } => {
                self.close_in_flight("…")?;
                let kind_wire = match driver_kind {
                    DriverKind::VerdictGate
                    | DriverKind::RetryDispatch
                    | DriverKind::PushGateWalk
                    | DriverKind::PushGateRefuse
                    | DriverKind::PushGateClean
                    | DriverKind::ContainerSpawn
                    | DriverKind::ContainerOom
                    | DriverKind::InfraFailure
                    | DriverKind::TokenUsage
                    | DriverKind::DuplicateToolResult
                    | DriverKind::DoomLoopTripped
                    | DriverKind::EpicAutoClosed => driver_kind.as_wire(),
                    DriverKind::Other(name) => name.as_str(),
                };
                let line = if self.parallel {
                    format!("  [{}] → {kind_wire}: {summary}\n", self.bead_id.as_str(),)
                } else {
                    format!("  → {kind_wire}: {summary}\n")
                };
                self.out.write_all(line.as_bytes())?;
                self.out.flush()?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Close the in-place running line — if one is open — with a final
    /// `\r` + clear-to-EOL + `<summary>   <glyph> Ns\n` line. `glyph` is
    /// `✓` on clean ToolResult, `✗` on errored ToolResult, `…` when an
    /// intervening event preempted the result. The caller may pass a
    /// duration derived from `ts_ms` deltas instead of falling back to
    /// the local clock; `loom logs` replay drives this with the event
    /// envelope's `ts_ms` so the rendered duration matches what the
    /// producer recorded. Idempotent — a `running = None` call is a
    /// no-op so cleanup paths can invoke unconditionally.
    fn finalize_running_with_glyph_dur(
        &mut self,
        glyph: &str,
        pair_duration: Option<Duration>,
    ) -> io::Result<()> {
        let Some((_, started, summary, indent)) = self.running.take() else {
            return Ok(());
        };
        let elapsed =
            pair_duration.unwrap_or_else(|| self.clock.now().saturating_duration_since(started));
        let secs = elapsed.as_secs_f64();
        self.out.write_all(b"\r")?;
        self.out.write_all(CLEAR_TO_EOL.as_bytes())?;
        let prefix = self.prefix_str();
        let final_line =
            format!("{prefix}{indent}{summary}{SUMMARY_SEPARATOR}{glyph} {secs:.1}s\n");
        self.out.write_all(final_line.as_bytes())?;
        self.out.flush()?;
        Ok(())
    }

    /// Close any in-flight summary cell — running indicator OR buffered
    /// overflow case — with the given glyph. Both inner finalizers are
    /// idempotent no-ops when their state is empty, so callers can
    /// invoke unconditionally before laying down a new event line.
    fn close_in_flight(&mut self, glyph: &str) -> io::Result<()> {
        self.finalize_running_with_glyph_dur(glyph, None)?;
        self.finalize_buffered_with_glyph_dur(glyph, None)
    }

    /// Emit the two-line summary cell for a buffered (overflow) top-level
    /// tool call: right-edge column right-aligned on line 1 so it stays
    /// fully visible at the terminal edge, followed by the full left-edge
    /// summary on a body-indented continuation line. Mirrors the spec's
    /// "right-edge column alone on the first line, full content wraps
    /// onto a single indented continuation line beneath it" rule.
    fn finalize_buffered_with_glyph_dur(
        &mut self,
        glyph: &str,
        pair_duration: Option<Duration>,
    ) -> io::Result<()> {
        let Some((_, started, summary, indent)) = self.buffered_overflow.take() else {
            return Ok(());
        };
        let elapsed =
            pair_duration.unwrap_or_else(|| self.clock.now().saturating_duration_since(started));
        let secs = elapsed.as_secs_f64();
        let right_col = format!("{glyph} {secs:.1}s");
        let prefix = self.prefix_str();
        let prefix_w = display_width(&prefix);
        let right_w = display_width(&right_col);
        let pad_w = self.term_width().saturating_sub(prefix_w + right_w);
        let pad = " ".repeat(pad_w);
        // Continuation indent matches the body indent used for tool
        // bodies in verbose mode — one extra level deeper than the
        // summary's depth indent.
        let body_indent = format!("{indent}  ");
        let line1 = format!("{prefix}{pad}{right_col}\n");
        let line2 = format!("{prefix}{body_indent}{summary}\n");
        self.out.write_all(line1.as_bytes())?;
        self.out.write_all(line2.as_bytes())?;
        self.out.flush()?;
        Ok(())
    }

    /// Print the closing line and consume the renderer.
    ///
    /// `✓ done  (N tool calls, Ms)` for [`BeadOutcome::Done`],
    /// `✗ failed  (N tool calls, Ms)` for [`BeadOutcome::Failed`],
    /// `↻ retry  (N tool calls, Ms)` for [`BeadOutcome::Retry`].
    ///
    /// Status color is applied to the leading glyph + word only when
    /// `color = true`; the rest of the line is plain.
    pub fn finish(mut self, outcome: BeadOutcome) -> io::Result<()> {
        let elapsed = self.clock.now().saturating_duration_since(self.started);
        self.write_finish(outcome, elapsed)
    }

    fn write_finish(&mut self, outcome: BeadOutcome, elapsed: Duration) -> io::Result<()> {
        // Close out any open running line before the bead's closing
        // summary so a cancelled / failed bead doesn't leave a dangling
        // `\r` region for the next renderer to inherit.
        let cleanup_glyph = match outcome {
            BeadOutcome::Done => "✓",
            BeadOutcome::Failed => "✗",
            BeadOutcome::Retry => "↻",
        };
        self.close_in_flight(cleanup_glyph)?;
        let (glyph, word, color) = match outcome {
            BeadOutcome::Done => ("✓", "done", ANSI_GREEN),
            BeadOutcome::Failed => ("✗", "failed", ANSI_RED),
            BeadOutcome::Retry => ("↻", "retry", ANSI_YELLOW),
        };
        let prefix = if self.color {
            format!("{color}{glyph} {word}{ANSI_RESET}")
        } else {
            format!("{glyph} {word}")
        };
        let body = format!(
            "  ({tools} tool calls, {secs}s)\n",
            tools = self.tool_count,
            secs = elapsed.as_secs(),
        );
        let line = if self.parallel {
            format!("[{}] {prefix}{body}", self.bead_id.as_str())
        } else {
            format!("  {prefix}{body}")
        };
        self.out.write_all(line.as_bytes())?;
        self.out.flush()?;
        self.closed = true;
        Ok(())
    }

    /// Number of `ToolCall` events observed since construction.
    pub fn tool_count(&self) -> u32 {
        self.tool_count
    }

    /// Whether the header line has been printed.
    pub fn header_printed(&self) -> bool {
        self.header_printed
    }
}

impl Drop for TerminalRenderer {
    /// Best-effort cleanup of any open in-place running line so a
    /// panicking / dropped renderer never leaves a dangling `\r`
    /// region. The explicit cleanup contract still lives in
    /// [`finish`] — Drop is the safety net.
    fn drop(&mut self) {
        if self.running.is_some() || self.buffered_overflow.is_some() {
            let _ = self.close_in_flight("⚠");
        }
    }
}

/// Output target for one bead's event stream. Four concrete impls
/// (Pretty/Plain/Json/Raw) sit behind this trait; selection happens
/// once per spawn via [`RenderMode::select`] and the chosen impl is
/// handed to [`crate::LogSink`] as `Box<dyn Renderer>`.
///
/// `loom run` and `loom logs` share these impls — the `live` /
/// replay distinction is encoded in how they feed events to the
/// renderer, not in the renderer itself. H3-H6 add per-tool body
/// rendering, in-place running indicators, OSC 8 hyperlinks, and
/// Task subagent nesting on top of this trait.
pub trait Renderer: Send {
    /// Optional per-bead header — `Pretty`/`Plain` print it; `Json`/`Raw`
    /// suppress (header is chrome, not data). Default impl is a no-op
    /// so simpler impls don't have to override.
    fn header(&mut self, _title: &str, _profile: &ProfileName) -> io::Result<()> {
        Ok(())
    }

    /// Render one event. The renderer owns its `Write` sink.
    fn render_event(&mut self, event: &AgentEvent) -> io::Result<()>;

    /// Close the renderer with the bead outcome. `&mut self` (not
    /// consuming) so trait-object call through `Box<dyn Renderer>`
    /// stays straightforward without `Box<Self>` indirection.
    fn finish(&mut self, outcome: BeadOutcome, elapsed: Duration) -> io::Result<()>;
}

impl Renderer for TerminalRenderer {
    fn header(&mut self, title: &str, profile: &ProfileName) -> io::Result<()> {
        TerminalRenderer::header(self, title, profile)
    }
    fn render_event(&mut self, event: &AgentEvent) -> io::Result<()> {
        TerminalRenderer::render_event(self, event)
    }
    fn finish(&mut self, outcome: BeadOutcome, elapsed: Duration) -> io::Result<()> {
        self.write_finish(outcome, elapsed)
    }
}

/// `Pretty` mode — colored, glyph-decorated output for an interactive
/// terminal. Thin wrapper that delegates to the existing
/// [`TerminalRenderer`] with `color = true`. `live` distinguishes
/// `loom run` (events arrive in real time, in-place indicator spins)
/// from `loom logs` (replay; indicator suppressed, durations from
/// `ts_ms` deltas).
pub struct PrettyRenderer {
    inner: TerminalRenderer,
}

impl PrettyRenderer {
    pub fn new(
        out: impl Write + Send + 'static,
        bead_id: BeadId,
        parallel: bool,
        live: bool,
    ) -> Self {
        let inner = if live {
            TerminalRenderer::new(out, RenderMode::Default, bead_id, parallel, true)
        } else {
            TerminalRenderer::new_replay(out, RenderMode::Default, bead_id, parallel, true)
        };
        Self { inner }
    }
}

impl Renderer for PrettyRenderer {
    fn header(&mut self, title: &str, profile: &ProfileName) -> io::Result<()> {
        self.inner.header(title, profile)
    }
    fn render_event(&mut self, event: &AgentEvent) -> io::Result<()> {
        self.inner.render_event(event)
    }
    fn finish(&mut self, outcome: BeadOutcome, elapsed: Duration) -> io::Result<()> {
        self.inner.write_finish(outcome, elapsed)
    }
}

/// `Plain` mode — same shape as `Pretty` but with `color = false`.
/// Pipe-safe; no ANSI escapes; no OSC 8. The `live` parameter follows
/// the same contract as [`PrettyRenderer::new`].
pub struct PlainRenderer {
    inner: TerminalRenderer,
}

impl PlainRenderer {
    pub fn new(
        out: impl Write + Send + 'static,
        bead_id: BeadId,
        parallel: bool,
        live: bool,
    ) -> Self {
        let inner = if live {
            TerminalRenderer::new(out, RenderMode::Default, bead_id, parallel, false)
        } else {
            TerminalRenderer::new_replay(out, RenderMode::Default, bead_id, parallel, false)
        };
        Self { inner }
    }
}

impl Renderer for PlainRenderer {
    fn header(&mut self, title: &str, profile: &ProfileName) -> io::Result<()> {
        self.inner.header(title, profile)
    }
    fn render_event(&mut self, event: &AgentEvent) -> io::Result<()> {
        self.inner.render_event(event)
    }
    fn finish(&mut self, outcome: BeadOutcome, elapsed: Duration) -> io::Result<()> {
        self.inner.write_finish(outcome, elapsed)
    }
}

/// `Json` mode — one pretty-printed JSON object per line. No header,
/// no closing line; the consumer is expected to parse the stream.
pub struct JsonRenderer {
    out: Box<dyn Write + Send>,
}

impl JsonRenderer {
    pub fn new(out: impl Write + Send + 'static) -> Self {
        Self { out: Box::new(out) }
    }
}

impl Renderer for JsonRenderer {
    fn render_event(&mut self, event: &AgentEvent) -> io::Result<()> {
        let line = serde_json::to_string_pretty(event)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        self.out.write_all(line.as_bytes())?;
        self.out.write_all(b"\n")?;
        self.out.flush()
    }
    fn finish(&mut self, _outcome: BeadOutcome, _elapsed: Duration) -> io::Result<()> {
        self.out.flush()
    }
}

/// `Raw` mode — one compact JSON line per event. Used by
/// `loom logs --raw` to reproduce the on-disk shape exactly.
pub struct RawRenderer {
    out: Box<dyn Write + Send>,
}

impl RawRenderer {
    pub fn new(out: impl Write + Send + 'static) -> Self {
        Self { out: Box::new(out) }
    }
}

impl Renderer for RawRenderer {
    fn render_event(&mut self, event: &AgentEvent) -> io::Result<()> {
        let line = serde_json::to_string(event)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        self.out.write_all(line.as_bytes())?;
        self.out.write_all(b"\n")?;
        self.out.flush()
    }
    fn finish(&mut self, _outcome: BeadOutcome, _elapsed: Duration) -> io::Result<()> {
        self.out.flush()
    }
}

/// Construct the right [`Renderer`] for a given mode. Production
/// callers in `loom run` / `loom logs` pass a `Box<dyn Write + Send>`
/// (typically `io::stdout()` or a buffered wrapper). `live` is `true`
/// for `loom run` (in-place indicator + wall-clock fallback) and
/// `false` for `loom logs` replay (indicator suppressed, durations
/// from `ts_ms` deltas).
pub fn build_renderer(
    mode: RenderMode,
    out: Box<dyn Write + Send>,
    bead_id: BeadId,
    parallel: bool,
    live: bool,
) -> Box<dyn Renderer> {
    match mode {
        RenderMode::Pretty => Box::new(PrettyRenderer::new(out, bead_id, parallel, live)),
        RenderMode::Plain | RenderMode::Default => {
            Box::new(PlainRenderer::new(out, bead_id, parallel, live))
        }
        // `Verbose` is a real mode at the TerminalRenderer level — it
        // streams `TextDelta` verbatim and renders capped tool bodies.
        // Construct the underlying renderer with `RenderMode::Verbose`
        // directly so the verbose dispatch in `render_event` triggers;
        // `color=true` is safe because the writer is opaque (TTY-vs-pipe
        // is the caller's concern via `RenderMode::select`).
        RenderMode::Verbose => {
            let inner = if live {
                TerminalRenderer::new(out, RenderMode::Verbose, bead_id, parallel, true)
            } else {
                TerminalRenderer::new_replay(out, RenderMode::Verbose, bead_id, parallel, true)
            };
            Box::new(inner)
        }
        RenderMode::Json => Box::new(JsonRenderer::new(out)),
        RenderMode::Raw => Box::new(RawRenderer::new(out)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_events::EventEnvelope;
    use loom_events::Source;
    use loom_events::identifier::ToolCallId;
    use serde_json::json;

    /// Fixture envelope for renderer tests. Carries a fully-valid
    /// `wx-test` bead id; the renderer reads `ts_ms` / `seq` / `source`
    /// but does not dispatch on `bead_id`, so the fixture is reused
    /// across every test that does not need to vary the envelope.
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

    fn capture<F>(mode: RenderMode, parallel: bool, color: bool, f: F) -> String
    where
        F: FnOnce(&mut TerminalRenderer),
    {
        let buf: Vec<u8> = Vec::new();
        let cell = std::sync::Arc::new(std::sync::Mutex::new(buf));
        let cell_for_writer = cell.clone();
        struct Sink {
            inner: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
        }
        impl Write for Sink {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.inner
                    .lock()
                    .map_err(|_| io::Error::other("poisoned"))?
                    .extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut r = TerminalRenderer::new(
            Sink {
                inner: cell_for_writer,
            },
            mode,
            BeadId::new("wx-1").expect("valid bead id"),
            parallel,
            color,
        );
        f(&mut r);
        let guard = cell.lock().expect("not poisoned");
        String::from_utf8(guard.clone()).expect("utf-8")
    }

    #[test]
    fn default_mode_suppresses_message_deltas() {
        let out = capture(RenderMode::Default, false, false, |r| {
            r.render_event(&AgentEvent::TextDelta {
                envelope: sample_envelope(),
                text: "hello world".to_string(),
            })
            .expect("render");
        });
        assert_eq!(out, "");
    }

    #[test]
    fn verbose_mode_streams_message_deltas_verbatim() {
        let out = capture(RenderMode::Verbose, false, false, |r| {
            r.render_event(&AgentEvent::TextDelta {
                envelope: sample_envelope(),
                text: "hel".to_string(),
            })
            .expect("render");
            r.render_event(&AgentEvent::TextDelta {
                envelope: sample_envelope(),
                text: "lo".to_string(),
            })
            .expect("render");
        });
        assert_eq!(out, "hello");
    }

    #[test]
    fn tool_call_line_in_default_mode_shows_path() {
        let out = capture(RenderMode::Default, false, false, |r| {
            r.render_event(&AgentEvent::ToolCall {
                envelope: sample_envelope(),
                id: ToolCallId::new("t1"),
                tool: "Read".to_string(),
                params: json!({"file_path": "src/lib.rs"}),
                parent_tool_call_id: None,
            })
            .expect("render");
        });
        assert!(out.contains("Read"), "{out:?}");
        assert!(out.contains("src/lib.rs"), "{out:?}");
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn parallel_mode_prefixes_tool_lines_with_bead_id() {
        let out = capture(RenderMode::Default, true, false, |r| {
            r.render_event(&AgentEvent::ToolCall {
                envelope: sample_envelope(),
                id: ToolCallId::new("t1"),
                tool: "Bash".to_string(),
                params: json!({"command": "cargo build"}),
                parent_tool_call_id: None,
            })
            .expect("render");
        });
        assert!(out.contains("[wx-1]"), "{out:?}");
        assert!(out.contains("Bash"));
    }

    #[test]
    fn header_line_includes_id_title_profile() {
        let out = capture(RenderMode::Default, false, false, |r| {
            r.header("Implement parser", &ProfileName::new("rust"))
                .expect("header");
        });
        assert!(out.contains("wx-1"), "{out:?}");
        assert!(out.contains("Implement parser"));
        assert!(out.contains("[profile:rust]"));
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn finish_done_includes_tool_count_and_secs() {
        let out = capture(RenderMode::Default, false, false, |r| {
            r.render_event(&AgentEvent::ToolCall {
                envelope: sample_envelope(),
                id: ToolCallId::new("t1"),
                tool: "Read".to_string(),
                params: json!({"file_path": "a"}),
                parent_tool_call_id: None,
            })
            .expect("render");
            r.render_event(&AgentEvent::ToolCall {
                envelope: sample_envelope(),
                id: ToolCallId::new("t2"),
                tool: "Edit".to_string(),
                params: json!({"file_path": "b"}),
                parent_tool_call_id: None,
            })
            .expect("render");
            r.write_finish(BeadOutcome::Done, std::time::Duration::from_secs(47))
                .expect("finish");
        });
        assert!(out.contains("done"));
        assert!(out.contains("2 tool calls"));
        assert!(out.contains("47s"));
    }

    #[test]
    fn finish_with_color_emits_ansi_codes() {
        let out = capture(RenderMode::Default, false, true, |r| {
            r.write_finish(BeadOutcome::Failed, std::time::Duration::from_secs(1))
                .expect("finish");
        });
        assert!(out.contains(ANSI_RED), "{out:?}");
        assert!(out.contains(ANSI_RESET));
    }

    // -- summary-cell tests ------------------------------------------------

    /// `ToolCall` lines use the per-tool spec cell — `Read   <path>:<range>`,
    /// `Edit   <path>   +N -M   diff↓`, `Bash   <cmd>`. The cell shape comes
    /// from `tool_body::summary_cell`; this test pins that the renderer
    /// dispatches through it.
    #[test]
    fn tool_call_line_uses_summary_cell_shape() {
        let out = capture(RenderMode::Default, false, false, |r| {
            r.render_event(&AgentEvent::ToolCall {
                envelope: sample_envelope(),
                id: ToolCallId::new("e1"),
                tool: "Edit".into(),
                params: json!({
                    "file_path": "src/lib.rs",
                    "old_string": "fn old() {}\n",
                    "new_string": "fn new() {}\nfn extra() {}\n",
                }),
                parent_tool_call_id: None,
            })
            .expect("render");
        });
        assert!(out.contains("Edit"), "{out:?}");
        assert!(out.contains("src/lib.rs"), "{out:?}");
        assert!(out.contains("+"), "{out:?}");
        assert!(out.contains("-"), "{out:?}");
        assert!(out.contains("diff"), "{out:?}");
    }

    /// Verbose mode emits the `ToolResult` body, capped to 10 lines, with
    /// the spec'd recovery hint when truncated.
    #[test]
    fn verbose_mode_renders_capped_tool_result_body_with_recovery_hint() {
        let big_body: String = (1..=15)
            .map(|i| format!("output-line-{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = capture(RenderMode::Verbose, false, false, |r| {
            r.render_event(&AgentEvent::ToolCall {
                envelope: sample_envelope(),
                id: ToolCallId::new("b1"),
                tool: "Bash".into(),
                params: json!({"command": "echo hi"}),
                parent_tool_call_id: None,
            })
            .expect("render call");
            r.render_event(&AgentEvent::ToolResult {
                envelope: sample_envelope(),
                id: ToolCallId::new("b1"),
                output: big_body,
                is_error: false,
            })
            .expect("render result");
        });
        assert!(out.contains("output-line-1"), "{out:?}");
        assert!(out.contains("output-line-10"), "{out:?}");
        assert!(
            !out.contains("output-line-11"),
            "body cap should drop line 11: {out:?}",
        );
        assert!(out.contains("more lines"), "missing recovery hint: {out:?}",);
        assert!(
            out.contains("loom logs -b wx-1 --tool b1"),
            "recovery hint must reference the bead and tool call id: {out:?}",
        );
    }

    /// Default mode hides the Bash body when `exit == 0` (is_error=false).
    /// Per the spec's per-tool body table — a clean Bash invocation is
    /// summary-cell only so the eye learns the shape.
    #[test]
    fn default_mode_hides_bash_body_on_success() {
        let out = capture(RenderMode::Default, false, false, |r| {
            r.render_event(&AgentEvent::ToolCall {
                envelope: sample_envelope(),
                id: ToolCallId::new("b1"),
                tool: "Bash".into(),
                params: json!({"command": "echo hi"}),
                parent_tool_call_id: None,
            })
            .expect("render call");
            r.render_event(&AgentEvent::ToolResult {
                envelope: sample_envelope(),
                id: ToolCallId::new("b1"),
                output: "result body".into(),
                is_error: false,
            })
            .expect("render result");
        });
        assert!(out.contains("Bash"), "{out:?}");
        assert!(!out.contains("result body"), "{out:?}");
    }

    /// Default mode renders the Bash body when `exit != 0` (is_error=true),
    /// capped at 10 lines with the standard recovery hint when the output
    /// exceeds the cap. Pin both the kept-line set and the cap line.
    #[test]
    fn default_mode_renders_bash_body_on_error_capped_at_ten_lines() {
        let big_body: String = (1..=15)
            .map(|i| format!("output-line-{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = capture(RenderMode::Default, false, false, |r| {
            r.render_event(&AgentEvent::ToolCall {
                envelope: sample_envelope(),
                id: ToolCallId::new("b1"),
                tool: "Bash".into(),
                params: json!({"command": "false"}),
                parent_tool_call_id: None,
            })
            .expect("render call");
            r.render_event(&AgentEvent::ToolResult {
                envelope: sample_envelope(),
                id: ToolCallId::new("b1"),
                output: big_body,
                is_error: true,
            })
            .expect("render result");
        });
        assert!(out.contains("output-line-1"), "{out:?}");
        assert!(out.contains("output-line-10"), "{out:?}");
        assert!(
            !out.contains("output-line-11"),
            "default Bash error body must cap at 10 lines: {out:?}",
        );
        assert!(out.contains("more lines"), "missing cap line: {out:?}");
        assert!(
            out.contains("loom logs -b wx-1 --tool b1"),
            "cap line must reference the bead and tool call id: {out:?}",
        );
    }

    /// Default mode renders the full Bash body when `exit != 0` and the
    /// output is at or under the 10-line cap — no cap line is emitted.
    #[test]
    fn default_mode_renders_bash_body_on_error_short_output_no_cap_line() {
        let body = "line1\nline2\nline3";
        let out = capture(RenderMode::Default, false, false, |r| {
            r.render_event(&AgentEvent::ToolCall {
                envelope: sample_envelope(),
                id: ToolCallId::new("b1"),
                tool: "Bash".into(),
                params: json!({"command": "false"}),
                parent_tool_call_id: None,
            })
            .expect("render call");
            r.render_event(&AgentEvent::ToolResult {
                envelope: sample_envelope(),
                id: ToolCallId::new("b1"),
                output: body.into(),
                is_error: true,
            })
            .expect("render result");
        });
        assert!(out.contains("line1"), "{out:?}");
        assert!(out.contains("line3"), "{out:?}");
        assert!(
            !out.contains("more lines"),
            "no cap line when body is at or below the 10-line cap: {out:?}",
        );
    }

    /// Default mode keeps non-Bash error bodies hidden — only Bash gets
    /// the "first 10 lines on exit != 0" treatment in this bead's scope.
    /// Other tools' default-mode body policy lives in separate beads.
    #[test]
    fn default_mode_hides_non_bash_error_body() {
        let out = capture(RenderMode::Default, false, false, |r| {
            r.render_event(&AgentEvent::ToolCall {
                envelope: sample_envelope(),
                id: ToolCallId::new("r1"),
                tool: "Read".into(),
                params: json!({"file_path": "src/lib.rs"}),
                parent_tool_call_id: None,
            })
            .expect("render call");
            r.render_event(&AgentEvent::ToolResult {
                envelope: sample_envelope(),
                id: ToolCallId::new("r1"),
                output: "file-contents".into(),
                is_error: true,
            })
            .expect("render result");
        });
        assert!(out.contains("Read"), "{out:?}");
        assert!(!out.contains("file-contents"), "{out:?}");
    }

    /// Renderer wraps `ToolCall` summary cells in OSC 8 hyperlinks when
    /// the OSC 8 context is enabled. Cmd-click on `src/lib.rs` in the
    /// rendered line opens the file in the user's editor.
    #[test]
    fn tool_call_line_wraps_paths_in_osc8_when_enabled() {
        let buf: Vec<u8> = Vec::new();
        let cell = std::sync::Arc::new(std::sync::Mutex::new(buf));
        let cell_for_writer = cell.clone();
        struct Sink {
            inner: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
        }
        impl Write for Sink {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.inner
                    .lock()
                    .map_err(|_| io::Error::other("poisoned"))?
                    .extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut r = TerminalRenderer::new(
            Sink {
                inner: cell_for_writer,
            },
            RenderMode::Default,
            BeadId::new("wx-1").expect("valid bead id"),
            false,
            false,
        )
        .with_osc8(tool_body::Osc8Context::enabled(std::path::PathBuf::from(
            "/workspace",
        )));
        r.render_event(&AgentEvent::ToolCall {
            envelope: sample_envelope(),
            id: ToolCallId::new("e1"),
            tool: "Edit".into(),
            params: json!({
                "file_path": "src/lib.rs",
                "old_string": "old\n",
                "new_string": "new\n",
            }),
            parent_tool_call_id: None,
        })
        .expect("render");
        let out = String::from_utf8(cell.lock().expect("not poisoned").clone()).expect("utf-8");
        assert!(
            out.contains("\x1b]8;;file:///workspace/src/lib.rs"),
            "Edit summary cell must wrap the path in OSC 8: {out:?}",
        );
        assert!(
            out.contains("src/lib.rs"),
            "display text still present: {out:?}"
        );
    }

    /// Without OSC 8 (default), the rendered line contains plain text
    /// with no escape bytes around the path. Silent degradation.
    #[test]
    fn tool_call_line_no_osc8_by_default() {
        let out = capture(RenderMode::Default, false, false, |r| {
            r.render_event(&AgentEvent::ToolCall {
                envelope: sample_envelope(),
                id: ToolCallId::new("e1"),
                tool: "Edit".into(),
                params: json!({
                    "file_path": "src/lib.rs",
                    "old_string": "old\n",
                    "new_string": "new\n",
                }),
                parent_tool_call_id: None,
            })
            .expect("render");
        });
        assert!(out.contains("src/lib.rs"), "{out:?}");
        assert!(
            !out.contains("\x1b]8;;"),
            "default context must not emit OSC 8 escapes: {out:?}",
        );
    }

    /// Verbose mode marks tool-error results with a `[tool error]` line
    /// after the body. Used by the renderer-driven failure UI.
    #[test]
    fn verbose_mode_flags_tool_errors_after_body() {
        let out = capture(RenderMode::Verbose, false, false, |r| {
            r.render_event(&AgentEvent::ToolCall {
                envelope: sample_envelope(),
                id: ToolCallId::new("b1"),
                tool: "Bash".into(),
                params: json!({"command": "false"}),
                parent_tool_call_id: None,
            })
            .expect("render call");
            r.render_event(&AgentEvent::ToolResult {
                envelope: sample_envelope(),
                id: ToolCallId::new("b1"),
                output: "exit 1".into(),
                is_error: true,
            })
            .expect("render result");
        });
        assert!(out.contains("[tool error]"), "{out:?}");
    }

    // -- H2 tests ----------------------------------------------------------

    fn captured() -> (
        std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
        impl Write + Send + 'static,
    ) {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = SharedWriter(buf.clone());
        (buf, writer)
    }

    struct SharedWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
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

    fn captured_str(buf: &std::sync::Arc<std::sync::Mutex<Vec<u8>>>) -> String {
        let g = buf.lock().expect("not poisoned");
        String::from_utf8(g.clone()).expect("utf-8")
    }

    fn sample_text_delta() -> AgentEvent {
        AgentEvent::TextDelta {
            envelope: sample_envelope(),
            text: "hello".into(),
        }
    }

    fn sample_tool_call() -> AgentEvent {
        AgentEvent::ToolCall {
            envelope: sample_envelope(),
            id: ToolCallId::new("t1"),
            tool: "Read".into(),
            params: json!({"file_path": "src/lib.rs"}),
            parent_tool_call_id: None,
        }
    }

    /// All four spec'd render modes are present and `build_renderer`
    /// dispatches to a concrete impl for each.
    #[test]
    fn renderer_modes_present() {
        let bead = BeadId::new("wx-1").expect("valid id");
        for mode in [
            RenderMode::Pretty,
            RenderMode::Plain,
            RenderMode::Json,
            RenderMode::Raw,
        ] {
            let (_, writer) = captured();
            let r = build_renderer(mode, Box::new(writer), bead.clone(), false, true);
            drop(r); // sanity — each mode constructs without panic
        }
    }

    /// TTY + no env / flags → Pretty.
    #[test]
    fn pretty_selected_on_tty() {
        assert_eq!(
            RenderMode::select(true, false, false, false, false),
            RenderMode::Pretty,
        );
    }

    /// Non-TTY → Plain (NO_COLOR-agnostic).
    #[test]
    fn plain_selected_on_non_tty() {
        assert_eq!(
            RenderMode::select(false, false, false, false, false),
            RenderMode::Plain,
        );
    }

    /// `NO_COLOR=1` on TTY → Plain (no-color.org contract).
    #[test]
    fn plain_selected_when_no_color_env() {
        assert_eq!(
            RenderMode::select(true, true, false, false, false),
            RenderMode::Plain,
        );
    }

    /// `--json` beats TTY-detect → Json regardless of env.
    #[test]
    fn json_flag_wins_over_tty() {
        assert_eq!(
            RenderMode::select(true, false, false, true, false),
            RenderMode::Json,
        );
    }

    /// `--raw` beats `--json` per the spec's exclusivity contract.
    /// (clap enforces this at parse time but the selector is
    /// defensive: raw > json > plain > pretty.)
    #[test]
    fn raw_flag_wins_over_json() {
        assert_eq!(
            RenderMode::select(true, false, false, true, true),
            RenderMode::Raw,
        );
    }

    /// `JsonRenderer` writes pretty-printed JSON, one object per line.
    /// Pretty-printing means newlines INSIDE the object, then the
    /// terminator newline.
    #[test]
    fn json_mode_pretty_prints() {
        let (buf, writer) = captured();
        let mut r = JsonRenderer::new(writer);
        r.render_event(&sample_text_delta()).expect("render");
        let out = captured_str(&buf);
        assert!(out.starts_with('{'), "json must start with `{{`: {out}");
        // Pretty-printing keeps fields on separate lines.
        assert!(
            out.contains("\n  \"kind\""),
            "json output must be pretty-printed: {out}",
        );
        assert!(out.ends_with('\n'));
    }

    /// H6 — when a `ToolCall` carries `parent_tool_call_id = Some(parent)`,
    /// the renderer prints it indented by 2 spaces beyond the parent.
    /// Tracks indent depth per tool_call_id so nested chains layer
    /// correctly (Task → Read → Edit etc.).
    #[test]
    fn task_subagent_nesting_indents_nested_tool_calls() {
        let out = capture(RenderMode::Default, false, false, |r| {
            // Top-level Task call — no parent, depth 0.
            r.render_event(&AgentEvent::ToolCall {
                envelope: sample_envelope(),
                id: ToolCallId::new("task1"),
                tool: "Task".into(),
                params: json!({}),
                parent_tool_call_id: None,
            })
            .expect("render task");
            // Nested call inside the Task — depth 1, prefixed with two
            // extra spaces.
            r.render_event(&AgentEvent::ToolCall {
                envelope: sample_envelope(),
                id: ToolCallId::new("read1"),
                tool: "Read".into(),
                params: json!({"file_path": "a"}),
                parent_tool_call_id: Some(ToolCallId::new("task1")),
            })
            .expect("render nested");
        });
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "expected 2 lines, got: {out:?}");
        // Top-level: 2-space prefix (the renderer's base indent).
        assert!(lines[0].starts_with("  Task"), "{}", lines[0]);
        // Nested: base 2 + indent 2 = 4 spaces before the tool name.
        assert!(lines[1].starts_with("    Read"), "{}", lines[1]);
    }

    /// `RawRenderer` writes a compact JSON line — no embedded newlines
    /// in the object, terminator newline at end. Round-trips back to
    /// `AgentEvent` cleanly.
    #[test]
    fn raw_mode_passthrough() {
        let (buf, writer) = captured();
        let mut r = RawRenderer::new(writer);
        r.render_event(&sample_tool_call()).expect("render");
        let out = captured_str(&buf);
        let trimmed = out.trim_end_matches('\n');
        // Compact form has no internal newlines.
        assert!(
            !trimmed.contains('\n'),
            "raw output must be one compact line: {out}",
        );
        // Parses back to the same variant.
        let back: AgentEvent = serde_json::from_str(trimmed).expect("parse back");
        assert!(matches!(back, AgentEvent::ToolCall { .. }));
    }

    /// Build a `ToolCall` + `ToolResult` pair with explicit `ts_ms`
    /// values so the test can pin pair duration without touching
    /// either the wall clock or the renderer's `Clock` injection.
    fn tool_pair_with_ts(
        id: &str,
        tool: &str,
        params: serde_json::Value,
        call_ts: i64,
        result_ts: i64,
    ) -> (AgentEvent, AgentEvent) {
        let call_env = EventEnvelope {
            ts_ms: call_ts,
            ..sample_envelope()
        };
        let result_env = EventEnvelope {
            ts_ms: result_ts,
            ..sample_envelope()
        };
        (
            AgentEvent::ToolCall {
                envelope: call_env,
                id: ToolCallId::new(id),
                tool: tool.to_string(),
                params,
                parent_tool_call_id: None,
            },
            AgentEvent::ToolResult {
                envelope: result_env,
                id: ToolCallId::new(id),
                output: String::new(),
                is_error: false,
            },
        )
    }

    /// H2 + spec criterion `test_tool_call_result_pairing` — a paired
    /// `tool_call` + `tool_result` renders as a single closing line
    /// whose `Ns` duration comes from the events' `ts_ms` delta. We
    /// use the live + indicator path (color=true) because that is the
    /// branch where pair collapse is visible — the in-place line is
    /// rewritten with the final glyph + duration.
    #[test]
    fn tool_call_result_pairing_collapses_with_ts_ms_duration() {
        // color=true → indicator on (live + non-parallel). Pair the
        // result with a 3.5s ts_ms delta so the test can pin the
        // rendered duration independent of wall clock.
        let out = capture(RenderMode::Default, false, true, |r| {
            let (call, result) = tool_pair_with_ts(
                "t1",
                "Bash",
                serde_json::json!({"command": "cargo test"}),
                1_000,
                4_500,
            );
            r.render_event(&call).expect("render call");
            r.render_event(&result).expect("render result");
        });
        // After ToolResult, the running line is finalized — `\r` +
        // clear-to-EOL + the closing summary with the pair duration.
        assert!(out.contains("Bash"), "{out:?}");
        assert!(out.contains("cargo test"), "{out:?}");
        // ts_ms delta = 3500ms → "3.5s".
        assert!(out.contains("3.5s"), "expected pair duration 3.5s: {out:?}");
        // Single combined block: only one trailing newline after the
        // final summary, i.e. no separate ToolResult line.
        let trailing_newlines = out.chars().rev().take_while(|c| *c == '\n').count();
        assert_eq!(trailing_newlines, 1, "{out:?}");
    }

    /// H2 + spec criterion `test_live_vs_replay_distinction` — the
    /// `PrettyRenderer` (and `TerminalRenderer`) take a `live: bool`
    /// switch. Live emits the in-place `running...` indicator while a
    /// tool is in flight; replay suppresses it entirely and computes
    /// the pair's duration from `ts_ms` deltas.
    #[test]
    fn live_vs_replay_distinction_pretty_renderer() {
        // Live path: indicator appears between ToolCall and ToolResult.
        let (live_buf, live_writer) = captured();
        let mut live_pretty =
            PrettyRenderer::new(live_writer, BeadId::new("wx-1").expect("id"), false, true);
        let (call, result) = tool_pair_with_ts(
            "t1",
            "Bash",
            serde_json::json!({"command": "sleep 3"}),
            0,
            3_000,
        );
        live_pretty.render_event(&call).expect("live call");
        // Snapshot mid-pair: the running line is present.
        let mid = captured_str(&live_buf);
        assert!(
            mid.contains("running..."),
            "live mode must emit in-place running indicator: {mid:?}",
        );
        live_pretty.render_event(&result).expect("live result");
        let live_final = captured_str(&live_buf);
        assert!(live_final.contains("3.0s"), "{live_final:?}");

        // Replay path: indicator is suppressed; the ToolCall summary
        // prints once and the ToolResult appends a closing line under
        // it with the ts_ms-derived duration.
        let (replay_buf, replay_writer) = captured();
        let mut replay_pretty = PrettyRenderer::new(
            replay_writer,
            BeadId::new("wx-1").expect("id"),
            false,
            false,
        );
        replay_pretty.render_event(&call).expect("replay call");
        let mid_replay = captured_str(&replay_buf);
        assert!(
            !mid_replay.contains("running..."),
            "replay must NOT emit the running indicator: {mid_replay:?}",
        );
        replay_pretty.render_event(&result).expect("replay result");
        let replay_final = captured_str(&replay_buf);
        assert!(
            replay_final.contains("3.0s"),
            "replay duration from ts_ms delta: {replay_final:?}",
        );
    }

    /// Spec criterion `test_logs_reuses_renderer` — the `Renderer`
    /// trait + impls are reused between `loom run` and `loom logs`. We
    /// pin this by deserializing events from a JSONL line (the on-disk
    /// shape `loom logs` reads) and feeding them through the same
    /// `build_renderer` path `loom run` uses. The renderer trait
    /// object is the only contract both share.
    #[test]
    fn logs_reuses_renderer_via_jsonl_round_trip() {
        let bead = BeadId::new("wx-1").expect("id");
        let (call, result) = tool_pair_with_ts(
            "t1",
            "Bash",
            serde_json::json!({"command": "echo hi"}),
            0,
            2_000,
        );
        // Round-trip through JSONL — same path `loom logs` would take
        // when replaying a saved log file.
        let lines = [
            serde_json::to_string(&call).expect("ser call"),
            serde_json::to_string(&result).expect("ser result"),
        ];
        let replayed: Vec<AgentEvent> = lines
            .iter()
            .map(|s| serde_json::from_str::<AgentEvent>(s).expect("deser"))
            .collect();

        // Build via `build_renderer` (the public selection function
        // shared by `loom run` and `loom logs`) with `live=false`.
        let (buf, writer) = captured();
        let mut r = build_renderer(RenderMode::Plain, Box::new(writer), bead, false, false);
        for ev in &replayed {
            r.render_event(ev).expect("render");
        }
        r.finish(BeadOutcome::Done, Duration::from_secs(2))
            .expect("finish");
        let out = captured_str(&buf);
        assert!(out.contains("Bash"), "{out:?}");
        // Replay path emits a closing glyph + ts_ms-derived duration
        // for the pair.
        assert!(out.contains("2.0s"), "{out:?}");
    }

    fn driver_event(kind: &str, summary: &str, payload: serde_json::Value) -> AgentEvent {
        AgentEvent::DriverEvent {
            envelope: EventEnvelope {
                source: loom_events::Source::Driver,
                ..sample_envelope()
            },
            driver_kind: DriverKind::from_wire(kind),
            summary: summary.to_string(),
            payload,
        }
    }

    /// `driver_event` variants render with the `→` arrow glyph followed by
    /// `<driver_kind>: <summary>`. Pins the rendered shape for every
    /// known driver_kind the spec enumerates.
    #[test]
    fn driver_event_renders_arrow_glyph() {
        for kind in [
            "verdict_gate",
            "retry_dispatch",
            "push_gate_walk",
            "push_gate_refuse",
            "push_gate_clean",
            "container_spawn",
            "container_oom",
            "infra_failure",
        ] {
            let out = capture(RenderMode::Default, false, false, |r| {
                r.render_event(&driver_event(kind, "summary text", json!({})))
                    .expect("render");
            });
            assert!(
                out.contains('→'),
                "driver_event must render with `→` glyph for kind={kind}: {out:?}",
            );
            assert!(
                out.contains(kind),
                "driver_event line must include kind {kind}: {out:?}",
            );
            assert!(
                out.contains("summary text"),
                "driver_event line must include summary: {out:?}",
            );
        }
    }

    /// Unknown `driver_kind` strings — new event types added without
    /// schema bumps — render as the same generic `→ <kind>: <summary>`
    /// fallback as known kinds. Renderer never errors on an unrecognized
    /// driver_kind.
    #[test]
    fn unknown_driver_kind_renders_generic_arrow_summary() {
        let out = capture(RenderMode::Default, false, false, |r| {
            r.render_event(&driver_event(
                "totally_new_driver_kind",
                "future event body",
                json!({"detail": 7}),
            ))
            .expect("render");
        });
        assert!(out.contains('→'), "unknown kind needs arrow glyph: {out:?}");
        assert!(
            out.contains("totally_new_driver_kind: future event body"),
            "fallback line is `<kind>: <summary>`: {out:?}",
        );
    }

    /// Driver events render in `Parallel` mode with the bead-id prefix
    /// just like tool-call lines, so multi-bead UIs attribute the event
    /// to its source bead.
    #[test]
    fn driver_event_parallel_mode_prefixes_bead_id() {
        let out = capture(RenderMode::Default, true, false, |r| {
            r.render_event(&driver_event(
                "push_gate_walk",
                "evaluating verdict",
                json!({}),
            ))
            .expect("render");
        });
        assert!(out.contains("[wx-1]"), "{out:?}");
        assert!(out.contains("→"), "{out:?}");
        assert!(out.contains("push_gate_walk"), "{out:?}");
    }

    // -- Summary-cell overflow tests ---------------------------------------

    /// Run a `TerminalRenderer` callback against a captured buffer with
    /// a pinned terminal width — the overflow tests depend on knowing
    /// exactly how many columns are available without relying on the
    /// host pty.
    fn capture_with_width<F>(parallel: bool, color: bool, width: usize, live: bool, f: F) -> String
    where
        F: FnOnce(&mut TerminalRenderer),
    {
        let buf: Vec<u8> = Vec::new();
        let cell = std::sync::Arc::new(std::sync::Mutex::new(buf));
        let cell_for_writer = cell.clone();
        struct Sink {
            inner: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
        }
        impl Write for Sink {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.inner
                    .lock()
                    .map_err(|_| io::Error::other("poisoned"))?
                    .extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let sink = Sink {
            inner: cell_for_writer,
        };
        let renderer = if live {
            TerminalRenderer::new(
                sink,
                RenderMode::Default,
                BeadId::new("wx-1").expect("id"),
                parallel,
                color,
            )
        } else {
            TerminalRenderer::new_replay(
                sink,
                RenderMode::Default,
                BeadId::new("wx-1").expect("id"),
                parallel,
                color,
            )
        };
        let mut r = renderer.with_term_width(width);
        f(&mut r);
        let guard = cell.lock().expect("not poisoned");
        String::from_utf8(guard.clone()).expect("utf-8")
    }

    /// A short Bash command with the indicator on (live, color, single-
    /// bead) lays the summary cell + right-edge column on one line — the
    /// "fits as today" case from the spec. Terminal width is wide enough
    /// to hold the full single-line form, so no continuation appears.
    #[test]
    fn summary_overflow_short_command_stays_single_line() {
        let out = capture_with_width(false, true, 120, true, |r| {
            let (call, result) = tool_pair_with_ts("t1", "Bash", json!({"command": "ls"}), 0, 500);
            r.render_event(&call).expect("call");
            r.render_event(&result).expect("result");
        });
        // Single-line layout: summary + spacer + right-edge column on
        // one row. The pair collapses into one printed line (one final
        // newline) after the \r overwrite.
        assert!(out.contains("Bash"), "{out:?}");
        assert!(out.contains("ls"), "{out:?}");
        assert!(out.contains("0.5s"), "{out:?}");
        let lines: Vec<&str> = out.split('\n').filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 1, "fits case must be single line: {out:?}");
        assert!(
            lines[0].contains("ls") && lines[0].contains("0.5s"),
            "summary and right-col must share the line: {out:?}",
        );
    }

    /// When the summary cell + right-edge column would overrun the
    /// terminal width, the renderer suppresses the in-place indicator
    /// and emits a two-line layout at result time: right-edge column
    /// alone on line 1 (right-aligned so the duration / glyph stays at
    /// the terminal edge), full summary on a body-indented continuation
    /// line beneath it.
    #[test]
    fn summary_overflow_wraps_to_continuation_line() {
        // 40-col terminal — narrow enough that a long Bash command can't
        // share a row with the right-edge column.
        let out = capture_with_width(false, true, 40, true, |r| {
            let (call, result) = tool_pair_with_ts(
                "t1",
                "Bash",
                json!({
                    "command": "cargo test --workspace --all-features",
                }),
                0,
                1_200,
            );
            r.render_event(&call).expect("call");
            r.render_event(&result).expect("result");
        });
        let lines: Vec<&str> = out.split('\n').filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            2,
            "overflow case must emit right-col line + continuation: {out:?}",
        );
        // Line 1: right-edge column right-aligned to the terminal width.
        // Line 2: full summary on the body-indented continuation line.
        assert!(
            lines[0].trim_end().ends_with("1.2s"),
            "line 1 must end with the duration / right-edge column: {out:?}",
        );
        assert!(
            lines[0].contains('✓'),
            "line 1 must carry the status glyph: {out:?}",
        );
        assert!(
            !lines[0].contains("cargo"),
            "line 1 must NOT carry the summary content: {out:?}",
        );
        assert!(
            lines[1].contains("cargo test --workspace --all-features"),
            "line 2 must carry the full summary: {out:?}",
        );
        // Right-edge column must stay within the terminal width — the
        // hard spec contract.
        let line1_width = display_width(lines[0]);
        assert!(
            line1_width <= 40,
            "right-edge column line must not exceed terminal width: {line1_width} > 40 in {out:?}",
        );
        // Continuation indent — the summary sits at one level deeper
        // than the top-level depth indent ("  " + "  " = 4 leading
        // spaces for a depth-0 call).
        assert!(
            lines[1].starts_with("    "),
            "continuation line must use the body indent (4 spaces for depth 0): {out:?}",
        );
    }

    /// Every per-tool summary cell goes through the same overflow path.
    /// Walk Read / Edit / Write / Grep / Glob / Bash / WebFetch /
    /// WebSearch / Task / a custom unknown tool and pin that the
    /// right-edge column survives intact when the left content blows
    /// past the terminal width.
    #[test]
    fn summary_overflow_applies_to_every_per_tool_cell() {
        let long = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let cases: Vec<(&str, serde_json::Value)> = vec![
            (
                "Read",
                json!({"file_path": format!("very/deep/path/to/some/{long}.rs")}),
            ),
            (
                "Edit",
                json!({
                    "file_path": format!("deep/{long}.rs"),
                    "old_string": "x\n",
                    "new_string": "y\n",
                }),
            ),
            (
                "Write",
                json!({"file_path": format!("deep/{long}.rs"), "content": "a"}),
            ),
            ("Grep", json!({"pattern": long, "path": "src"})),
            ("Glob", json!({"pattern": long, "path": "src"})),
            ("Bash", json!({"command": format!("echo {long}")})),
            (
                "WebFetch",
                json!({"url": format!("https://example.com/{long}")}),
            ),
            ("WebSearch", json!({"query": long})),
            (
                "Task",
                json!({"description": long, "subagent_type": "general-purpose"}),
            ),
            // Generic fallback path — unknown tools render as the tool
            // name verbatim, so the overflow trigger is a long name.
            (
                "ACustomUnknownToolWithAnExceptionallyLongIdentifier",
                json!({"anything": long}),
            ),
        ];
        for (tool, params) in cases {
            let mut env_a = sample_envelope();
            env_a.ts_ms = 0;
            let mut env_b = sample_envelope();
            env_b.ts_ms = 1_000;
            let call = AgentEvent::ToolCall {
                envelope: env_a,
                id: ToolCallId::new("t1"),
                tool: tool.to_string(),
                params,
                parent_tool_call_id: None,
            };
            let result = AgentEvent::ToolResult {
                envelope: env_b,
                id: ToolCallId::new("t1"),
                output: String::new(),
                is_error: false,
            };
            let out = capture_with_width(false, true, 40, true, |r| {
                r.render_event(&call).expect("call");
                r.render_event(&result).expect("result");
            });
            let lines: Vec<&str> = out.split('\n').filter(|l| !l.is_empty()).collect();
            assert!(
                lines.len() >= 2,
                "{tool} overflow must produce >=2 lines: {out:?}",
            );
            // Right-edge column visible on the first line and within
            // the terminal width.
            assert!(
                lines[0].contains('✓') && lines[0].contains("1.0s"),
                "{tool} right-edge column missing on line 1: {out:?}",
            );
            let line1_width = display_width(lines[0]);
            assert!(
                line1_width <= 40,
                "{tool} right-edge column line overflows terminal width \
                 ({line1_width} > 40): {out:?}",
            );
            // Tool name surfaces on the continuation line (for the
            // generic / custom case, this is just the tool string).
            assert!(
                lines[1].contains(tool),
                "{tool} continuation line must carry the tool name: {out:?}",
            );
        }
    }

    /// Once the in-place indicator decides the line fits, it must keep
    /// fitting for the duration of the call — i.e. the reserve covers
    /// the running counter's worst case. Pin this by checking that the
    /// initial `running... 0.0s` line never exceeds the terminal width
    /// when the renderer chose the single-line path.
    #[test]
    fn summary_overflow_indicator_reserve_covers_running_text() {
        // Choose a summary length that just barely fits the threshold
        // at 40 cols. With prefix=2, separator=3, reserve=17 the
        // summary budget is 40 - 22 = 18 cols. A `Bash   echo hi`
        // summary (15 cols) sits below that.
        let out = capture_with_width(false, true, 40, true, |r| {
            r.render_event(&AgentEvent::ToolCall {
                envelope: sample_envelope(),
                id: ToolCallId::new("t1"),
                tool: "Bash".to_string(),
                params: json!({"command": "echo hi"}),
                parent_tool_call_id: None,
            })
            .expect("call");
        });
        // The indicator opened — the line carries `running...` text
        // and stays within 40 columns including the right column.
        assert!(out.contains("running..."), "indicator must open: {out:?}");
        let line_width = display_width(out.trim_end_matches('\n'));
        assert!(
            line_width <= 40,
            "running line width {line_width} must fit in 40 cols: {out:?}",
        );
    }

    /// Buffered overflow plays well with sibling preemption: if a new
    /// tool call arrives before the result of the buffered one, the
    /// renderer flushes the buffered cell with `…` so the user sees the
    /// in-flight tool was preempted rather than silently dropping it.
    #[test]
    fn summary_overflow_preempts_buffered_call_on_sibling() {
        let out = capture_with_width(false, true, 40, true, |r| {
            // First call: long Bash → buffered.
            r.render_event(&AgentEvent::ToolCall {
                envelope: sample_envelope(),
                id: ToolCallId::new("t1"),
                tool: "Bash".to_string(),
                params: json!({"command": "echo this command is far too long for 40 cols"}),
                parent_tool_call_id: None,
            })
            .expect("first call");
            // Second call: short, fits — preempts the first.
            r.render_event(&AgentEvent::ToolCall {
                envelope: sample_envelope(),
                id: ToolCallId::new("t2"),
                tool: "Read".to_string(),
                params: json!({"file_path": "a"}),
                parent_tool_call_id: None,
            })
            .expect("second call");
        });
        assert!(
            out.contains('…'),
            "preempted buffered cell must carry the preempt glyph: {out:?}",
        );
        assert!(
            out.contains("echo this command is far too long for 40 cols"),
            "preempted summary must still print on the continuation line: {out:?}",
        );
    }
}

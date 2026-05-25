//! In-place running indicator — keeps the cursor on the tool's header
//! line and updates a duration counter via `\r` + clear-to-EOL until
//! the tool result arrives.
//!
//! ```text
//!   Bash   cargo test --lib                           running... 4.2s
//! ```
//!
//! When the result arrives, the same line is rewritten with the final
//! form. Auto-suppressed in non-TTY modes (`Plain` / `Json` / `Raw`)
//! and when `--parallel N > 1` (multiple `\r` regions don't compose).
//!
//! ## Cleanup contract
//!
//! Every exit path must clear the in-place region — leaving a dangling
//! `\r` produces garbled terminal output. [`RunningIndicator::end`]
//! emits the clear sequence; the renderer wires it into a panic hook
//! and tokio signal handler so cancellation / panic paths land cleanly.
//!
//! ## Cleanup discipline
//!
//! Cleanup-on-drop is **not** sufficient because a panic during a
//! held lock or a Tokio-cancelled future may not reliably run `Drop`.
//! Callers must call [`end`] explicitly from every exit path; the
//! renderer's outer shutdown sequence is the right place.

use std::io::{self, IsTerminal, Write};

/// The ANSI escape that clears from cursor to end-of-line. Used after
/// `\r` to wipe the previously-rendered indicator before writing the
/// updated text.
pub const CLEAR_TO_EOL: &str = "\x1b[K";

/// Probe stdout for TTY-attached + size-reachable state. The CLI flips
/// the in-place running indicator on when this returns `true` and the
/// user did not pass `--plain`/`--json`/`--raw`. We use crossterm's
/// `terminal::size` rather than just `IsTerminal::is_terminal` because
/// some CI runners attach a pseudo-tty but report a 0-width terminal —
/// the indicator's `\r` overwrite pattern requires a real width to
/// avoid mangling output.
pub fn stdout_supports_indicator() -> bool {
    if !io::stdout().is_terminal() {
        return false;
    }
    matches!(crossterm::terminal::size(), Ok((cols, _)) if cols > 0)
}

/// Active in-place running indicator. Constructed when a tool starts
/// running; the caller invokes [`tick`] to refresh the elapsed-time
/// display and [`end`] when the tool result arrives (or on any exit
/// path).
pub struct RunningIndicator<W: Write> {
    out: W,
    /// `true` after the first `tick` — distinguishes "first write" from
    /// "update". The first write is just the line; updates prepend
    /// `\r` + clear-to-EOL.
    written: bool,
    /// Whether output should actually emit escape sequences. False for
    /// non-TTY and parallel modes — `tick` becomes a no-op so the
    /// indicator silently disables itself per spec.
    enabled: bool,
}

impl<W: Write> RunningIndicator<W> {
    /// Construct with the writer the indicator owns and a flag for
    /// whether output should actually emit escape sequences. Pass
    /// `enabled = false` for Plain/Json/Raw modes or `parallel > 1`;
    /// the indicator becomes a silent no-op without further branching
    /// in callers.
    pub fn new(out: W, enabled: bool) -> Self {
        Self {
            out,
            written: false,
            enabled,
        }
    }

    /// Refresh the indicator with the current `text`. First call writes
    /// the text without a trailing newline; subsequent calls prepend
    /// `\r` + clear-to-EOL to overwrite the previous line.
    pub fn tick(&mut self, text: &str) -> io::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        if self.written {
            self.out.write_all(b"\r")?;
            self.out.write_all(CLEAR_TO_EOL.as_bytes())?;
        }
        self.out.write_all(text.as_bytes())?;
        self.out.flush()?;
        self.written = true;
        Ok(())
    }

    /// Clear the in-place region. Idempotent — safe to call from
    /// panic / signal handlers without checking state first. The
    /// caller follows up with the final non-running line.
    pub fn end(&mut self) -> io::Result<()> {
        if !self.enabled || !self.written {
            return Ok(());
        }
        self.out.write_all(b"\r")?;
        self.out.write_all(CLEAR_TO_EOL.as_bytes())?;
        self.out.flush()?;
        self.written = false;
        Ok(())
    }

    /// Consume the indicator and return its writer. Used when the
    /// caller wants to continue writing to the same stream after the
    /// indicator finishes.
    pub fn into_inner(self) -> W {
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_tick_writes_text_without_escape() {
        let mut buf: Vec<u8> = Vec::new();
        let mut r = RunningIndicator::new(&mut buf, true);
        r.tick("Bash    running... 0.1s").expect("tick");
        let s = String::from_utf8(buf).expect("utf-8");
        assert!(!s.contains('\r'), "first tick must not emit \\r: {s:?}");
        assert!(s.contains("Bash"), "text not present: {s:?}");
    }

    #[test]
    fn second_tick_overwrites_with_carriage_return_and_clear() {
        let mut buf: Vec<u8> = Vec::new();
        let mut r = RunningIndicator::new(&mut buf, true);
        r.tick("Bash    running... 0.1s").expect("first");
        r.tick("Bash    running... 1.2s").expect("second");
        let s = String::from_utf8(buf).expect("utf-8");
        assert!(s.contains('\r'), "second tick must emit \\r: {s:?}");
        assert!(
            s.contains(CLEAR_TO_EOL),
            "clear-to-EOL must follow \\r: {s:?}"
        );
        assert!(s.contains("1.2s"), "updated text must be present: {s:?}");
    }

    #[test]
    fn end_clears_the_region() {
        let mut buf: Vec<u8> = Vec::new();
        let mut r = RunningIndicator::new(&mut buf, true);
        r.tick("Bash    running... 0.5s").expect("tick");
        r.end().expect("end");
        let s = String::from_utf8(buf).expect("utf-8");
        let last_two_bytes = &s[s.len() - 4..];
        // Last bytes are \r + clear-to-EOL (no trailing text).
        assert!(last_two_bytes.contains('\r'));
        assert!(last_two_bytes.contains(CLEAR_TO_EOL));
    }

    #[test]
    fn end_is_idempotent() {
        let mut r = RunningIndicator::new(Vec::<u8>::new(), true);
        r.tick("running...").expect("tick");
        r.end().expect("first end");
        let after_first = r.into_inner();
        let after_first_len = after_first.len();
        let mut r = RunningIndicator::new(after_first, true);
        // Calling end on a fresh indicator (never ticked) must be a no-op.
        r.end().expect("second end");
        let final_buf = r.into_inner();
        assert_eq!(
            final_buf.len(),
            after_first_len,
            "second end on un-ticked indicator must not write again",
        );
    }

    /// When `enabled = false` (non-TTY or parallel modes), the
    /// indicator must write nothing. Pin this with a buffer that stays
    /// empty after `tick`+`end`.
    #[test]
    fn disabled_indicator_writes_nothing() {
        let mut buf: Vec<u8> = Vec::new();
        let mut r = RunningIndicator::new(&mut buf, false);
        r.tick("running... 1s").expect("tick");
        r.tick("running... 2s").expect("tick");
        r.end().expect("end");
        assert!(
            buf.is_empty(),
            "disabled indicator must write nothing: {buf:?}",
        );
    }
}

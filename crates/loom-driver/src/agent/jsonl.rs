use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::ChildStdout;

use super::error::ProtocolError;

/// Maximum size in bytes of a single JSONL line accepted from an agent.
///
/// Caps memory pressure from a malicious or malfunctioning agent that emits a
/// line without a `\n` terminator. 10 MB is well above any legitimate JSONL
/// message — the largest are tool results carrying file contents.
pub const MAX_LINE_BYTES: usize = 10 * 1024 * 1024;

/// Buffered line reader over an agent process's stdout.
///
/// Splits on `\n` (0x0A) only. Unicode line separators (U+2028, U+2029) pass
/// through as part of the JSON content. Trailing `\r` is stripped so CRLF
/// survives. Empty lines (blank between objects) are silently skipped — only
/// non-empty lines are returned to the caller.
pub struct JsonlReader {
    reader: BufReader<ChildStdout>,
    line_buf: String,
}

impl JsonlReader {
    /// Wrap a child stdout pipe in a fresh reader. Internally allocates a
    /// reusable line buffer that grows up to [`MAX_LINE_BYTES`].
    pub fn new(stdout: ChildStdout) -> Self {
        Self {
            reader: BufReader::new(stdout),
            line_buf: String::new(),
        }
    }

    /// Read the next non-empty line from the stream.
    ///
    /// Returns `Ok(None)` on EOF, `Ok(Some(line))` for each non-empty line
    /// (with the trailing `\n` and any preceding `\r` stripped), and
    /// [`ProtocolError::LineTooLong`] if a single line exceeds
    /// [`MAX_LINE_BYTES`].
    pub async fn next_line(&mut self) -> Result<Option<&str>, ProtocolError> {
        loop {
            self.line_buf.clear();
            let n = self.reader.read_line(&mut self.line_buf).await?;
            if n == 0 {
                return Ok(None);
            }
            if self.line_buf.len() > MAX_LINE_BYTES {
                return Err(ProtocolError::LineTooLong {
                    len: self.line_buf.len(),
                    max: MAX_LINE_BYTES,
                });
            }
            let trimmed_len = self.line_buf.trim_end_matches(['\n', '\r']).len();
            self.line_buf.truncate(trimmed_len);
            if !self.line_buf.is_empty() {
                return Ok(Some(self.line_buf.as_str()));
            }
        }
    }
}

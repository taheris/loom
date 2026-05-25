//! Per-tool summary cells + body formatters.
//!
//! Each builtin tool gets a tailored one-line summary cell; verbose
//! mode (`-v`) widens the body cap to "full content". The default
//! mode caps body at **10 lines or 2 KB whichever first** with a
//! recovery hint line.
//!
//! ## Summary cells (per spec table)
//!
//! | Tool      | Cell                                                |
//! |-----------|-----------------------------------------------------|
//! | Read      | `<path>:<line-range>   <N> lines`                   |
//! | Edit      | `<path>   +<add> -<del>   diff↓`                    |
//! | Write     | `<path>   +<lines>   new file`                      |
//! | Grep      | `"<pattern>" in <path>   <N> files`                 |
//! | Glob      | `"<pattern>" in <path>   <N> files`                 |
//! | Bash      | `<cmd-truncated>   <duration> <✓|✗ exit=N>`         |
//! | WebFetch  | `<url>   <bytes> <duration> <✓|✗>`                  |
//! | WebSearch | `"<query>"   <N> results`                           |
//! | Task      | `<description>   [agent:<subagent>]   <duration>`   |
//!
//! Unknown tools fall through to a generic `<tool>` cell.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::osc8;

/// Body cap policy for the default render mode.
pub const BODY_CAP_LINES: usize = 10;
pub const BODY_CAP_BYTES: usize = 2048;

/// OSC 8 wrapping context for summary cells. When `supported = true`,
/// path-bearing tools (Read/Edit/Write/Grep/WebFetch) wrap the path or
/// URL in the OSC 8 hyperlink escape so cmd-click opens the editor or
/// browser. `cwd` is the workspace root used to compute absolute
/// `file://` URLs from relative paths and to normalize absolute
/// workspace paths to repo-relative form in the displayed summary
/// cell.
///
/// Constructed once per renderer at session start — environment
/// detection lives at the call site, not inside this struct, so tests
/// can pin both branches without env mutation.
#[derive(Debug, Clone)]
pub struct Osc8Context {
    pub supported: bool,
    pub cwd: PathBuf,
}

impl Osc8Context {
    /// OSC 8 wrapping suppressed — every `wrap` call returns the
    /// display string unchanged. Used by non-Pretty render modes and
    /// the default case when no terminal capability has been probed.
    pub fn disabled() -> Self {
        Self {
            supported: false,
            cwd: PathBuf::new(),
        }
    }

    /// OSC 8 wrapping active. `cwd` is the workspace root, used to
    /// turn relative paths into absolute `file://` URLs so cmd-click
    /// resolves correctly regardless of the terminal's working dir.
    pub fn enabled(cwd: PathBuf) -> Self {
        Self {
            supported: true,
            cwd,
        }
    }

    /// Set the workspace root on a context built via [`disabled`]. Used
    /// when OSC 8 is unsupported but path normalization should still
    /// strip the workspace prefix from absolute paths in summary cells.
    pub fn with_cwd(mut self, cwd: PathBuf) -> Self {
        self.cwd = cwd;
        self
    }
}

impl Default for Osc8Context {
    fn default() -> Self {
        Self::disabled()
    }
}

/// Build the one-line summary cell for a tool call. `tool` is the
/// builtin name; `params` is the call's argument JSON; `osc8` controls
/// hyperlink wrapping for path-bearing cells. Pure function so tests
/// can pin per-tool shape without the renderer state.
pub fn summary_cell(tool: &str, params: &Value, osc8: &Osc8Context) -> String {
    match tool {
        "Read" => read_summary(params, osc8),
        "Edit" => edit_summary(params, osc8),
        "Write" => write_summary(params, osc8),
        "Grep" => grep_or_glob_summary(tool, params, osc8),
        "Glob" => grep_or_glob_summary(tool, params, osc8),
        "Bash" => bash_summary(params),
        "WebFetch" => webfetch_summary(params, osc8),
        "WebSearch" => websearch_summary(params),
        "Task" => task_summary(params),
        other => other.to_string(),
    }
}

fn read_summary(params: &Value, osc8: &Osc8Context) -> String {
    let path = params
        .get("file_path")
        .and_then(Value::as_str)
        .unwrap_or("");
    let offset = params.get("offset").and_then(Value::as_i64);
    let limit = params.get("limit").and_then(Value::as_i64);
    let range = match (offset, limit) {
        (Some(o), Some(l)) => format!(":{o}-{}", o + l),
        (Some(o), None) => format!(":{o}-"),
        _ => String::new(),
    };
    let display_path = normalize_for_display(&osc8.cwd, path);
    let display = format!("{display_path}{range}");
    let line = offset.and_then(|o| u32::try_from(o).ok());
    let wrapped = wrap_path(osc8, path, line, &display);
    format!("Read   {wrapped}")
}

fn edit_summary(params: &Value, osc8: &Osc8Context) -> String {
    let path = params
        .get("file_path")
        .and_then(Value::as_str)
        .unwrap_or("");
    let old = params
        .get("old_string")
        .and_then(Value::as_str)
        .unwrap_or("");
    let new = params
        .get("new_string")
        .and_then(Value::as_str)
        .unwrap_or("");
    let (add, del) = diff_counts(old, new);
    let display = normalize_for_display(&osc8.cwd, path);
    let wrapped = wrap_path(osc8, path, None, &display);
    format!("Edit   {wrapped}   +{add} -{del}   diff↓")
}

fn write_summary(params: &Value, osc8: &Osc8Context) -> String {
    let path = params
        .get("file_path")
        .and_then(Value::as_str)
        .unwrap_or("");
    let content = params.get("content").and_then(Value::as_str).unwrap_or("");
    let lines = content.lines().count();
    let display = normalize_for_display(&osc8.cwd, path);
    let wrapped = wrap_path(osc8, path, None, &display);
    format!("Write   {wrapped}   +{lines}   new file")
}

fn grep_or_glob_summary(tool: &str, params: &Value, osc8: &Osc8Context) -> String {
    let pattern = params.get("pattern").and_then(Value::as_str).unwrap_or("");
    let path = params.get("path").and_then(Value::as_str).unwrap_or("");
    let display = normalize_for_display(&osc8.cwd, path);
    let wrapped = wrap_path(osc8, path, None, &display);
    format!("{tool}   \"{pattern}\" in {wrapped}")
}

fn bash_summary(params: &Value) -> String {
    let cmd = params.get("command").and_then(Value::as_str).unwrap_or("");
    let truncated = truncate(cmd, 60);
    format!("Bash   {truncated}")
}

fn webfetch_summary(params: &Value, osc8: &Osc8Context) -> String {
    let url = params.get("url").and_then(Value::as_str).unwrap_or("");
    let wrapped = if osc8.supported && !url.is_empty() {
        osc8::wrap(url, url, true)
    } else {
        url.to_string()
    };
    format!("WebFetch   {wrapped}")
}

fn wrap_path(osc8: &Osc8Context, path: &str, line: Option<u32>, display: &str) -> String {
    if !osc8.supported || path.is_empty() {
        return display.to_string();
    }
    let url = osc8::file_url(Path::new(&osc8.cwd), path, line);
    osc8::wrap(&url, display, true)
}

/// Strip the workspace-root prefix from `path` for display purposes
/// only — the agent's invocation still uses the absolute form. Returns
/// the input unchanged when `cwd` is empty, when `path` is not absolute,
/// or when `path` does not start with `cwd`.
pub fn normalize_for_display(cwd: &Path, path: &str) -> String {
    if path.is_empty() || cwd.as_os_str().is_empty() {
        return path.to_string();
    }
    let abs = Path::new(path);
    if !abs.is_absolute() {
        return path.to_string();
    }
    match abs.strip_prefix(cwd) {
        Ok(rel) => {
            let rel_str = rel.to_string_lossy();
            if rel_str.is_empty() {
                ".".to_string()
            } else {
                rel_str.into_owned()
            }
        }
        Err(_) => path.to_string(),
    }
}

fn websearch_summary(params: &Value) -> String {
    let query = params.get("query").and_then(Value::as_str).unwrap_or("");
    format!("WebSearch   \"{query}\"")
}

fn task_summary(params: &Value) -> String {
    let description = params
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");
    let subagent = params
        .get("subagent_type")
        .and_then(Value::as_str)
        .unwrap_or("");
    format!("Task   {description}   [agent:{subagent}]")
}

/// Compute `(added, removed)` line counts between two strings via
/// imara-diff. Used by Edit summary cells.
pub fn diff_counts(old: &str, new: &str) -> (usize, usize) {
    use imara_diff::intern::InternedInput;
    use imara_diff::sink::Sink;
    use imara_diff::{Algorithm, diff};

    /// Accumulates added/removed line counts across every hunk.
    struct CountingSink {
        added: usize,
        removed: usize,
    }
    impl Sink for CountingSink {
        type Out = (usize, usize);
        fn process_change(&mut self, before: std::ops::Range<u32>, after: std::ops::Range<u32>) {
            self.added += (after.end - after.start) as usize;
            self.removed += (before.end - before.start) as usize;
        }
        fn finish(self) -> Self::Out {
            (self.added, self.removed)
        }
    }

    let input = InternedInput::new(old, new);
    diff(
        Algorithm::Histogram,
        &input,
        CountingSink {
            added: 0,
            removed: 0,
        },
    )
}

/// Truncate `s` to `max` chars, appending `…` if it exceeded.
pub fn truncate(s: &str, max: usize) -> String {
    let cleaned: String = s.chars().take_while(|c| *c != '\n').collect();
    if cleaned.chars().count() <= max {
        cleaned
    } else {
        let mut out: String = cleaned.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Cap a body to 10 lines or 2 KB (whichever first). When the cap
/// trims content, appends the spec-defined recovery hint as the final
/// line so the user knows where to find the full body.
pub fn cap_body(body: &str, bead_id: &str, tool_call_id: &str) -> String {
    let mut total_bytes = 0;
    let mut kept: Vec<String> = Vec::new();
    let mut truncated = false;
    for (i, line) in body.lines().enumerate() {
        let next_bytes = total_bytes + line.len() + 1;
        if i >= BODY_CAP_LINES || next_bytes > BODY_CAP_BYTES {
            truncated = true;
            break;
        }
        kept.push(line.to_string());
        total_bytes = next_bytes;
    }
    if truncated {
        let remaining = body.lines().count() - kept.len();
        kept.push(String::new());
        kept.push(format!(
            "  [{remaining} more lines — loom logs -b {bead_id} --tool {tool_call_id}]"
        ));
    }
    kept.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn disabled() -> Osc8Context {
        Osc8Context::disabled()
    }

    #[test]
    fn read_summary_includes_path_and_range() {
        let cell = summary_cell(
            "Read",
            &json!({"file_path": "src/lib.rs", "offset": 10, "limit": 20}),
            &disabled(),
        );
        assert!(cell.contains("src/lib.rs"), "{cell}");
        assert!(cell.contains(":10-30"), "{cell}");
    }

    #[test]
    fn edit_summary_includes_added_removed_counts() {
        let cell = summary_cell(
            "Edit",
            &json!({
                "file_path": "src/lib.rs",
                "old_string": "fn old() {}\n",
                "new_string": "fn new() {}\nfn extra() {}\n",
            }),
            &disabled(),
        );
        assert!(cell.contains("src/lib.rs"));
        assert!(cell.contains("+"));
        assert!(cell.contains("-"));
        assert!(cell.contains("diff"));
    }

    #[test]
    fn write_summary_includes_path_and_line_count() {
        let cell = summary_cell(
            "Write",
            &json!({"file_path": "src/new.rs", "content": "a\nb\nc\n"}),
            &disabled(),
        );
        assert!(cell.contains("src/new.rs"));
        assert!(cell.contains("+3"));
        assert!(cell.contains("new file"));
    }

    #[test]
    fn bash_summary_truncates_long_commands() {
        let cmd = "echo ".to_owned() + &"x".repeat(100);
        let cell = summary_cell("Bash", &json!({"command": cmd}), &disabled());
        assert!(cell.contains("Bash"));
        // 60-char cap leaves room for the leading "Bash   "
        let body_part = cell.trim_start_matches("Bash").trim();
        assert!(body_part.ends_with('…'), "{cell}");
    }

    #[test]
    fn task_summary_includes_subagent_label() {
        let cell = summary_cell(
            "Task",
            &json!({
                "description": "Review changes",
                "subagent_type": "code-reviewer",
            }),
            &disabled(),
        );
        assert!(cell.contains("Task"));
        assert!(cell.contains("Review changes"));
        assert!(cell.contains("[agent:code-reviewer]"));
    }

    #[test]
    fn unknown_tool_falls_through_to_name() {
        assert_eq!(
            summary_cell("CustomTool", &json!({}), &disabled()),
            "CustomTool",
        );
    }

    /// Read/Edit/Write/Grep paths wrap in OSC 8 when supported. The
    /// display text stays the same; the escape brackets it.
    #[test]
    fn enabled_osc8_wraps_read_path() {
        let ctx = Osc8Context::enabled(PathBuf::from("/workspace"));
        let cell = summary_cell(
            "Read",
            &json!({"file_path": "src/lib.rs", "offset": 42, "limit": 0}),
            &ctx,
        );
        // OSC 8 wrap brackets the path; the start marker carries the
        // file URL with the line-number fragment from `offset`.
        assert!(
            cell.contains("\x1b]8;;file:///workspace/src/lib.rs#L42"),
            "{cell:?}"
        );
        assert!(cell.contains("src/lib.rs"), "{cell:?}");
        // BEL terminator after the display string.
        assert!(cell.contains("\x07"), "{cell:?}");
    }

    /// WebFetch URL wraps the same URL as both target and display so
    /// cmd-click opens the browser at the visible URL.
    #[test]
    fn enabled_osc8_wraps_webfetch_url() {
        let ctx = Osc8Context::enabled(PathBuf::from("/workspace"));
        let cell = summary_cell(
            "WebFetch",
            &json!({"url": "https://example.com/page"}),
            &ctx,
        );
        assert!(
            cell.contains("\x1b]8;;https://example.com/page"),
            "{cell:?}",
        );
    }

    /// `normalize_for_display` strips a leading workspace prefix so
    /// summary cells render repo-relative paths. The agent still uses
    /// the absolute form internally; this is display-only.
    #[test]
    fn normalize_for_display_strips_workspace_prefix() {
        let cwd = PathBuf::from("/workspace");
        assert_eq!(
            normalize_for_display(&cwd, "/workspace/src/lib.rs"),
            "src/lib.rs",
        );
        assert_eq!(
            normalize_for_display(&cwd, "/workspace/tests/foo.rs"),
            "tests/foo.rs",
        );
    }

    /// Already-relative paths pass through unchanged. Used by the
    /// agent's own relative-path calls.
    #[test]
    fn normalize_for_display_passes_relative_paths_through() {
        let cwd = PathBuf::from("/workspace");
        assert_eq!(normalize_for_display(&cwd, "src/lib.rs"), "src/lib.rs");
    }

    /// Paths outside the workspace render as-is (no aggressive "../"
    /// rewriting; absolute paths the agent uses for /tmp or system
    /// files stay readable).
    #[test]
    fn normalize_for_display_keeps_non_workspace_paths() {
        let cwd = PathBuf::from("/workspace");
        assert_eq!(normalize_for_display(&cwd, "/tmp/x.rs"), "/tmp/x.rs");
        assert_eq!(normalize_for_display(&cwd, "/etc/hosts"), "/etc/hosts");
    }

    /// Empty cwd disables normalization. Used by the `disabled`
    /// context when no workspace root has been configured.
    #[test]
    fn normalize_for_display_passthrough_when_cwd_empty() {
        let cwd = PathBuf::new();
        assert_eq!(
            normalize_for_display(&cwd, "/workspace/src/lib.rs"),
            "/workspace/src/lib.rs",
        );
    }

    /// Summary cells emit repo-relative paths even when the agent
    /// passes the absolute `/workspace/...` form. Pin the end-to-end
    /// path through `summary_cell` for the Read tool.
    #[test]
    fn read_summary_normalizes_absolute_workspace_path() {
        let ctx = Osc8Context::disabled().with_cwd(PathBuf::from("/workspace"));
        let cell = summary_cell("Read", &json!({"file_path": "/workspace/src/lib.rs"}), &ctx);
        assert!(cell.contains("src/lib.rs"), "{cell:?}");
        assert!(
            !cell.contains("/workspace/"),
            "absolute workspace prefix must be stripped: {cell:?}",
        );
    }

    /// Edit/Write/Grep cells also normalize. Walk each one with the
    /// same /workspace/... input to pin the consistent contract.
    #[test]
    fn edit_write_grep_summaries_normalize_paths() {
        let ctx = Osc8Context::disabled().with_cwd(PathBuf::from("/workspace"));
        for (tool, params) in [
            (
                "Edit",
                json!({
                    "file_path": "/workspace/a.rs",
                    "old_string": "x\n",
                    "new_string": "y\n",
                }),
            ),
            (
                "Write",
                json!({"file_path": "/workspace/a.rs", "content": "x"}),
            ),
            ("Grep", json!({"pattern": "TODO", "path": "/workspace/src"})),
        ] {
            let cell = summary_cell(tool, &params, &ctx);
            assert!(
                !cell.contains("/workspace/"),
                "{tool} cell still carries absolute workspace prefix: {cell:?}",
            );
        }
    }

    /// When OSC 8 is enabled, the URL must keep the absolute path
    /// (cmd-click needs to resolve it without depending on the
    /// terminal's cwd) but the display text is the relative form.
    #[test]
    fn osc8_enabled_keeps_absolute_url_with_relative_display() {
        let ctx = Osc8Context::enabled(PathBuf::from("/workspace"));
        let cell = summary_cell("Read", &json!({"file_path": "/workspace/src/lib.rs"}), &ctx);
        assert!(
            cell.contains("\x1b]8;;file:///workspace/src/lib.rs"),
            "OSC 8 URL must keep absolute path: {cell:?}",
        );
        // The display text after the BEL terminator is relative.
        assert!(
            cell.contains("\x07src/lib.rs\x1b]8;;"),
            "display text must be repo-relative: {cell:?}",
        );
    }

    /// Disabled context returns plain text — no escape bytes.
    #[test]
    fn disabled_osc8_leaves_text_unchanged() {
        let cell = summary_cell(
            "Read",
            &json!({"file_path": "src/lib.rs", "offset": 42, "limit": 0}),
            &disabled(),
        );
        assert!(!cell.contains('\x1b'), "no escapes when disabled: {cell:?}");
    }

    #[test]
    fn diff_counts_tracks_simple_replacement() {
        // imara-diff in histogram mode counts the changed-line spans.
        // A single-line replacement reports (1, 1).
        let (a, d) = diff_counts("old line\n", "new line\n");
        assert_eq!((a, d), (1, 1));
    }

    #[test]
    fn diff_counts_handles_pure_additions() {
        let (a, d) = diff_counts("a\n", "a\nb\nc\n");
        assert_eq!((a, d), (2, 0));
    }

    #[test]
    fn cap_body_keeps_short_bodies_unchanged() {
        let body = "line 1\nline 2\nline 3";
        assert_eq!(cap_body(body, "wx-1", "tc-1"), body);
    }

    #[test]
    fn cap_body_truncates_long_bodies_with_recovery_hint() {
        let body: String = (1..=20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = cap_body(&body, "wx-1", "tc-1");
        // 10 lines kept + blank + hint = 12 lines
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 12, "{out}");
        assert!(lines[0].starts_with("line 1"));
        assert!(lines[9].starts_with("line 10"));
        assert!(lines[10].is_empty(), "{out}");
        assert!(lines[11].contains("10 more lines"), "{out}");
        assert!(lines[11].contains("loom logs -b wx-1 --tool tc-1"), "{out}");
    }

    #[test]
    fn cap_body_respects_byte_cap() {
        // 5 lines of 600 bytes each = 3000 bytes > 2048 cap.
        let body: String = (1..=5)
            .map(|_| "x".repeat(600))
            .collect::<Vec<_>>()
            .join("\n");
        let out = cap_body(&body, "wx-1", "tc-1");
        // Only ~3 lines fit before the byte cap trips.
        assert!(out.contains("more lines"), "{out}");
        assert!(out.len() < body.len(), "{out}");
    }
}

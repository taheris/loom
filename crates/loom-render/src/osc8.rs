//! OSC 8 hyperlinks — wrap link targets in the terminal-hyperlink
//! escape sequence so cmd-click in supporting terminals opens the
//! target.
//!
//! OSC 8 format: `\x1b]8;;<url>\x07<display>\x1b]8;;\x07`. The empty
//! parameter slot (`;;`) is for hyperlink ids and is unused here. The
//! `\x07` (BEL) terminator is what most terminals accept; ST (`\x1b\\`)
//! also works but is uglier in non-supporting terminals.
//!
//! Capability detection is best-effort and silent: when the terminal
//! doesn't support OSC 8, [`wrap`] returns the display string unchanged
//! so output degrades to plain text with no warning.

use std::path::Path;

const OSC: &str = "\x1b]";
const BEL: &str = "\x07";

/// Detect whether the current terminal supports OSC 8 hyperlinks.
/// Reads `TERM_PROGRAM` (set by iTerm2, VS Code, WezTerm, Apple
/// Terminal, etc.) and a small allowlist of `TERM` values
/// (`kitty`, `xterm-kitty`, `alacritty`). Pure inputs so tests pin
/// behavior without env mutation.
pub fn supports_osc8(term_program: Option<&str>, term: Option<&str>) -> bool {
    if let Some(tp) = term_program {
        // Known programs that ship OSC 8 support. Apple Terminal is
        // notably absent — it claims to support links but renders the
        // escape as garbage on older macOS versions.
        if matches!(tp, "iTerm.app" | "vscode" | "WezTerm" | "ghostty" | "kitty") {
            return true;
        }
    }
    if let Some(t) = term
        && (t.starts_with("kitty") || t == "xterm-kitty" || t.starts_with("alacritty"))
    {
        return true;
    }
    false
}

/// Wrap `display` in an OSC 8 escape pointing at `url`. When OSC 8 is
/// unsupported, returns `display` unchanged so callers can use the
/// result without branching.
pub fn wrap(url: &str, display: &str, supported: bool) -> String {
    if !supported || url.is_empty() {
        return display.to_string();
    }
    format!("{OSC}8;;{url}{BEL}{display}{OSC}8;;{BEL}")
}

/// Build a `file://` URL with an optional line-fragment for the
/// editor. Cmd-click on iTerm2 / VS Code / WezTerm jumps to that
/// line. `cwd` is the workspace root so relative paths resolve.
pub fn file_url(cwd: &Path, rel_or_abs: &str, line: Option<u32>) -> String {
    let abs = if Path::new(rel_or_abs).is_absolute() {
        std::path::PathBuf::from(rel_or_abs)
    } else {
        cwd.join(rel_or_abs)
    };
    let path = abs.to_string_lossy();
    match line {
        Some(n) => format!("file://{path}#L{n}"),
        None => format!("file://{path}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_osc8_detects_known_programs() {
        assert!(supports_osc8(Some("iTerm.app"), None));
        assert!(supports_osc8(Some("vscode"), None));
        assert!(supports_osc8(Some("WezTerm"), None));
        assert!(supports_osc8(None, Some("kitty")));
        assert!(supports_osc8(None, Some("xterm-kitty")));
        assert!(supports_osc8(None, Some("alacritty")));
    }

    #[test]
    fn supports_osc8_rejects_unknown_terminals() {
        assert!(!supports_osc8(None, None));
        assert!(!supports_osc8(Some("Apple_Terminal"), None));
        assert!(!supports_osc8(None, Some("xterm")));
        assert!(!supports_osc8(None, Some("vt100")));
    }

    #[test]
    fn wrap_returns_display_unchanged_when_unsupported() {
        assert_eq!(wrap("file:///a", "src/lib.rs", false), "src/lib.rs");
    }

    #[test]
    fn wrap_emits_osc8_escape_when_supported() {
        let out = wrap("file:///a/b.rs", "b.rs:42", true);
        // OSC start
        assert!(out.starts_with("\x1b]8;;file:///a/b.rs\x07"), "{out:?}");
        // Display text in the middle
        assert!(out.contains("\x07b.rs:42\x1b]8;;"), "{out:?}");
        // BEL terminator at the end
        assert!(out.ends_with("\x07"), "{out:?}");
    }

    #[test]
    fn wrap_with_empty_url_returns_display_unchanged() {
        assert_eq!(wrap("", "src/lib.rs", true), "src/lib.rs");
    }

    #[test]
    fn file_url_uses_absolute_path() {
        let cwd = Path::new("/workspace");
        assert_eq!(
            file_url(cwd, "src/lib.rs", None),
            "file:///workspace/src/lib.rs"
        );
        assert_eq!(
            file_url(cwd, "src/lib.rs", Some(42)),
            "file:///workspace/src/lib.rs#L42",
        );
    }

    #[test]
    fn file_url_preserves_absolute_input() {
        let cwd = Path::new("/workspace");
        assert_eq!(
            file_url(cwd, "/tmp/file.rs", Some(3)),
            "file:///tmp/file.rs#L3",
        );
    }
}

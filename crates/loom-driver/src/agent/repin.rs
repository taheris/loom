use serde::{Deserialize, Serialize};

/// Re-pin payload for delivery via pi's `steer` command on
/// `compaction_start`. Claude's compaction recovery flows through
/// [`ScratchSession`]'s `repin.sh`, which reads `prompt.txt` and
/// `scratch.md` from the scratch dir at run time, so the on-disk
/// envelope is written there — not here.
///
/// [`ScratchSession`]: crate::scratch::ScratchSession
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RePinContent {
    /// Short orientation banner (label, command, mode) — first thing the
    /// agent reads after compaction.
    pub orientation: String,

    /// Stable context block — spec path, exit signals, companions index.
    pub pinned_context: String,

    /// Additional partial bodies (template snippets, role-specific guidance)
    /// appended verbatim. May be empty.
    pub partial_bodies: Vec<String>,
}

impl RePinContent {
    /// Render the re-pin payload as a single prompt string for delivery via
    /// pi's `steer` command.
    ///
    /// The format is stable: orientation, blank line, pinned context, then
    /// each partial body separated by a blank line. No backend-specific
    /// framing — the caller wraps this in the appropriate command envelope.
    ///
    /// ```
    /// use loom_driver::agent::RePinContent;
    ///
    /// let r = RePinContent {
    ///     orientation: "loom run @ wx-1".to_string(),
    ///     pinned_context: "Spec: specs/harness.md".to_string(),
    ///     partial_bodies: vec!["partial alpha".to_string(), "partial beta".to_string()],
    /// };
    /// let prompt = r.to_prompt();
    /// assert!(prompt.starts_with("loom run @ wx-1"));
    /// assert!(prompt.contains("Spec: specs/harness.md"));
    /// assert!(prompt.contains("partial alpha"));
    /// assert!(prompt.contains("partial beta"));
    /// ```
    pub fn to_prompt(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.orientation);
        out.push_str("\n\n");
        out.push_str(&self.pinned_context);
        for body in &self.partial_bodies {
            out.push_str("\n\n");
            out.push_str(body);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_prompt_orders_orientation_context_partials() {
        let r = RePinContent {
            orientation: "ORI".to_string(),
            pinned_context: "CTX".to_string(),
            partial_bodies: vec!["P1".to_string(), "P2".to_string()],
        };
        let s = r.to_prompt();
        assert_eq!(s, "ORI\n\nCTX\n\nP1\n\nP2");
    }

    #[test]
    fn to_prompt_omits_partials_when_empty() {
        let r = RePinContent {
            orientation: "ORI".to_string(),
            pinned_context: "CTX".to_string(),
            partial_bodies: vec![],
        };
        assert_eq!(r.to_prompt(), "ORI\n\nCTX");
    }
}

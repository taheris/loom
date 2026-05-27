//! Tree-not-clean dispatcher helper (`specs/harness.md` §"Verdict Gate ·
//! Tree-clean check").
//!
//! After a worker emits `LOOM_COMPLETE` / `LOOM_NOOP` and the bead is
//! bd-closed, the driver runs `git status --porcelain` against the bead's
//! worktree BEFORE invoking the verify / review subprocesses — running
//! verifiers against a half-staged tree would conflate the agent's
//! intended diff with its leftover scratch. A non-empty porcelain result
//! routes to [`crate::review::RecoveryCause::TreeNotClean`] with cause
//! `tree-not-clean`, preceding `verify-fail` / `review-concern`.
//!
//! Path discipline: the dirty-path list is capped at [`TREE_NOT_CLEAN_CAP`]
//! entries by the driver before construction. When the underlying set is
//! larger, the capped list carries an extra `"+N more"` element as the
//! final entry; the render layer
//! (`loom_templates::run::PreviousFailure::TreeNotClean`) emits this
//! verbatim so the next attempt's agent sees the overflow count.

/// Hard cap on the dirty-path list carried by
/// [`crate::review::RecoveryCause::TreeNotClean`]. The 31st entry, if
/// present, is the `"+N more"` overflow marker — never a path.
pub const TREE_NOT_CLEAN_CAP: usize = 30;

/// Parse `git status --porcelain` output into the capped dirty-path list.
///
/// Each non-empty line of the input is one dirty entry (modified,
/// staged-but-uncommitted, or untracked outside the ignore set, per
/// porcelain v1 semantics). When the entry count exceeds
/// [`TREE_NOT_CLEAN_CAP`], the function keeps the first
/// `TREE_NOT_CLEAN_CAP` entries verbatim and appends a single `"+N more"`
/// element naming the overflow count. The render layer formats this as
/// the trailing suffix on the rendered path list.
///
/// Returns an empty `Vec` for clean trees (empty input or whitespace-only).
pub fn dirty_paths_from_porcelain(porcelain: &str) -> Vec<String> {
    let entries: Vec<String> = porcelain
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    if entries.len() <= TREE_NOT_CLEAN_CAP {
        return entries;
    }
    let overflow = entries.len() - TREE_NOT_CLEAN_CAP;
    let mut capped: Vec<String> = entries.into_iter().take(TREE_NOT_CLEAN_CAP).collect();
    capped.push(format!("+{overflow} more"));
    capped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_tree_yields_empty_vec() {
        assert!(dirty_paths_from_porcelain("").is_empty());
        assert!(dirty_paths_from_porcelain("\n\n\n").is_empty());
    }

    #[test]
    fn under_cap_passes_lines_through() {
        let porcelain = " M src/a.rs\n?? scratch.tmp\nA  src/new.rs";
        let dirty = dirty_paths_from_porcelain(porcelain);
        assert_eq!(
            dirty,
            vec![" M src/a.rs", "?? scratch.tmp", "A  src/new.rs"]
        );
        assert!(
            !dirty.iter().any(|p| p.starts_with('+')),
            "no overflow marker under the cap",
        );
    }

    #[test]
    fn exactly_cap_no_overflow_marker() {
        let porcelain = (0..TREE_NOT_CLEAN_CAP)
            .map(|i| format!(" M src/file_{i}.rs"))
            .collect::<Vec<_>>()
            .join("\n");
        let dirty = dirty_paths_from_porcelain(&porcelain);
        assert_eq!(dirty.len(), TREE_NOT_CLEAN_CAP);
        assert!(!dirty.iter().any(|p| p.starts_with('+')));
    }

    #[test]
    fn over_cap_appends_overflow_marker_with_count() {
        let total = TREE_NOT_CLEAN_CAP + 17;
        let porcelain = (0..total)
            .map(|i| format!(" M src/file_{i}.rs"))
            .collect::<Vec<_>>()
            .join("\n");
        let dirty = dirty_paths_from_porcelain(&porcelain);
        assert_eq!(dirty.len(), TREE_NOT_CLEAN_CAP + 1);
        assert_eq!(dirty[0], " M src/file_0.rs");
        assert_eq!(dirty[TREE_NOT_CLEAN_CAP - 1], " M src/file_29.rs");
        assert_eq!(dirty[TREE_NOT_CLEAN_CAP], "+17 more");
    }

    #[test]
    fn over_cap_preserves_first_entries_verbatim() {
        // Spec: the cap drops *later* entries, not random ones — the first
        // 30 must be carried through unchanged so the agent sees a stable
        // prefix of what it left dirty.
        let porcelain = (0..50)
            .map(|i| format!("?? scratch_{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let dirty = dirty_paths_from_porcelain(&porcelain);
        for (i, entry) in dirty.iter().take(TREE_NOT_CLEAN_CAP).enumerate() {
            assert_eq!(
                entry,
                &format!("?? scratch_{i}"),
                "first {TREE_NOT_CLEAN_CAP} entries preserved in order",
            );
        }
    }
}

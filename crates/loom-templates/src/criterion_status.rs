//! Typed `criterion_status` decomposition-evidence surface.
//!
//! `CriterionStatus` is the per-criterion record that gives `todo_*`
//! decomposition agents evidence of which Success-Criteria bullets already
//! pass before they fan out beads. The driver populates each row from the
//! gate's sqlite status cache and computes `commits_since` against the
//! current HEAD at prompt-render time. The struct + its [`CriterionResult`]
//! verdict enum are part of the `templates` public contract — consumers
//! writing decomposition-style tools reuse this shape against their own
//! caches.
//!
//! Shape follows `specs/templates.md` § Criterion-Status Surface. The
//! struct deliberately does not encode staleness thresholds; the
//! heuristic for what counts as a gap lives in
//! `partial/decomposition_discipline.md` so it can evolve without
//! changing the typed contract.

/// Per-criterion recency + verdict record threaded into `todo_new` /
/// `todo_update` contexts via `criterion_status`. Each row exposes the
/// annotation target the criterion declared plus the cached verifier
/// verdict and recency signals (timestamp, commit, commits-since-HEAD)
/// the agent uses to decide whether a criterion is already covered or
/// is a real gap worth a bead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CriterionStatus {
    /// Stable identifier for this criterion within the spec (e.g. the
    /// trailing fragment of its anchor in the rendered markdown). Format
    /// owned by the gate's status cache.
    pub criterion_anchor: String,

    /// The annotation target as the criterion declared it
    /// (`[check](...)`, `[test](...)`, `[system](...)`, `[judge](...)`).
    pub annotation: String,

    /// Last cached verdict for this criterion's verifier.
    pub last_result: CriterionResult,

    /// Unix-millis timestamp of the verifier run that produced
    /// `last_result`. `None` if no run has ever populated the cache.
    pub last_timestamp_ms: Option<i64>,

    /// Commit hash the cached result was recorded against. `None` when
    /// `last_result` is [`CriterionResult::NoResult`].
    pub last_commit: Option<String>,

    /// Number of commits between `last_commit` and the current HEAD
    /// (computed by the driver from `git rev-list --count`). `None`
    /// when `last_commit` is `None`.
    pub commits_since: Option<u32>,
}

/// Verdict variant for a single cached criterion run. Tagged enum so the
/// agent (and any consumer composing their own decomposition prompt)
/// gets exhaustive-match coverage instead of stringly-typed status
/// strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CriterionResult {
    /// The cached verifier run succeeded.
    Pass,
    /// The cached verifier run failed.
    Fail,
    /// The verifier reported the criterion was out of scope for the
    /// run (e.g. file-scoped `--files` filter excluded it).
    Skipped,
    /// No cached run exists — this criterion has never been verified
    /// on this machine.
    NoResult,
}

impl CriterionResult {
    /// Stable label used by `todo_*` templates when rendering each
    /// `criterion_status` row. The strings match the spec's enum
    /// variant names so the prompt surface and the Rust type stay in
    /// lockstep.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pass => "Pass",
            Self::Fail => "Fail",
            Self::Skipped => "Skipped",
            Self::NoResult => "NoResult",
        }
    }
}

//! Per-phase verdict gate (`specs/harness.md` lines 444-470).
//!
//! After every agent phase ends, `loom review` evaluates the result through
//! this deterministic gate before the bead's state can advance. The gate
//! combines four mechanical/agent-judged signals — exit marker, bd-closed,
//! diff emptiness, and the review verdict — into one of `done`, `blocked`,
//! `clarify`, or `recovery` with a typed cause.
//!
//! Logic is a pure function of the four signals; the binary owns the
//! plumbing that produces them and the recovery-loop dispatch on the other
//! side.

use super::verify_fail::VerifyFailure;
use crate::todo::ExitSignal;

/// Which concern in the review LLM's structured response triggered the flag.
/// Mirrors the per-diff rubric flag causes enumerated in
/// `specs/gate.md` ("Per-diff stage checks") and the flag-emission
/// schema in `loom-templates/templates/review.md`: the four verifier-honesty
/// sub-checks, mock discipline, scope appropriateness, `[judge]` rubric
/// satisfaction, style-rule conformance, plus the standing/tree-scope
/// concerns (surface drift, cross-spec clash, template-vs-spec drift,
/// spec-conventions violation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewConcern {
    VerifierBypass,
    FabricatedResult,
    WeakAssertion,
    CoincidentalPass,
    Mock,
    Scope,
    Judge,
    StyleRule,
    SurfaceDrift,
    CrossSpecClash,
    TemplateSpecDrift,
    SpecConventionsViolation,
}

impl ReviewConcern {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VerifierBypass => "verifier-bypass",
            Self::FabricatedResult => "fabricated-result",
            Self::WeakAssertion => "weak-assertion",
            Self::CoincidentalPass => "coincidental-pass",
            Self::Mock => "mock",
            Self::Scope => "scope",
            Self::Judge => "judge",
            Self::StyleRule => "style-rule",
            Self::SurfaceDrift => "surface-drift",
            Self::CrossSpecClash => "cross-spec-clash",
            Self::TemplateSpecDrift => "template-spec-drift",
            Self::SpecConventionsViolation => "spec-conventions-violation",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "verifier-bypass" => Some(Self::VerifierBypass),
            "fabricated-result" => Some(Self::FabricatedResult),
            "weak-assertion" => Some(Self::WeakAssertion),
            "coincidental-pass" => Some(Self::CoincidentalPass),
            "mock" => Some(Self::Mock),
            "scope" => Some(Self::Scope),
            "judge" => Some(Self::Judge),
            "style-rule" => Some(Self::StyleRule),
            "surface-drift" => Some(Self::SurfaceDrift),
            "cross-spec-clash" => Some(Self::CrossSpecClash),
            "template-spec-drift" => Some(Self::TemplateSpecDrift),
            "spec-conventions-violation" => Some(Self::SpecConventionsViolation),
            _ => None,
        }
    }
}

/// Parsed contents of the review LLM's structured flag emission. The detail
/// string carried here is what feeds the `review-concern` row of
/// `previous_failure` (`specs/harness.md` §"Recovery context") — sourced
/// from the structured emission, not regex-extracted from prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewFlag {
    pub concern: ReviewConcern,
    pub detail: String,
}

/// Why the gate routes to recovery. Mirrors the cause strings in the spec
/// table so they show up unchanged in `bd update --notes` when retries are
/// exhausted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryCause {
    /// No exit marker found in the agent output.
    SwallowedMarker,
    /// Marker was emitted but the bead was not bd-closed.
    IncompleteSignaling,
    /// `LOOM_COMPLETE` with an empty worktree diff. `LOOM_NOOP` is the
    /// legitimate path for an empty diff and never produces this cause.
    ZeroProgress,
    /// At least one deterministic-tier verifier failed. Carries every failure so the
    /// downstream `previous_failure` builder can format them into a single
    /// budget-bounded body — none short-circuit each other. `review_notes`
    /// holds the review LLM's flag, if any: review still runs on verify-fail
    /// (`specs/harness.md` §"Push gate · Review always runs") so the
    /// agent gets verify failures *and* live-path/mock/scope/judge feedback in
    /// one `previous_failure` round trip — appended under a `Review notes:`
    /// heading by the formatter. The cause label stays `verify-fail`
    /// (mechanical trumps semantic).
    VerifyFail {
        failures: Vec<VerifyFailure>,
        review_notes: Option<ReviewFlag>,
    },
    /// Verify passed but the reviewer raised a concern. Carries the structured
    /// concern + reasoning emitted by the review LLM so downstream surfaces
    /// (`bd update --notes`, `previous_failure`) can name which concern
    /// triggered without re-parsing the agent's prose.
    ReviewConcern(ReviewFlag),
    /// An `EventSink::react()` returned `SessionCommand::Abort` and the
    /// driver cancelled the session before the agent emitted a marker.
    /// Disambiguates "no marker" from `swallowed-marker` per
    /// `specs/harness.md` §"Disambiguating no marker". `reason` is
    /// the verbatim payload the observer emitted.
    ObserverAbort { reason: String },
    /// Agent emitted `LOOM_COMPLETE` / `LOOM_NOOP` and bd-closed the bead,
    /// but the bead's worktree left `git status --porcelain` non-empty.
    /// Routes BEFORE verify-fail / review-concern (`specs/harness.md`
    /// §"Verdict Gate · Tree-clean check") so verifiers do not run
    /// against a half-staged tree. `dirty_paths` is the already-capped
    /// list (up to 30 entries; an extra `"+N more"` element follows when
    /// the underlying set was larger). Driver caps before construction;
    /// the variant carries already-capped paths.
    TreeNotClean { dirty_paths: Vec<String> },
}

impl RecoveryCause {
    /// Stable spec-table label used in user-facing surfaces (logs, bd notes).
    /// The label is the same for every review-concern variant; per-concern
    /// detail lives in [`RecoveryCause::ReviewConcern`]'s payload.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SwallowedMarker => "swallowed-marker",
            Self::IncompleteSignaling => "incomplete-signaling",
            Self::ZeroProgress => "zero-progress",
            Self::VerifyFail { .. } => "verify-fail",
            Self::ReviewConcern(_) => "review-concern",
            Self::ObserverAbort { .. } => "observer-abort",
            Self::TreeNotClean { .. } => "tree-not-clean",
        }
    }
}

/// One of the four post-gate branches. The driver maps `Recovery` onto
/// `retry` (under `[loop] max_iterations`) or `blocked` (cap exhausted) one
/// layer up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseVerdict {
    /// Phase passed every gate stage — caller advances state.
    Done,
    /// Agent emitted `LOOM_BLOCKED` — surface to user without retry.
    Blocked { reason: String },
    /// Agent emitted `LOOM_CLARIFY` — apply `loom:clarify` and stop.
    Clarify { question: String },
    /// Mechanical or review failure — caller resolves to retry/blocked
    /// against the iteration counter.
    Recovery { cause: RecoveryCause },
}

/// Mechanical inputs the gate consumes alongside the parsed exit marker.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GateInputs {
    /// Bead carries `closed` status after the phase ran.
    pub bd_closed: bool,
    /// `git diff` against the driver branch produced no output.
    pub diff_empty: bool,
    /// Dirty entries reported by `git status --porcelain` on the bead's
    /// worktree, **already capped** (up to 30 entries with an extra
    /// `"+N more"` element when the underlying set was larger; see
    /// [`crate::r#loop::dirty_paths_from_porcelain`]). Empty when the tree
    /// is clean. Non-empty drives the gate to
    /// [`RecoveryCause::TreeNotClean`] BEFORE verify-fail /
    /// review-concern, so verifiers do not run against a half-staged
    /// tree (`specs/harness.md` §"Verdict Gate · Tree-clean check").
    pub tree_dirty_paths: Vec<String>,
    /// Failure record for every deterministic-tier verifier that exited non-zero.
    /// Empty when every script passed; the gate routes to
    /// [`RecoveryCause::VerifyFail`] when this is non-empty and threads the
    /// list through so downstream surfaces can format `previous_failure`.
    pub verify_failures: Vec<VerifyFailure>,
    /// Parsed reviewer flag, or `None` for a clean review.
    pub review_flag: Option<ReviewFlag>,
}

/// Apply the spec's decision table to the parsed marker plus mechanical
/// signals. `marker = None` means no exit marker was found in the agent
/// output (translated from [`crate::todo::parse_exit_signal`] returning
/// `None`).
pub fn decide(marker: Option<&ExitSignal>, inputs: GateInputs) -> PhaseVerdict {
    match marker {
        None => PhaseVerdict::Recovery {
            cause: RecoveryCause::SwallowedMarker,
        },
        Some(ExitSignal::Blocked { reason }) => PhaseVerdict::Blocked {
            reason: reason.clone(),
        },
        Some(ExitSignal::Clarify { question }) => PhaseVerdict::Clarify {
            question: question.clone(),
        },
        Some(ExitSignal::Complete) => decide_progress_marker(false, inputs),
        Some(ExitSignal::Noop) => decide_progress_marker(true, inputs),
        Some(ExitSignal::Concern { token, reason }) => decide_concern(token, reason, inputs),
    }
}

/// `LOOM_CONCERN` is review-phase-only per `specs/harness.md` § Marker
/// definitions. The review-phase classifier normalises the marker payload
/// into [`GateInputs::review_flag`] before invoking [`decide`]; this branch
/// is the fallback for callers that haven't done that normalisation yet —
/// a recognised token routes to [`RecoveryCause::ReviewConcern`] with the
/// structured detail, an unknown token collapses to `SwallowedMarker` so
/// downstream surfaces don't print a bogus concern.
fn decide_concern(token: &str, reason: &str, inputs: GateInputs) -> PhaseVerdict {
    if let Some(flag) = inputs.review_flag {
        return PhaseVerdict::Recovery {
            cause: RecoveryCause::ReviewConcern(flag),
        };
    }
    let Some(concern) = ReviewConcern::parse(token) else {
        return PhaseVerdict::Recovery {
            cause: RecoveryCause::SwallowedMarker,
        };
    };
    PhaseVerdict::Recovery {
        cause: RecoveryCause::ReviewConcern(ReviewFlag {
            concern,
            detail: reason.to_string(),
        }),
    }
}

/// Branch shared by `LOOM_COMPLETE` and `LOOM_NOOP`: both require the bead
/// to be closed and both gate on verify+review. They differ only in how an
/// empty diff is treated — Complete demands non-empty, Noop accepts any.
fn decide_progress_marker(is_noop: bool, inputs: GateInputs) -> PhaseVerdict {
    if !inputs.bd_closed {
        return PhaseVerdict::Recovery {
            cause: RecoveryCause::IncompleteSignaling,
        };
    }
    if !is_noop && inputs.diff_empty {
        return PhaseVerdict::Recovery {
            cause: RecoveryCause::ZeroProgress,
        };
    }
    // Tree-clean check precedes verify-fail / review-concern per
    // `specs/harness.md` §"Verdict Gate · Tree-clean check" — verifiers
    // do NOT run against a half-staged tree because that would conflate
    // the agent's intended diff with its leftover scratch.
    if !inputs.tree_dirty_paths.is_empty() {
        return PhaseVerdict::Recovery {
            cause: RecoveryCause::TreeNotClean {
                dirty_paths: inputs.tree_dirty_paths,
            },
        };
    }
    if !inputs.verify_failures.is_empty() {
        return PhaseVerdict::Recovery {
            cause: RecoveryCause::VerifyFail {
                failures: inputs.verify_failures,
                review_notes: inputs.review_flag,
            },
        };
    }
    if let Some(flag) = inputs.review_flag {
        return PhaseVerdict::Recovery {
            cause: RecoveryCause::ReviewConcern(flag),
        };
    }
    PhaseVerdict::Done
}

/// Marker prefix the review LLM emits to flag a concern. Format:
///
/// ```text
/// LOOM_CONCERN: <concern> -- <detail>
/// ```
///
/// `<concern>` is one of the [`ReviewConcern`] tokens — see its `as_str`
/// arms for the canonical set, which mirrors the flag-emission schema in
/// `loom-templates/templates/review.md`. `<detail>` is free-form one-line
/// reasoning. The marker is review-phase-only and must be the final line of
/// the agent's response per the mutual-exclusivity rule documented in
/// `partial/exit_signals.md`.
const REVIEW_FLAG_MARKER: &str = "LOOM_CONCERN:";
const REVIEW_FLAG_SEPARATOR: &str = "--";

/// Parse the review LLM's structured flag emission from its combined output.
/// Returns `None` for review-pass (no marker, or marker present but the
/// concern token is not one of the four enum values).
///
/// This is the **only** path by which a `RecoveryCause::ReviewConcern` detail
/// is produced — the spec ([§"Recovery context"](specs/harness.md))
/// requires the detail come from the structured emission, not regex-pulled
/// from surrounding prose.
pub fn parse_review_flag(output: &str) -> Option<ReviewFlag> {
    let mut last: Option<ReviewFlag> = None;
    for line in output.lines() {
        let Some(idx) = line.find(REVIEW_FLAG_MARKER) else {
            continue;
        };
        let after = line[idx + REVIEW_FLAG_MARKER.len()..].trim();
        let (concern_str, detail) = match after.split_once(REVIEW_FLAG_SEPARATOR) {
            Some((c, d)) => (c.trim(), d.trim().to_string()),
            None => (after, String::new()),
        };
        if let Some(concern) = ReviewConcern::parse(concern_str) {
            last = Some(ReviewFlag { concern, detail });
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(
        bd_closed: bool,
        diff_empty: bool,
        verify_pass: bool,
        review_flag: Option<ReviewFlag>,
    ) -> GateInputs {
        let verify_failures = if verify_pass {
            Vec::new()
        } else {
            vec![sample_failure()]
        };
        GateInputs {
            bd_closed,
            diff_empty,
            verify_failures,
            review_flag,
            ..GateInputs::default()
        }
    }

    fn sample_failure() -> VerifyFailure {
        VerifyFailure {
            script_path: std::path::PathBuf::from("tests/sample.sh"),
            exit_code: 1,
            stderr: "boom\n".into(),
        }
    }

    fn flag(concern: ReviewConcern, detail: &str) -> ReviewFlag {
        ReviewFlag {
            concern,
            detail: detail.to_string(),
        }
    }

    // --- Marker-only rows (bd/diff/review irrelevant). ---

    #[test]
    fn concern_marker_with_known_token_routes_to_review_concern_recovery() {
        let m = ExitSignal::Concern {
            token: "verifier-bypass".into(),
            reason: "test mocks the agent backend".into(),
        };
        match decide(Some(&m), inputs(true, false, true, None)) {
            PhaseVerdict::Recovery {
                cause: RecoveryCause::ReviewConcern(parsed),
            } => {
                assert_eq!(parsed.concern, ReviewConcern::VerifierBypass);
                assert_eq!(parsed.detail, "test mocks the agent backend");
            }
            other => panic!("expected Recovery::ReviewConcern, got {other:?}"),
        }
    }

    #[test]
    fn concern_marker_with_unknown_token_collapses_to_swallowed_marker() {
        let m = ExitSignal::Concern {
            token: "fictional-concern".into(),
            reason: "doesn't map to any enum".into(),
        };
        assert_eq!(
            decide(Some(&m), inputs(true, false, true, None)),
            PhaseVerdict::Recovery {
                cause: RecoveryCause::SwallowedMarker,
            },
        );
    }

    #[test]
    fn blocked_marker_routes_to_blocked_with_reason() {
        let m = ExitSignal::Blocked {
            reason: "missing schema".into(),
        };
        match decide(
            Some(&m),
            inputs(false, true, false, Some(flag(ReviewConcern::Mock, "x"))),
        ) {
            PhaseVerdict::Blocked { reason } => assert_eq!(reason, "missing schema"),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn clarify_marker_routes_to_clarify_with_question() {
        let m = ExitSignal::Clarify {
            question: "additive only?".into(),
        };
        match decide(Some(&m), inputs(true, false, true, None)) {
            PhaseVerdict::Clarify { question } => assert_eq!(question, "additive only?"),
            other => panic!("expected Clarify, got {other:?}"),
        }
    }

    #[test]
    fn missing_marker_routes_to_swallowed_marker_recovery() {
        assert_eq!(
            decide(None, inputs(true, false, true, None)),
            PhaseVerdict::Recovery {
                cause: RecoveryCause::SwallowedMarker,
            },
        );
    }

    // --- LOOM_COMPLETE rows. ---

    #[test]
    fn complete_without_bd_closed_routes_to_incomplete_signaling() {
        assert_eq!(
            decide(
                Some(&ExitSignal::Complete),
                inputs(false, false, true, None)
            ),
            PhaseVerdict::Recovery {
                cause: RecoveryCause::IncompleteSignaling,
            },
        );
    }

    #[test]
    fn complete_with_empty_diff_routes_to_zero_progress() {
        assert_eq!(
            decide(Some(&ExitSignal::Complete), inputs(true, true, true, None)),
            PhaseVerdict::Recovery {
                cause: RecoveryCause::ZeroProgress,
            },
        );
    }

    #[test]
    fn complete_with_verify_fail_routes_to_verify_fail() {
        let result = decide(
            Some(&ExitSignal::Complete),
            inputs(true, false, false, None),
        );
        match result {
            PhaseVerdict::Recovery {
                cause:
                    RecoveryCause::VerifyFail {
                        failures,
                        review_notes,
                    },
            } => {
                assert_eq!(failures.len(), 1, "carries every failure block");
                assert_eq!(failures[0].exit_code, 1);
                assert!(review_notes.is_none(), "no review flag in this row");
            }
            other => panic!("expected Recovery::VerifyFail, got {other:?}"),
        }
    }

    #[test]
    fn complete_with_verify_fail_and_review_flag_threads_both_into_recovery_cause() {
        // Spec rule: when verify fails, the cause is `verify-fail` (mechanical
        // trumps semantic) but review's reasoning still has to ride along so
        // the downstream formatter can append it under `Review notes:`.
        let detail = "test mocks the agent backend instead of spawning it";
        let g = GateInputs {
            bd_closed: true,
            diff_empty: false,
            verify_failures: vec![sample_failure()],
            review_flag: Some(flag(ReviewConcern::VerifierBypass, detail)),
            ..GateInputs::default()
        };
        match decide(Some(&ExitSignal::Complete), g) {
            PhaseVerdict::Recovery {
                cause:
                    RecoveryCause::VerifyFail {
                        failures,
                        review_notes,
                    },
            } => {
                assert_eq!(failures.len(), 1);
                let notes = review_notes.expect("review flag threaded into cause");
                assert_eq!(notes.concern, ReviewConcern::VerifierBypass);
                assert_eq!(notes.detail, detail);
            }
            other => panic!("expected Recovery::VerifyFail with review_notes, got {other:?}"),
        }
    }

    #[test]
    fn verify_fail_carries_every_failure_block_for_previous_failure() {
        // Spec gate: `previous_failure` carries every failure (not just the
        // first). The recovery-cause payload is the channel — downstream
        // formatter splits the 4000-char budget across them.
        let failures = vec![
            VerifyFailure {
                script_path: std::path::PathBuf::from("tests/a.sh"),
                exit_code: 1,
                stderr: "boom-a".into(),
            },
            VerifyFailure {
                script_path: std::path::PathBuf::from("tests/b.sh"),
                exit_code: 2,
                stderr: "boom-b".into(),
            },
        ];
        let g = GateInputs {
            bd_closed: true,
            diff_empty: false,
            verify_failures: failures.clone(),
            review_flag: None,
            ..GateInputs::default()
        };
        match decide(Some(&ExitSignal::Complete), g) {
            PhaseVerdict::Recovery {
                cause:
                    RecoveryCause::VerifyFail {
                        failures: carried, ..
                    },
            } => {
                assert_eq!(carried, failures, "every failure threaded through");
            }
            other => panic!("expected Recovery::VerifyFail, got {other:?}"),
        }
    }

    #[test]
    fn complete_with_review_concern_routes_to_review_concern() {
        let detail = "test mocks the agent backend instead of spawning it";
        let result = decide(
            Some(&ExitSignal::Complete),
            inputs(
                true,
                false,
                true,
                Some(flag(ReviewConcern::VerifierBypass, detail)),
            ),
        );
        match result {
            PhaseVerdict::Recovery {
                cause: RecoveryCause::ReviewConcern(parsed),
            } => {
                assert_eq!(parsed.concern, ReviewConcern::VerifierBypass);
                assert_eq!(parsed.detail, detail);
            }
            other => panic!("expected Recovery::ReviewConcern, got {other:?}"),
        }
    }

    #[test]
    fn complete_with_style_rule_flag_routes_to_review_concern_with_rule_id() {
        // The style-rule conformance rubric surfaces as a `review-concern`
        // cause whose concern is `style-rule`. The detail names the
        // violating rule id (e.g. `RS-12`) so downstream surfaces can
        // render it without re-parsing the LLM's prose.
        let detail = "RS-12 placeholder reaches consumer in src/agent/parser.rs:142-156";
        let result = decide(
            Some(&ExitSignal::Complete),
            inputs(
                true,
                false,
                true,
                Some(flag(ReviewConcern::StyleRule, detail)),
            ),
        );
        match result {
            PhaseVerdict::Recovery {
                cause: RecoveryCause::ReviewConcern(parsed),
            } => {
                assert_eq!(parsed.concern, ReviewConcern::StyleRule);
                assert_eq!(parsed.detail, detail);
            }
            other => panic!("expected Recovery::ReviewConcern(style-rule), got {other:?}"),
        }
    }

    #[test]
    fn complete_clean_routes_to_done() {
        assert_eq!(
            decide(Some(&ExitSignal::Complete), inputs(true, false, true, None)),
            PhaseVerdict::Done,
        );
    }

    // --- LOOM_NOOP rows (the four scoped by this bead). ---

    #[test]
    fn noop_without_bd_closed_routes_to_incomplete_signaling() {
        assert_eq!(
            decide(Some(&ExitSignal::Noop), inputs(false, true, true, None)),
            PhaseVerdict::Recovery {
                cause: RecoveryCause::IncompleteSignaling,
            },
        );
    }

    #[test]
    fn noop_with_verify_fail_routes_to_verify_fail() {
        // Empty diff allowed under Noop; verify failure still recovers.
        for diff_empty in [true, false] {
            let result = decide(
                Some(&ExitSignal::Noop),
                inputs(true, diff_empty, false, None),
            );
            match result {
                PhaseVerdict::Recovery {
                    cause: RecoveryCause::VerifyFail { failures, .. },
                } => {
                    assert_eq!(failures.len(), 1, "diff_empty={diff_empty}");
                }
                other => panic!("expected VerifyFail (diff_empty={diff_empty}), got {other:?}"),
            }
        }
    }

    #[test]
    fn noop_with_review_flag_routes_to_review_concern() {
        let detail = "diff edits files outside the spec's Affected Files list";
        let result = decide(
            Some(&ExitSignal::Noop),
            inputs(true, true, true, Some(flag(ReviewConcern::Scope, detail))),
        );
        match result {
            PhaseVerdict::Recovery {
                cause: RecoveryCause::ReviewConcern(parsed),
            } => {
                assert_eq!(parsed.concern, ReviewConcern::Scope);
                assert_eq!(parsed.detail, detail);
            }
            other => panic!("expected Recovery::ReviewConcern, got {other:?}"),
        }
    }

    #[test]
    fn noop_with_empty_diff_and_clean_review_is_done_not_zero_progress() {
        // The reason this gate exists: empty diff + Noop must NOT trip
        // zero-progress recovery — the work was already in tree.
        assert_eq!(
            decide(Some(&ExitSignal::Noop), inputs(true, true, true, None)),
            PhaseVerdict::Done,
        );
    }

    #[test]
    fn noop_with_non_empty_diff_and_clean_review_is_done() {
        assert_eq!(
            decide(Some(&ExitSignal::Noop), inputs(true, false, true, None)),
            PhaseVerdict::Done,
        );
    }

    // --- Tree-not-clean rows (precede verify-fail / review-concern). ---

    #[test]
    fn complete_with_dirty_tree_routes_to_tree_not_clean_before_verify() {
        // Spec gate (`specs/harness.md` §"Verdict Gate · Tree-clean check"):
        // when the worktree is dirty after the bead bd-closed, the gate
        // routes to `tree-not-clean` recovery BEFORE verify-fail /
        // review-concern. Verifiers do not run against a half-staged tree.
        // Set BOTH verify_failures and review_flag to non-default values so
        // the test pins precedence — tree-not-clean wins over both.
        let dirty = vec![" M src/foo.rs".to_string(), "?? scratch.tmp".to_string()];
        let g = GateInputs {
            bd_closed: true,
            diff_empty: false,
            tree_dirty_paths: dirty.clone(),
            verify_failures: vec![sample_failure()],
            review_flag: Some(flag(ReviewConcern::Scope, "out of scope")),
        };
        match decide(Some(&ExitSignal::Complete), g) {
            PhaseVerdict::Recovery {
                cause: RecoveryCause::TreeNotClean { dirty_paths },
            } => {
                assert_eq!(
                    dirty_paths, dirty,
                    "tree-not-clean carries the (already-capped) dirty paths verbatim",
                );
            }
            other => panic!(
                "expected Recovery::TreeNotClean (precedes verify-fail & review-concern), \
                 got {other:?}",
            ),
        }
    }

    #[test]
    fn noop_with_dirty_tree_routes_to_tree_not_clean() {
        // Same gate fires for `LOOM_NOOP`: a NOOP claims "no work needed"
        // but a dirty tree disagrees; surfacing the discrepancy beats
        // letting the bead close on a false negative.
        let dirty = vec![" M crates/loom-driver/src/git/client.rs".to_string()];
        let g = GateInputs {
            bd_closed: true,
            diff_empty: true,
            tree_dirty_paths: dirty.clone(),
            verify_failures: vec![],
            review_flag: None,
        };
        match decide(Some(&ExitSignal::Noop), g) {
            PhaseVerdict::Recovery {
                cause: RecoveryCause::TreeNotClean { dirty_paths },
            } => {
                assert_eq!(dirty_paths, dirty);
            }
            other => panic!("expected Recovery::TreeNotClean for NOOP+dirty, got {other:?}"),
        }
    }

    #[test]
    fn tree_not_clean_detail_enumerates_and_caps_dirty_paths() {
        // Spec (`specs/harness.md` §"Verdict Gate · Tree-clean check"):
        // dirty paths capped at 30 entries with a "+N more" suffix when
        // truncated. The driver caps before construction; the variant
        // carries the already-capped list verbatim.
        use crate::r#loop::{TREE_NOT_CLEAN_CAP, dirty_paths_from_porcelain};

        // 42 lines of porcelain → 30 cap + "+12 more" suffix == 31 entries.
        let porcelain = (0..42)
            .map(|i| format!(" M src/file_{i}.rs"))
            .collect::<Vec<_>>()
            .join("\n");
        let capped = dirty_paths_from_porcelain(&porcelain);
        assert_eq!(
            capped.len(),
            TREE_NOT_CLEAN_CAP + 1,
            "cap of {TREE_NOT_CLEAN_CAP} + 1 overflow marker line",
        );
        assert_eq!(capped[0], " M src/file_0.rs");
        assert_eq!(capped[TREE_NOT_CLEAN_CAP - 1], " M src/file_29.rs");
        assert_eq!(
            capped[TREE_NOT_CLEAN_CAP], "+12 more",
            "final entry names the overflow count",
        );

        // Under the cap: every line passes through, no overflow marker.
        let porcelain_small = (0..5)
            .map(|i| format!("?? scratch_{i}.tmp"))
            .collect::<Vec<_>>()
            .join("\n");
        let capped_small = dirty_paths_from_porcelain(&porcelain_small);
        assert_eq!(capped_small.len(), 5);
        assert!(
            !capped_small.iter().any(|p| p.starts_with('+')),
            "no overflow marker under the cap (got: {capped_small:?})",
        );

        // The verdict gate accepts the already-capped Vec verbatim — the
        // capping discipline is enforced by the caller, not re-applied here.
        let g = GateInputs {
            bd_closed: true,
            diff_empty: false,
            tree_dirty_paths: capped.clone(),
            verify_failures: vec![],
            review_flag: None,
        };
        match decide(Some(&ExitSignal::Complete), g) {
            PhaseVerdict::Recovery {
                cause: RecoveryCause::TreeNotClean { dirty_paths },
            } => {
                assert_eq!(dirty_paths, capped);
                assert_eq!(dirty_paths.last().map(String::as_str), Some("+12 more"));
            }
            other => panic!("expected Recovery::TreeNotClean, got {other:?}"),
        }
    }

    // --- Cause label round-trip (bd notes / log surfaces). ---

    #[test]
    fn recovery_cause_labels_match_spec_strings() {
        assert_eq!(RecoveryCause::SwallowedMarker.as_str(), "swallowed-marker");
        assert_eq!(
            RecoveryCause::IncompleteSignaling.as_str(),
            "incomplete-signaling",
        );
        assert_eq!(RecoveryCause::ZeroProgress.as_str(), "zero-progress");
        assert_eq!(
            RecoveryCause::VerifyFail {
                failures: vec![],
                review_notes: None,
            }
            .as_str(),
            "verify-fail",
        );
        assert_eq!(
            RecoveryCause::VerifyFail {
                failures: vec![],
                review_notes: Some(flag(ReviewConcern::Mock, "x")),
            }
            .as_str(),
            "verify-fail",
            "label is mechanical-only — review-notes piggyback never relabels",
        );
        assert_eq!(
            RecoveryCause::ReviewConcern(flag(ReviewConcern::Judge, "")).as_str(),
            "review-concern",
        );
        assert_eq!(
            RecoveryCause::ObserverAbort {
                reason: "doom-loop: 3 identical tool calls".into(),
            }
            .as_str(),
            "observer-abort",
        );
        assert_eq!(
            RecoveryCause::TreeNotClean {
                dirty_paths: vec![" M src/foo.rs".into()],
            }
            .as_str(),
            "tree-not-clean",
        );
    }

    #[test]
    fn review_concern_labels_match_spec_vocabulary() {
        assert_eq!(ReviewConcern::VerifierBypass.as_str(), "verifier-bypass");
        assert_eq!(
            ReviewConcern::FabricatedResult.as_str(),
            "fabricated-result",
        );
        assert_eq!(ReviewConcern::WeakAssertion.as_str(), "weak-assertion");
        assert_eq!(
            ReviewConcern::CoincidentalPass.as_str(),
            "coincidental-pass"
        );
        assert_eq!(ReviewConcern::Mock.as_str(), "mock");
        assert_eq!(ReviewConcern::Scope.as_str(), "scope");
        assert_eq!(ReviewConcern::Judge.as_str(), "judge");
        assert_eq!(ReviewConcern::StyleRule.as_str(), "style-rule");
        assert_eq!(ReviewConcern::SurfaceDrift.as_str(), "surface-drift");
        assert_eq!(ReviewConcern::CrossSpecClash.as_str(), "cross-spec-clash");
        assert_eq!(
            ReviewConcern::TemplateSpecDrift.as_str(),
            "template-spec-drift",
        );
        assert_eq!(
            ReviewConcern::SpecConventionsViolation.as_str(),
            "spec-conventions-violation",
        );
    }

    #[test]
    fn review_concern_parse_round_trips_each_variant() {
        for c in [
            ReviewConcern::VerifierBypass,
            ReviewConcern::FabricatedResult,
            ReviewConcern::WeakAssertion,
            ReviewConcern::CoincidentalPass,
            ReviewConcern::Mock,
            ReviewConcern::Scope,
            ReviewConcern::Judge,
            ReviewConcern::StyleRule,
            ReviewConcern::SurfaceDrift,
            ReviewConcern::CrossSpecClash,
            ReviewConcern::TemplateSpecDrift,
            ReviewConcern::SpecConventionsViolation,
        ] {
            assert_eq!(ReviewConcern::parse(c.as_str()), Some(c));
        }
    }

    #[test]
    fn review_concern_parse_rejects_unknown_token() {
        // `live-path` is the pre-rubric-expansion umbrella token; it must
        // read as unknown rather than silently round-trip to a sub-check.
        assert_eq!(ReviewConcern::parse("live-path"), None);
        assert_eq!(ReviewConcern::parse("verifierbypass"), None);
        assert_eq!(ReviewConcern::parse("nit"), None);
        assert_eq!(ReviewConcern::parse(""), None);
    }

    // --- Structured flag parsing. ---

    #[test]
    fn parse_review_flag_returns_none_when_marker_absent() {
        assert!(parse_review_flag("LOOM_COMPLETE\n").is_none());
        assert!(parse_review_flag("ok\nno flag here\n").is_none());
    }

    #[test]
    fn parse_review_flag_extracts_concern_and_detail_from_marker() {
        let out = "preamble\nLOOM_CONCERN: verifier-bypass -- test mocks the agent backend\nLOOM_COMPLETE\n";
        let parsed = parse_review_flag(out).expect("flag parsed");
        assert_eq!(parsed.concern, ReviewConcern::VerifierBypass);
        assert_eq!(parsed.detail, "test mocks the agent backend");
    }

    #[test]
    fn parse_review_flag_supports_each_concern_variant() {
        for (token, expected) in [
            ("verifier-bypass", ReviewConcern::VerifierBypass),
            ("fabricated-result", ReviewConcern::FabricatedResult),
            ("weak-assertion", ReviewConcern::WeakAssertion),
            ("coincidental-pass", ReviewConcern::CoincidentalPass),
            ("mock", ReviewConcern::Mock),
            ("scope", ReviewConcern::Scope),
            ("judge", ReviewConcern::Judge),
            ("style-rule", ReviewConcern::StyleRule),
            ("surface-drift", ReviewConcern::SurfaceDrift),
            ("cross-spec-clash", ReviewConcern::CrossSpecClash),
            ("template-spec-drift", ReviewConcern::TemplateSpecDrift),
            (
                "spec-conventions-violation",
                ReviewConcern::SpecConventionsViolation,
            ),
        ] {
            let out = format!("LOOM_CONCERN: {token} -- because\n");
            let parsed = parse_review_flag(&out).expect("flag parsed");
            assert_eq!(parsed.concern, expected);
            assert_eq!(parsed.detail, "because");
        }
    }

    #[test]
    fn parse_review_flag_takes_last_well_formed_match() {
        let out = "LOOM_CONCERN: mock -- first\n\
                   LOOM_CONCERN: judge -- second\n";
        let parsed = parse_review_flag(out).expect("flag parsed");
        assert_eq!(parsed.concern, ReviewConcern::Judge);
        assert_eq!(parsed.detail, "second");
    }

    #[test]
    fn parse_review_flag_skips_marker_with_unknown_concern() {
        // A garbled marker collapses to "no flag" — better than synthesising
        // a bogus concern that downstream surfaces would print verbatim.
        assert!(parse_review_flag("LOOM_CONCERN: nit -- whatever\n").is_none());
    }

    #[test]
    fn parse_review_flag_accepts_empty_detail() {
        let parsed =
            parse_review_flag("LOOM_CONCERN: scope\n").expect("concern-only marker parses");
        assert_eq!(parsed.concern, ReviewConcern::Scope);
        assert!(parsed.detail.is_empty());
    }
}

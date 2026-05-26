//! Verdict-gate recovery resolution (`specs/harness.md` lines
//! 829-832 + 906-918 + 1960-1981).
//!
//! When the verdict gate's decision table produces
//! [`PhaseVerdict::Recovery`](super::phase_verdict::PhaseVerdict::Recovery)
//! the driver maps that onto one of two terminal shapes against the
//! bead's iteration counter and `[loop] max_iterations`:
//!
//! - **Retry** — iter `<` max. The original recovery cause is rendered
//!   into the next session's `previous_failure` payload (see
//!   [`super::verify_fail::format_previous_failure`]) and the bead is
//!   re-dispatched.
//! - **Blocked** — iter `>=` max. The driver applies `loom:blocked` with
//!   cause `retry-exhausted`; the original recovery cause is preserved
//!   in `bd update --notes` so the human can see *why* the loop gave up.
//!
//! The "spawns fix-up bead OR retries" disjunction in the spec criterion
//! `test_recovery_under_max` is satisfied by this module's Retry branch
//! plus the dedicated chokepoint in [`super::fixup`]: callers that want
//! to spawn a fix-up bead during a Retry pass invoke
//! [`super::fixup::spawn_fixup_bead`] explicitly.

use loom_templates::run::{DriverNoticeCause, PreviousFailure, ReviewConcernKind, VerifierFailure};

use super::phase_verdict::{RecoveryCause, ReviewConcern};
use super::verify_fail::{VerifyFailure, format_previous_failure};

/// Render a [`RecoveryCause`] into a `previous_failure` body suitable for
/// threading into the next agent attempt's prompt — or into the blocked
/// notes when retries exhaust. Mirrors the per-cause detail table in
/// `specs/harness.md` §"Recovery context (`previous_failure`)".
fn render_previous_failure(cause: &RecoveryCause) -> String {
    match cause {
        RecoveryCause::SwallowedMarker => {
            "Last phase ended without a `LOOM_*` exit marker.".to_string()
        }
        RecoveryCause::IncompleteSignaling => {
            "Marker `LOOM_COMPLETE` emitted but the bead was not bd-closed.".to_string()
        }
        RecoveryCause::ZeroProgress => {
            "Marker `LOOM_COMPLETE` emitted with empty diff. Use `LOOM_NOOP` if no work was needed."
                .to_string()
        }
        RecoveryCause::VerifyFail {
            failures,
            review_notes,
        } => format_previous_failure(failures, review_notes.as_ref()),
        RecoveryCause::ReviewConcern(flag) => {
            format!("[{}] {}", flag.concern.as_str(), flag.detail)
        }
        RecoveryCause::ObserverAbort { reason } => {
            format!("Session aborted by observer: {reason}.")
        }
        RecoveryCause::TreeNotClean { dirty_paths } => PreviousFailure::TreeNotClean {
            dirty_paths: dirty_paths.clone(),
        }
        .to_string(),
    }
}

/// Map a [`RecoveryCause`] to the typed
/// [`PreviousFailure`](loom_templates::run::PreviousFailure) variant the
/// `run.md` template renders. Per `specs/harness.md` § Verdict Gate
/// (recovery-context table):
///
/// - `SwallowedMarker` / `IncompleteSignaling` / `ZeroProgress` →
///   `DriverNotice` with the corresponding `DriverNoticeCause` + detail.
/// - `ObserverAbort` → `DriverNotice { cause: ObserverAbort, detail }`.
/// - `VerifyFail` → `VerifyFailures(Vec<VerifierFailure>)`. The companion
///   `review_notes` flag, when present, must be carried separately on
///   `RunContext.review_notes` (this function does not embed it inline).
/// - `ReviewConcern` → `ReviewConcern { concern: ReviewConcernKind, reason }`.
/// - `TreeNotClean` → `TreeNotClean { dirty_paths }`. The capped path list
///   passes through verbatim; the variant's `Display` impl emits the
///   spec-pinned framing + per-path enumeration + `"+N more"` suffix.
pub fn cause_to_previous_failure(cause: &RecoveryCause) -> PreviousFailure {
    match cause {
        RecoveryCause::SwallowedMarker => PreviousFailure::DriverNotice {
            cause: DriverNoticeCause::SwallowedMarker,
            detail: "Last phase ended without a `LOOM_*` exit marker.".to_string(),
        },
        RecoveryCause::IncompleteSignaling => PreviousFailure::DriverNotice {
            cause: DriverNoticeCause::IncompleteSignaling,
            detail: "Marker `LOOM_COMPLETE` emitted but the bead was not bd-closed.".to_string(),
        },
        RecoveryCause::ZeroProgress => PreviousFailure::DriverNotice {
            cause: DriverNoticeCause::ZeroProgress,
            detail:
                "Marker `LOOM_COMPLETE` emitted with empty diff. Use `LOOM_NOOP` if no work was needed."
                    .to_string(),
        },
        RecoveryCause::ObserverAbort { reason } => PreviousFailure::DriverNotice {
            cause: DriverNoticeCause::ObserverAbort,
            detail: format!("Session aborted by observer: {reason}."),
        },
        RecoveryCause::VerifyFail { failures, .. } => {
            PreviousFailure::VerifyFailures(failures.iter().map(verify_failure_to_typed).collect())
        }
        RecoveryCause::ReviewConcern(flag) => PreviousFailure::ReviewConcern {
            concern: concern_to_kind(flag.concern),
            reason: flag.detail.clone(),
        },
        RecoveryCause::TreeNotClean { dirty_paths } => PreviousFailure::TreeNotClean {
            dirty_paths: dirty_paths.clone(),
        },
    }
}

fn verify_failure_to_typed(failure: &VerifyFailure) -> VerifierFailure {
    VerifierFailure::new(
        failure.script_path.display().to_string(),
        failure.exit_code,
        failure.stderr.clone(),
    )
}

fn concern_to_kind(concern: ReviewConcern) -> ReviewConcernKind {
    match concern {
        ReviewConcern::VerifierBypass => ReviewConcernKind::VerifierBypass,
        ReviewConcern::FabricatedResult => ReviewConcernKind::FabricatedResult,
        ReviewConcern::WeakAssertion => ReviewConcernKind::WeakAssertion,
        ReviewConcern::CoincidentalPass => ReviewConcernKind::CoincidentalPass,
        ReviewConcern::Mock => ReviewConcernKind::MockDiscipline,
        ReviewConcern::Scope => ReviewConcernKind::ScopeCreep,
        ReviewConcern::Judge => ReviewConcernKind::JudgeFlag,
        ReviewConcern::StyleRule => ReviewConcernKind::Other("style-rule".into()),
        ReviewConcern::SurfaceDrift => ReviewConcernKind::Other("surface-drift".into()),
        ReviewConcern::CrossSpecClash => ReviewConcernKind::Other("cross-spec-clash".into()),
        ReviewConcern::TemplateSpecDrift => ReviewConcernKind::Other("template-spec-drift".into()),
        ReviewConcern::SpecConventionsViolation => {
            ReviewConcernKind::Other("spec-conventions-violation".into())
        }
    }
}

/// Cause string written to `bd update --notes` when the recovery loop
/// exhausts `[loop] max_iterations`. Mirrored from
/// `specs/harness.md` §"Verdict Gate · Labels".
pub const RETRY_EXHAUSTED_CAUSE: &str = "retry-exhausted";

/// Resolution of one [`RecoveryCause`] against the bead's iteration
/// counter + `[loop] max_iterations`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryResolution {
    /// Iter `<` max. Re-dispatch the bead with `previous_failure`
    /// threaded into the next session's prompt. The body is the spec's
    /// recovery-cause formatting (`super::verify_fail::format_previous_failure`).
    Retry { previous_failure: String },
    /// Iter `>=` max. Apply `loom:blocked` + `retry-exhausted` to the
    /// bead. `notes` is the `bd update --notes` body — it leads with the
    /// `retry-exhausted` label, names the original recovery cause for
    /// human review, and embeds the same `previous_failure` body the
    /// last retry would have seen (so the surface tells the human what
    /// the agent kept failing on).
    Blocked { cause: String, notes: String },
}

/// Pure-ish recovery resolver. `max` is `[loop] max_iterations` (default
/// 3). `iter` is the bead's pre-decision iteration counter: 0 on the
/// first failure, so the function returns Retry until `iter` reaches
/// `max`.
pub fn resolve_recovery(cause: &RecoveryCause, iter: u32, max: u32) -> RecoveryResolution {
    let previous_failure = render_previous_failure(cause);
    if iter >= max {
        let notes = format!(
            "{RETRY_EXHAUSTED_CAUSE}: recovery loop hit [loop] max_iterations ({max}) on cause `{original}`.\n\n{previous_failure}",
            original = cause.as_str(),
        );
        RecoveryResolution::Blocked {
            cause: RETRY_EXHAUSTED_CAUSE.to_string(),
            notes,
        }
    } else {
        RecoveryResolution::Retry { previous_failure }
    }
}

#[cfg(test)]
#[expect(clippy::panic, reason = "tests use panicking helpers")]
mod tests {
    use super::*;
    use crate::review::phase_verdict::{ReviewConcern, ReviewFlag};
    use crate::review::verify_fail::VerifyFailure;

    #[test]
    fn cause_to_previous_failure_maps_driver_notices() {
        // The three driver-procedural causes all collapse onto
        // PreviousFailure::DriverNotice carrying the matching DriverNoticeCause
        // and the spec table's verbatim detail string.
        for (cause, expected_cause, expected_detail_fragment) in [
            (
                RecoveryCause::SwallowedMarker,
                DriverNoticeCause::SwallowedMarker,
                "without a `LOOM_*` exit marker",
            ),
            (
                RecoveryCause::IncompleteSignaling,
                DriverNoticeCause::IncompleteSignaling,
                "not bd-closed",
            ),
            (
                RecoveryCause::ZeroProgress,
                DriverNoticeCause::ZeroProgress,
                "empty diff",
            ),
        ] {
            match cause_to_previous_failure(&cause) {
                PreviousFailure::DriverNotice { cause: c, detail } => {
                    assert_eq!(c, expected_cause, "cause mismatch for {cause:?}");
                    assert!(
                        detail.contains(expected_detail_fragment),
                        "detail missing fragment for {cause:?}: {detail}",
                    );
                }
                other => panic!("expected DriverNotice for {cause:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn cause_to_previous_failure_maps_observer_abort_with_reason_in_detail() {
        let cause = RecoveryCause::ObserverAbort {
            reason: "doom-loop: 3 identical tool calls".into(),
        };
        match cause_to_previous_failure(&cause) {
            PreviousFailure::DriverNotice {
                cause: DriverNoticeCause::ObserverAbort,
                detail,
            } => {
                assert!(
                    detail.contains("doom-loop: 3 identical tool calls"),
                    "verbatim observer reason missing: {detail}",
                );
            }
            other => panic!("expected DriverNotice(ObserverAbort), got {other:?}"),
        }
    }

    #[test]
    fn cause_to_previous_failure_maps_verify_fail_into_typed_failures() {
        let cause = RecoveryCause::VerifyFail {
            failures: vec![
                VerifyFailure {
                    script_path: std::path::PathBuf::from("tests/a.sh"),
                    exit_code: 2,
                    stderr: "boom-a".into(),
                },
                VerifyFailure {
                    script_path: std::path::PathBuf::from("tests/b.sh"),
                    exit_code: 3,
                    stderr: "boom-b".into(),
                },
            ],
            review_notes: None,
        };
        match cause_to_previous_failure(&cause) {
            PreviousFailure::VerifyFailures(failures) => {
                assert_eq!(failures.len(), 2);
                assert_eq!(failures[0].target, "tests/a.sh");
                assert_eq!(failures[0].exit_code, 2);
                assert!(failures[0].stderr_tail.contains("boom-a"));
                assert_eq!(failures[1].target, "tests/b.sh");
                assert_eq!(failures[1].exit_code, 3);
            }
            other => panic!("expected VerifyFailures, got {other:?}"),
        }
    }

    #[test]
    fn cause_to_previous_failure_maps_review_concern_to_typed_concern() {
        let cause = RecoveryCause::ReviewConcern(ReviewFlag {
            concern: ReviewConcern::VerifierBypass,
            detail: "test mocks the agent backend".into(),
        });
        match cause_to_previous_failure(&cause) {
            PreviousFailure::ReviewConcern { concern, reason } => {
                assert_eq!(concern, ReviewConcernKind::VerifierBypass);
                assert_eq!(reason, "test mocks the agent backend");
            }
            other => panic!("expected ReviewConcern, got {other:?}"),
        }
    }

    #[test]
    fn cause_to_previous_failure_maps_tree_not_clean_passes_paths_through() {
        // Spec (`specs/harness.md` §"Verdict Gate · Tree-clean check"):
        // dirty paths are capped by the driver before construction; the
        // mapping passes the capped list verbatim to the typed variant so
        // the template's render layer sees the same list (with `"+N more"`
        // suffix when truncated) that the driver constructed.
        let dirty = vec![
            " M src/foo.rs".to_string(),
            "?? scratch.tmp".to_string(),
            "+7 more".to_string(),
        ];
        let cause = RecoveryCause::TreeNotClean {
            dirty_paths: dirty.clone(),
        };
        match cause_to_previous_failure(&cause) {
            PreviousFailure::TreeNotClean { dirty_paths } => {
                assert_eq!(dirty_paths, dirty, "capped list passes through verbatim");
            }
            other => panic!("expected TreeNotClean, got {other:?}"),
        }
    }

    #[test]
    fn tree_not_clean_cause_renders_with_dirty_paths_in_notes() {
        // `retry-exhausted` notes preserve the original cause body so the
        // human reading `bd show --notes` sees the same framing the agent
        // would have seen in `previous_failure`. The body lines up with
        // the typed render path (`PreviousFailure::TreeNotClean`'s
        // `Display` impl) so framing stays identical across both
        // surfaces.
        let cause = RecoveryCause::TreeNotClean {
            dirty_paths: vec![" M src/foo.rs".into(), "?? scratch.tmp".into()],
        };
        match resolve_recovery(&cause, 5, 3) {
            RecoveryResolution::Blocked { cause: c, notes } => {
                assert_eq!(c, RETRY_EXHAUSTED_CAUSE);
                assert!(
                    notes.contains("tree-not-clean"),
                    "original cause label appears in notes: {notes}",
                );
                assert!(
                    notes.contains("Working tree was not clean"),
                    "spec-pinned framing appears in notes: {notes}",
                );
                assert!(
                    notes.contains(" M src/foo.rs"),
                    "dirty path appears in notes: {notes}",
                );
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    fn verify_fail_cause() -> RecoveryCause {
        RecoveryCause::VerifyFail {
            failures: vec![VerifyFailure {
                script_path: std::path::PathBuf::from("tests/sample.sh"),
                exit_code: 1,
                stderr: "boom\n".into(),
            }],
            review_notes: None,
        }
    }

    #[test]
    fn under_max_recovers_with_previous_failure() {
        // iter < max → Retry, and the `previous_failure` body must be the
        // spec-formatted cause body so the next session's prompt sees
        // every failing-verify block (and any threaded review notes).
        let cause = verify_fail_cause();
        match resolve_recovery(&cause, 0, 3) {
            RecoveryResolution::Retry { previous_failure } => {
                assert!(
                    previous_failure.contains("tests/sample.sh"),
                    "previous_failure carries the spec-formatted cause body: {previous_failure}",
                );
                assert!(
                    previous_failure.contains("boom"),
                    "stderr tail is part of the previous_failure body: {previous_failure}",
                );
            }
            other => panic!("expected Retry, got {other:?}"),
        }
    }

    #[test]
    fn under_max_at_two_still_recovers_with_previous_failure() {
        // The boundary is strict-less-than: iter=2 with max=3 still
        // retries (one slot left). iter=3 is exhausted (see next test).
        let cause = RecoveryCause::SwallowedMarker;
        let res = resolve_recovery(&cause, 2, 3);
        assert!(matches!(res, RecoveryResolution::Retry { .. }));
    }

    #[test]
    fn at_or_above_max_applies_blocked_with_retry_exhausted_cause() {
        // iter >= max → Blocked. The cause is the literal spec string
        // `retry-exhausted`; the notes preserve the original recovery
        // cause so the human reading `bd show --notes` knows what kept
        // failing.
        let cause = verify_fail_cause();
        for iter in [3u32, 4, 99] {
            match resolve_recovery(&cause, iter, 3) {
                RecoveryResolution::Blocked {
                    cause: applied,
                    notes,
                } => {
                    assert_eq!(applied, RETRY_EXHAUSTED_CAUSE);
                    assert_eq!(applied, "retry-exhausted");
                    assert!(
                        notes.contains("retry-exhausted"),
                        "notes lead with the retry-exhausted label: {notes}",
                    );
                    assert!(
                        notes.contains("verify-fail"),
                        "notes preserve original cause for human review: {notes}",
                    );
                    assert!(
                        notes.contains("max_iterations (3)"),
                        "notes cite the cap that was hit: {notes}",
                    );
                    assert!(
                        notes.contains("tests/sample.sh"),
                        "previous_failure body is embedded in notes: {notes}",
                    );
                    assert!(
                        notes.contains("boom"),
                        "stderr tail is embedded in notes: {notes}",
                    );
                }
                other => panic!("expected Blocked at iter={iter}, got {other:?}"),
            }
        }
    }

    #[test]
    fn review_concern_cause_round_trips_through_notes() {
        // The `review-concern` cause carries the concern + verbatim flag
        // detail; both must survive into the blocked notes when the
        // recovery loop gives up on a flag.
        let cause = RecoveryCause::ReviewConcern(ReviewFlag {
            concern: ReviewConcern::VerifierBypass,
            detail: "test mocks the agent backend".into(),
        });
        match resolve_recovery(&cause, 3, 3) {
            RecoveryResolution::Blocked { cause, notes } => {
                assert_eq!(cause, "retry-exhausted");
                assert!(
                    notes.contains("review-concern"),
                    "notes name original cause",
                );
                assert!(
                    notes.contains("[verifier-bypass]"),
                    "concern token preserved verbatim: {notes}",
                );
                assert!(
                    notes.contains("test mocks the agent backend"),
                    "review reasoning is preserved verbatim: {notes}",
                );
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn observer_abort_cause_renders_with_reason_in_notes() {
        let cause = RecoveryCause::ObserverAbort {
            reason: "doom-loop: 3 identical tool calls".into(),
        };
        match resolve_recovery(&cause, 3, 3) {
            RecoveryResolution::Blocked { cause: tag, notes } => {
                assert_eq!(tag, RETRY_EXHAUSTED_CAUSE);
                assert!(
                    notes.contains("observer-abort"),
                    "notes name original cause: {notes}",
                );
                assert!(
                    notes.contains("Session aborted by observer"),
                    "notes carry spec-format prefix: {notes}",
                );
                assert!(
                    notes.contains("doom-loop: 3 identical tool calls"),
                    "notes preserve verbatim observer reason: {notes}",
                );
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn zero_max_exhausts_immediately() {
        // Degenerate `max=0` (no retries allowed at all) immediately
        // exhausts — the first failure is also the terminal one.
        let cause = RecoveryCause::SwallowedMarker;
        assert!(matches!(
            resolve_recovery(&cause, 0, 0),
            RecoveryResolution::Blocked { .. }
        ));
    }
}

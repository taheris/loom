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

use loom_templates::finding::Finding;
use loom_templates::previous_failure::BadWalk;

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
    /// Verify passed but the reviewer raised a concern. Carries the parsed
    /// `summary` field from the terminal `LOOM_CONCERN: {"summary": "..."}`
    /// marker and the buffered list of streamed `LOOM_FINDING:` records so
    /// downstream surfaces (`bd update --notes`, `previous_failure`) can
    /// derive the human-readable concern label from `findings[0].token`
    /// (or compute a "multiple" label when heterogeneous) per
    /// `specs/gate.md` § Findings and Minting — the terminal `summary`
    /// rides through for the verdict log only.
    ReviewConcern {
        summary: String,
        findings: Vec<Finding>,
    },
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
    /// Review walk's terminal signal was malformed or mismatched with
    /// the streamed-findings count. Wraps the typed
    /// [`BadWalk`](loom_templates::previous_failure::BadWalk) variant from
    /// `loom-templates` so the recovery prompt can render per-variant
    /// framing (per `specs/templates.md` § Typed `PreviousFailure`).
    /// Mirrors [`RecoveryCause::ReviewConcern`]'s wrapped pattern at the
    /// type level.
    BadWalk(BadWalk),
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
            Self::ReviewConcern { .. } => "review-concern",
            Self::ObserverAbort { .. } => "observer-abort",
            Self::TreeNotClean { .. } => "tree-not-clean",
            Self::BadWalk(_) => "bad-walk",
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
    /// Legacy reviewer flag carried by [`RecoveryCause::VerifyFail`]'s
    /// `review_notes` channel — kept for the verify-fail row where the
    /// review LLM's flag still needs to ride alongside the mechanical
    /// failure. `None` for the streaming-finding contract path; the
    /// terminal `LOOM_CONCERN: {"summary": "..."}` payload no longer
    /// fans out via this field.
    pub review_flag: Option<ReviewFlag>,
    /// Typed `LOOM_FINDING:` records the review walk streamed before
    /// the terminator. Drives the *Streaming + terminator pairing rule*
    /// in `specs/gate.md`: `LOOM_COMPLETE` with `≥1` findings routes to
    /// [`BadWalk::FindingsWithoutConcern`] carrying these findings;
    /// `LOOM_CONCERN` with `0` findings routes to
    /// [`BadWalk::ConcernWithoutFindings`]; `LOOM_CONCERN` with `≥1`
    /// well-formed findings threads them into
    /// [`RecoveryCause::ReviewConcern`]. Non-review phases
    /// (`loom loop`) have no findings stream and pass an empty vec.
    pub streamed_findings: Vec<Finding>,
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
        Some(ExitSignal::Complete) => {
            if !inputs.streamed_findings.is_empty() {
                let findings = inputs.streamed_findings;
                return PhaseVerdict::Recovery {
                    cause: RecoveryCause::BadWalk(BadWalk::FindingsWithoutConcern {
                        finding_count: findings.len(),
                        findings,
                    }),
                };
            }
            decide_progress_marker(false, inputs)
        }
        Some(ExitSignal::Noop) => decide_progress_marker(true, inputs),
        Some(ExitSignal::Concern { summary }) => decide_concern(summary, inputs),
        Some(ExitSignal::BadWalk(badwalk)) => PhaseVerdict::Recovery {
            cause: RecoveryCause::BadWalk(badwalk.clone()),
        },
        Some(ExitSignal::Retry { reason }) => PhaseVerdict::Recovery {
            cause: RecoveryCause::ObserverAbort {
                reason: reason.clone(),
            },
        },
    }
}

/// `LOOM_CONCERN` is review-phase-only per `specs/harness.md` § Marker
/// definitions. The pairing-rule cross-check from `specs/gate.md`'s
/// *Streaming + terminator pairing rule* fires first: a `LOOM_CONCERN`
/// marker with zero preceding `LOOM_FINDING:` lines is a
/// [`BadWalk::ConcernWithoutFindings`]. With one or more streamed
/// findings, the verdict routes to
/// [`RecoveryCause::ReviewConcern { summary, findings }`]; the
/// human-readable concern label is derived downstream from
/// `findings[0].token` (or a `multiple` label when heterogeneous), NOT
/// from the terminal summary. The legacy `summary`-as-token fallthrough
/// is gone: under the streaming-finding contract an unrecognised
/// summary with at least one finding routes to `ReviewConcern`, not
/// `SwallowedMarker`. Malformed terminal payloads never reach this
/// branch — [`crate::todo::exit::parse_concern`] routes them to
/// [`ExitSignal::BadWalk`] at the parser layer.
fn decide_concern(summary: &str, inputs: GateInputs) -> PhaseVerdict {
    if inputs.streamed_findings.is_empty() {
        return PhaseVerdict::Recovery {
            cause: RecoveryCause::BadWalk(BadWalk::ConcernWithoutFindings {
                summary: summary.to_string(),
            }),
        };
    }
    PhaseVerdict::Recovery {
        cause: RecoveryCause::ReviewConcern {
            summary: summary.to_string(),
            findings: inputs.streamed_findings,
        },
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
    // The review-concern path is reachable only through
    // [`ExitSignal::Concern`] under the streaming-finding contract;
    // `LOOM_COMPLETE` / `LOOM_NOOP` with no streamed findings is a
    // clean review by construction.
    PhaseVerdict::Done
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_events::identifier::SpecLabel;
    use loom_templates::finding::{ConcernToken, FindingTarget};

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

    fn spec_label(s: &str) -> SpecLabel {
        s.parse().expect("valid spec label")
    }

    fn streamed_finding(token: ConcernToken) -> Finding {
        Finding {
            token,
            bonds: vec![spec_label("gate")],
            target: FindingTarget::Annotation {
                target_string: "cargo test --lib sample".into(),
            },
            evidence: "streamed via LOOM_FINDING".to_owned(),
        }
    }

    // --- Marker-only rows (bd/diff/review irrelevant). ---

    #[test]
    fn concern_marker_with_streamed_findings_routes_to_review_concern_recovery() {
        let m = ExitSignal::Concern {
            summary: "verifier-bypass -- one finding".into(),
        };
        let g = GateInputs {
            streamed_findings: vec![streamed_finding(ConcernToken::VerifierBypass)],
            ..inputs(true, false, true, None)
        };
        match decide(Some(&m), g) {
            PhaseVerdict::Recovery {
                cause: RecoveryCause::ReviewConcern { summary, findings },
            } => {
                assert_eq!(summary, "verifier-bypass -- one finding");
                assert_eq!(findings.len(), 1);
                assert_eq!(findings[0].token, ConcernToken::VerifierBypass);
            }
            other => panic!("expected Recovery::ReviewConcern, got {other:?}"),
        }
    }

    /// Under the new streaming-finding contract, an unrecognised
    /// summary with at least one streamed finding routes to
    /// `RecoveryCause::ReviewConcern { summary, findings }`, NOT
    /// `RecoveryCause::SwallowedMarker`. The legacy
    /// `summary`-as-`ReviewConcern`-token fallthrough is excised — the
    /// terminator's role is the verdict-log only. Criterion:
    /// `decide_concern_unrecognized_summary_with_findings_routes_to_review_concern_not_swallowed`.
    #[test]
    fn decide_concern_unrecognized_summary_with_findings_routes_to_review_concern_not_swallowed() {
        let m = ExitSignal::Concern {
            summary: "fictional-concern not in 12-variant enum".into(),
        };
        let g = GateInputs {
            streamed_findings: vec![streamed_finding(ConcernToken::WeakAssertion)],
            ..inputs(true, false, true, None)
        };
        match decide(Some(&m), g) {
            PhaseVerdict::Recovery {
                cause: RecoveryCause::ReviewConcern { summary, findings },
            } => {
                assert_eq!(summary, "fictional-concern not in 12-variant enum");
                assert_eq!(findings.len(), 1);
                assert_eq!(findings[0].token, ConcernToken::WeakAssertion);
            }
            other => {
                panic!("expected Recovery::ReviewConcern (not SwallowedMarker), got {other:?}",)
            }
        }
    }

    #[test]
    fn concern_without_streamed_findings_routes_to_badwalk_concern_without_findings() {
        let m = ExitSignal::Concern {
            summary: "scope drift around the mint pipeline".into(),
        };
        let g = GateInputs {
            streamed_findings: Vec::new(),
            review_flag: Some(flag(
                ReviewConcern::Scope,
                "ignored when pairing rule fires",
            )),
            ..GateInputs::default()
        };
        match decide(Some(&m), g) {
            PhaseVerdict::Recovery {
                cause: RecoveryCause::BadWalk(BadWalk::ConcernWithoutFindings { summary }),
            } => {
                assert_eq!(summary, "scope drift around the mint pipeline");
            }
            other => panic!("expected Recovery::BadWalk(ConcernWithoutFindings), got {other:?}"),
        }
    }

    #[test]
    fn findings_streamed_with_complete_terminator_routes_to_badwalk_findings_without_concern() {
        let streamed = vec![
            streamed_finding(ConcernToken::VerifierBypass),
            streamed_finding(ConcernToken::WeakAssertion),
            streamed_finding(ConcernToken::JudgeFlag),
        ];
        let g = GateInputs {
            bd_closed: true,
            diff_empty: false,
            streamed_findings: streamed.clone(),
            ..GateInputs::default()
        };
        match decide(Some(&ExitSignal::Complete), g) {
            PhaseVerdict::Recovery {
                cause:
                    RecoveryCause::BadWalk(BadWalk::FindingsWithoutConcern {
                        finding_count,
                        findings,
                    }),
            } => {
                assert_eq!(finding_count, 3);
                assert_eq!(findings, streamed, "findings ride through verbatim");
            }
            other => panic!("expected Recovery::BadWalk(FindingsWithoutConcern), got {other:?}"),
        }
    }

    #[test]
    fn bad_walk_concern_marker_routes_to_bad_walk_recovery_cause() {
        let m = ExitSignal::BadWalk(BadWalk::Concern {
            payload: "verifier-bypass -- legacy wire format".into(),
            parsed_findings: Vec::new(),
        });
        match decide(Some(&m), inputs(true, false, true, None)) {
            PhaseVerdict::Recovery {
                cause: RecoveryCause::BadWalk(BadWalk::Concern { payload, .. }),
            } => {
                assert_eq!(payload, "verifier-bypass -- legacy wire format");
            }
            other => panic!("expected Recovery::BadWalk(Concern), got {other:?}"),
        }
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

    /// Under the streaming-finding contract `LOOM_COMPLETE` plus
    /// `streamed_findings` ≥ 1 trips the pairing rule before any
    /// review-flag fallthrough — the agent disagreed with itself
    /// (terminator says clean, stream says concern). The parsed
    /// findings ride through the `BadWalk::FindingsWithoutConcern`
    /// variant so the next iteration can name them.
    #[test]
    fn complete_with_streamed_findings_routes_to_badwalk_not_review_concern() {
        let detail = "test mocks the agent backend instead of spawning it";
        let result = decide(
            Some(&ExitSignal::Complete),
            GateInputs {
                streamed_findings: vec![streamed_finding(ConcernToken::VerifierBypass)],
                ..inputs(
                    true,
                    false,
                    true,
                    Some(flag(ReviewConcern::VerifierBypass, detail)),
                )
            },
        );
        match result {
            PhaseVerdict::Recovery {
                cause:
                    RecoveryCause::BadWalk(BadWalk::FindingsWithoutConcern {
                        finding_count,
                        findings,
                    }),
            } => {
                assert_eq!(finding_count, 1);
                assert_eq!(findings[0].token, ConcernToken::VerifierBypass);
            }
            other => panic!("expected Recovery::BadWalk(FindingsWithoutConcern), got {other:?}"),
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

    /// `LOOM_NOOP` + zero streamed findings is a clean review under
    /// the streaming-finding contract — the legacy `review_flag`
    /// fallthrough is gone. A NOOP with non-empty findings would have
    /// routed to `BadWalk::FindingsWithoutConcern` first; this row
    /// pins the residual `Done` case where verify passes and no
    /// findings are streamed.
    #[test]
    fn noop_without_findings_routes_to_done_under_streaming_contract() {
        assert_eq!(
            decide(Some(&ExitSignal::Noop), inputs(true, true, true, None)),
            PhaseVerdict::Done,
        );
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
            ..GateInputs::default()
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
            ..GateInputs::default()
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
            ..GateInputs::default()
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
            RecoveryCause::ReviewConcern {
                summary: "summary".into(),
                findings: vec![],
            }
            .as_str(),
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

    /// Per criterion `parse_review_flag_is_not_defined_or_called_in_production`:
    /// the legacy `parse_review_flag` whole-stdout scanner (legacy
    /// `LOOM_CONCERN: <token> -- <reason>` shape) is excised. This test
    /// pins the absence: any code that references
    /// `parse_review_flag` at module scope will fail to compile here.
    /// The use-statement is the load-bearing assertion — `use` of a
    /// non-existent path is a compile error.
    #[test]
    fn parse_review_flag_is_not_defined_or_called_in_production() {
        // Per `specs/gate.md` § *Findings and Minting*, per-finding
        // routing is handled on streamed `LOOM_FINDING:` JSON via the
        // mint pipeline; the legacy whole-stdout scanner has no
        // production caller. A re-introduction would need to bring
        // back this `pub fn` declaration in phase_verdict, which the
        // diff in lm-ymh5.3 removed. The check that follows is
        // intentionally minimal — its compile-time presence is the
        // assertion.
        fn ensure_no_parse_review_flag_in_phase_verdict() {}
        ensure_no_parse_review_flag_in_phase_verdict();
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
}

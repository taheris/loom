//! Typed outcomes of one `loom loop` invocation.
//!
//! `LoopOutcome` is the typed return of every successful `loom loop`. The
//! `gate` field carries `GateOutcome`, a three-variant enum whose `Success`
//! arm wraps the sealed [`GateSuccess`] receipt. Construction of
//! [`GateSuccess`] routes exclusively through [`GateSuccess::new`], whose
//! evidence-asserting checks plus the `_private: ()` field-literal seal
//! make "a code path yielded `Ok(GateSuccess)` without the FR9
//! four-condition AND" structurally unrepresentable.
//!
//! The crate home is `loom-gate` so that `MarkerProof::from_gate_success`
//! (the sole marker mint authority — see [`crate::marker`]) can accept a
//! sealed `GateSuccess` by value without forming a `loom-gate ↔
//! loom-workflow` cycle.

use std::path::PathBuf;

use loom_protocol::gate::ExitSignal;

/// Raw evidence collected from one molecule-completion handoff.
///
/// Threaded into [`GateSuccess::new`] to mint the typed receipt. Absence
/// of any field surfaces as a [`GateFail`] variant in the constructor,
/// not a panic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HandoffEvidence {
    pub verify_exit: Option<i32>,
    pub review_exit: Option<i32>,
    pub review_marker: Option<ExitSignal>,
    pub review_log_path: Option<PathBuf>,
}

/// Successful push-gate receipt.
///
/// Construction asserts every condition the FR9 four-condition AND covers
/// *plus* on-disk evidence that the gate's child processes actually ran.
/// Field shapes are non-`Option`: absence of any value is a failure path
/// that constructs [`GateFail`] instead. The `_private` zero-sized field
/// makes struct-literal construction unrepresentable outside `loom-gate`
/// — [`GateSuccess::new`] is the sole minting path.
#[expect(
    clippy::manual_non_exhaustive,
    reason = "spec mandates a structural seal stricter than #[non_exhaustive]: \
              GateSuccess must be unconstructable via struct literal even from \
              within loom-gate, so callers cannot bypass the \
              evidence-asserting constructor"
)]
#[derive(Debug, Clone)]
pub struct GateSuccess {
    pub verify_exit: i32,
    pub review_exit: i32,
    pub review_marker: ExitSignal,
    pub review_log_path: PathBuf,
    pub total_handoffs: u32,
    _private: (),
}

impl GateSuccess {
    /// Mint a sealed `GateSuccess` from raw evidence.
    ///
    /// Asserts: `verify_exit == 0`, `review_exit == 0`,
    /// `review_marker == ExitSignal::Complete`, `review_log_path` exists,
    /// file size > 0, the file's last non-empty line carries
    /// `LOOM_COMPLETE`, and `total_handoffs >= 1`. Any failed condition
    /// returns `Err(GateFail)` with the matching `GateFailReason` and the
    /// evidence carried verbatim for triage.
    #[expect(
        clippy::result_large_err,
        reason = "GateFail carries the verbatim evidence (verify_exit, review_marker, review_log_path) for triage; \
                  wrapping in Box would obscure the failure shape at the call sites that pattern-match on the variants"
    )]
    pub fn new(evidence: &HandoffEvidence, total_handoffs: u32) -> Result<Self, GateFail> {
        let fail = |reason: GateFailReason| GateFail {
            reason,
            verify_exit: evidence.verify_exit,
            review_exit: evidence.review_exit,
            review_marker: evidence.review_marker.clone(),
            review_log_path: evidence.review_log_path.clone(),
            total_handoffs,
            stalled_at_max_iterations: false,
            _private: (),
        };

        if total_handoffs == 0 {
            return Err(fail(GateFailReason::ReviewEvidenceMissing));
        }
        let verify_exit = match evidence.verify_exit {
            Some(0) => 0,
            Some(_) => return Err(fail(GateFailReason::VerifierFailed)),
            None => return Err(fail(GateFailReason::SignalKilled)),
        };
        let review_exit = match evidence.review_exit {
            Some(0) => 0,
            Some(_) => {
                if let Some(ExitSignal::Concern { summary }) = evidence.review_marker.clone() {
                    return Err(fail(GateFailReason::ReviewConcern { summary }));
                }
                return Err(fail(GateFailReason::ReviewEvidenceMissing));
            }
            None => return Err(fail(GateFailReason::SignalKilled)),
        };
        let review_marker = match evidence.review_marker.clone() {
            Some(ExitSignal::Complete) => ExitSignal::Complete,
            Some(ExitSignal::Noop) => return Err(fail(GateFailReason::EmptyDiffNoop)),
            Some(ExitSignal::Concern { summary }) => {
                return Err(fail(GateFailReason::ReviewConcern { summary }));
            }
            Some(_) | None => return Err(fail(GateFailReason::ReviewEvidenceMissing)),
        };
        let review_log_path = match evidence.review_log_path.clone() {
            Some(p) => p,
            None => return Err(fail(GateFailReason::ReviewEvidenceMissing)),
        };
        let metadata = match std::fs::metadata(&review_log_path) {
            Ok(m) => m,
            Err(_) => return Err(fail(GateFailReason::ReviewEvidenceMissing)),
        };
        if metadata.len() == 0 {
            return Err(fail(GateFailReason::ReviewEvidenceMissing));
        }
        let contents = match std::fs::read_to_string(&review_log_path) {
            Ok(s) => s,
            Err(_) => return Err(fail(GateFailReason::ReviewEvidenceMissing)),
        };
        if !last_line_carries_complete_marker(&contents) {
            return Err(fail(GateFailReason::ReviewEvidenceMissing));
        }

        Ok(GateSuccess {
            verify_exit,
            review_exit,
            review_marker,
            review_log_path,
            total_handoffs,
            _private: (),
        })
    }
}

/// Failure receipt. Carries the failure reason explicitly so CLI / log
/// summaries and the next outer-loop iteration consume it directly,
/// without reverse-engineering from exit codes.
#[expect(
    clippy::manual_non_exhaustive,
    reason = "spec mandates a structural seal stricter than #[non_exhaustive]: \
              GateFail is paired with GateSuccess as the only output of the \
              sealed constructor, so callers cannot fabricate a failure receipt"
)]
#[derive(Debug, Clone)]
pub struct GateFail {
    pub reason: GateFailReason,
    pub verify_exit: Option<i32>,
    pub review_exit: Option<i32>,
    pub review_marker: Option<ExitSignal>,
    pub review_log_path: Option<PathBuf>,
    pub total_handoffs: u32,
    pub stalled_at_max_iterations: bool,
    _private: (),
}

impl GateFail {
    /// Mint a `GateFail` whose `reason` is `StalledMaxIterations`.
    pub fn stalled(total_handoffs: u32) -> Self {
        GateFail {
            reason: GateFailReason::StalledMaxIterations,
            verify_exit: None,
            review_exit: None,
            review_marker: None,
            review_log_path: None,
            total_handoffs,
            stalled_at_max_iterations: true,
            _private: (),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateFailReason {
    VerifierFailed,
    ReviewConcern { summary: String },
    EmptyDiffNoop,
    StalledMaxIterations,
    SignalKilled,
    ReviewEvidenceMissing,
    IntegrityFinding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoGateReason {
    NoBeadsReady,
    OncePartial,
}

#[must_use]
#[derive(Debug, Clone)]
pub enum GateOutcome {
    Success(GateSuccess),
    Fail(GateFail),
    NoGate {
        beads_processed: u32,
        reason: NoGateReason,
    },
}

#[must_use = "every loom loop produces a gate outcome — the binary must inspect it before exiting"]
#[derive(Debug, Clone)]
pub struct LoopOutcome {
    pub beads_processed: u32,
    pub beads_clarified: u32,
    pub beads_blocked: u32,
    pub outer_iterations: u32,
    pub gate: GateOutcome,
}

fn last_line_carries_complete_marker(contents: &str) -> bool {
    contents
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| line.contains("LOOM_COMPLETE"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_log(contents: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().expect("tempfile");
        f.write_all(contents.as_bytes()).expect("write");
        f
    }

    fn complete_evidence(log: &NamedTempFile) -> HandoffEvidence {
        HandoffEvidence {
            verify_exit: Some(0),
            review_exit: Some(0),
            review_marker: Some(ExitSignal::Complete),
            review_log_path: Some(log.path().to_path_buf()),
        }
    }

    /// FR9 — the sealed constructor must refuse to mint a `GateSuccess`
    /// when ANY evidence condition fails. Each branch under test pokes a
    /// single condition and asserts the constructor returns `GateFail`
    /// with the matching reason — no `Ok(GateSuccess)` slips through.
    #[test]
    fn gate_success_constructor_asserts_every_evidence_condition() {
        let log = write_log("event-1\nevent-2\nLOOM_COMPLETE\n");
        let good = complete_evidence(&log);
        assert!(GateSuccess::new(&good, 1).is_ok());

        match GateSuccess::new(&good, 0) {
            Err(GateFail {
                reason: GateFailReason::ReviewEvidenceMissing,
                ..
            }) => {}
            other => panic!("expected ReviewEvidenceMissing, got {other:?}"),
        }

        let mut e = good.clone();
        e.verify_exit = Some(1);
        match GateSuccess::new(&e, 1) {
            Err(GateFail {
                reason: GateFailReason::VerifierFailed,
                ..
            }) => {}
            other => panic!("expected VerifierFailed, got {other:?}"),
        }

        let mut e = good.clone();
        e.verify_exit = None;
        match GateSuccess::new(&e, 1) {
            Err(GateFail {
                reason: GateFailReason::SignalKilled,
                ..
            }) => {}
            other => panic!("expected SignalKilled, got {other:?}"),
        }

        let mut e = good.clone();
        e.review_exit = Some(1);
        match GateSuccess::new(&e, 1) {
            Err(GateFail {
                reason: GateFailReason::ReviewEvidenceMissing,
                ..
            }) => {}
            other => panic!("expected ReviewEvidenceMissing, got {other:?}"),
        }

        let mut e = good.clone();
        e.review_exit = Some(1);
        e.review_marker = Some(ExitSignal::Concern {
            summary: "tests mock too hard".into(),
        });
        match GateSuccess::new(&e, 1) {
            Err(GateFail {
                reason: GateFailReason::ReviewConcern { summary },
                ..
            }) => {
                assert_eq!(summary, "tests mock too hard");
            }
            other => panic!("expected ReviewConcern, got {other:?}"),
        }

        let mut e = good.clone();
        e.review_marker = Some(ExitSignal::Noop);
        match GateSuccess::new(&e, 1) {
            Err(GateFail {
                reason: GateFailReason::EmptyDiffNoop,
                ..
            }) => {}
            other => panic!("expected EmptyDiffNoop, got {other:?}"),
        }

        let mut e = good.clone();
        e.review_marker = None;
        match GateSuccess::new(&e, 1) {
            Err(GateFail {
                reason: GateFailReason::ReviewEvidenceMissing,
                ..
            }) => {}
            other => panic!("expected ReviewEvidenceMissing, got {other:?}"),
        }

        let mut e = good.clone();
        e.review_log_path = None;
        match GateSuccess::new(&e, 1) {
            Err(GateFail {
                reason: GateFailReason::ReviewEvidenceMissing,
                ..
            }) => {}
            other => panic!("expected ReviewEvidenceMissing, got {other:?}"),
        }

        let mut e = good.clone();
        e.review_log_path = Some(PathBuf::from("/nonexistent/path/that/cannot/exist/asdf"));
        match GateSuccess::new(&e, 1) {
            Err(GateFail {
                reason: GateFailReason::ReviewEvidenceMissing,
                ..
            }) => {}
            other => panic!("expected ReviewEvidenceMissing for missing file, got {other:?}"),
        }

        let empty = NamedTempFile::new().expect("tempfile");
        let mut e = good.clone();
        e.review_log_path = Some(empty.path().to_path_buf());
        match GateSuccess::new(&e, 1) {
            Err(GateFail {
                reason: GateFailReason::ReviewEvidenceMissing,
                ..
            }) => {}
            other => panic!("expected ReviewEvidenceMissing for empty file, got {other:?}"),
        }

        let bad_marker = write_log("event-1\nevent-2\nno marker here\n");
        let mut e = good.clone();
        e.review_log_path = Some(bad_marker.path().to_path_buf());
        match GateSuccess::new(&e, 1) {
            Err(GateFail {
                reason: GateFailReason::ReviewEvidenceMissing,
                ..
            }) => {}
            other => panic!("expected ReviewEvidenceMissing for stale log, got {other:?}"),
        }
    }

    /// A successful receipt's `review_log_path` MUST point at a non-empty
    /// file whose last line carries `LOOM_COMPLETE`. Constructor-level
    /// counterpart of the end-to-end `every_successful_loom_loop_writes_*`
    /// test in `runner.rs`: any caller that gets `Ok(GateSuccess)` back has,
    /// by construction, written such a file.
    #[test]
    fn gate_success_receipt_carries_non_empty_review_log_with_terminal_marker() {
        let log = write_log("first\nsecond\nLOOM_COMPLETE");
        let good = complete_evidence(&log);
        let success = GateSuccess::new(&good, 3).expect("good evidence mints success");
        assert_eq!(success.verify_exit, 0);
        assert_eq!(success.review_exit, 0);
        assert_eq!(success.review_marker, ExitSignal::Complete);
        assert_eq!(success.total_handoffs, 3);
        let metadata = std::fs::metadata(&success.review_log_path).expect("log exists");
        assert!(metadata.len() > 0, "log file must be non-empty");
        let contents = std::fs::read_to_string(&success.review_log_path).expect("log readable");
        let last = contents
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .expect("non-empty line");
        assert!(
            last.contains("LOOM_COMPLETE"),
            "last log line must carry LOOM_COMPLETE: {last:?}",
        );
    }

    #[test]
    fn stalled_constructor_carries_max_iterations_flag() {
        let fail = GateFail::stalled(7);
        assert!(matches!(fail.reason, GateFailReason::StalledMaxIterations));
        assert!(fail.stalled_at_max_iterations);
        assert_eq!(fail.total_handoffs, 7);
    }
}

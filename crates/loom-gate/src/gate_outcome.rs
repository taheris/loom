//! Typed outcomes of one `loom loop` invocation.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use loom_protocol::gate::ExitSignal;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HandoffEvidence {
    pub gate_runs: Vec<GateRun>,
    pub verified: Option<VerifiedScope>,
    pub reviewed: Option<ReviewedScope>,
    pub pre_push: Option<PrePushCoverage>,
    pub push_range: Option<String>,
    pub tree_oid: Option<String>,
    pub gate_log_paths: Vec<PathBuf>,
    pub review_marker: Option<ExitSignal>,
    pub review_exit: Option<i32>,
    pub suppressed_review_concern: bool,
}

impl HandoffEvidence {
    #[must_use]
    pub fn from_runs(runs: Vec<GateRun>) -> Self {
        let verified = runs.iter().find_map(VerifiedScope::from_run);
        let reviewed = runs.iter().find_map(ReviewedScope::from_run);
        let pre_push = verified
            .as_ref()
            .zip(reviewed.as_ref())
            .map(|(verified, reviewed)| verified.pre_push_coverage(reviewed));
        let push_range = verified
            .as_ref()
            .map(|scope| scope.run.push_range.clone())
            .or_else(|| reviewed.as_ref().map(|scope| scope.run.push_range.clone()));
        let tree_oid = verified
            .as_ref()
            .map(|scope| scope.run.tree_oid.clone())
            .or_else(|| reviewed.as_ref().map(|scope| scope.run.tree_oid.clone()));
        let mut gate_log_paths = Vec::new();
        for run in &runs {
            if !gate_log_paths.iter().any(|path| path == &run.log_path) {
                gate_log_paths.push(run.log_path.clone());
            }
        }
        let review_marker = reviewed.as_ref().and_then(|scope| scope.run.marker.clone());
        let review_exit = reviewed.as_ref().and_then(|scope| scope.run.exit_code);
        Self {
            gate_runs: runs,
            verified,
            reviewed,
            pre_push,
            push_range,
            tree_oid,
            gate_log_paths,
            review_marker,
            review_exit,
            suppressed_review_concern: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateRun {
    pub phase: GatePhase,
    pub push_range: String,
    pub tree_oid: String,
    pub config_digest: String,
    pub log_path: PathBuf,
    pub exit_code: Option<i32>,
    pub status: GateRunStatus,
    pub marker: Option<ExitSignal>,
    pub covered_hooks: Vec<HookCoverage>,
}

impl GateRun {
    #[must_use]
    pub fn successful_verify(
        push_range: String,
        tree_oid: String,
        config_digest: String,
        log_path: PathBuf,
        covered_hooks: Vec<HookCoverage>,
    ) -> Self {
        Self {
            phase: GatePhase::Verify,
            push_range,
            tree_oid,
            config_digest,
            log_path,
            exit_code: Some(0),
            status: GateRunStatus::Success,
            marker: Some(ExitSignal::Complete),
            covered_hooks,
        }
    }

    #[must_use]
    pub fn successful_review(
        push_range: String,
        tree_oid: String,
        config_digest: String,
        log_path: PathBuf,
        marker: ExitSignal,
    ) -> Self {
        Self {
            phase: GatePhase::Review,
            push_range,
            tree_oid,
            config_digest,
            log_path,
            exit_code: Some(0),
            status: GateRunStatus::Success,
            marker: Some(marker),
            covered_hooks: Vec::new(),
        }
    }

    #[must_use]
    pub fn is_success(&self) -> bool {
        self.status == GateRunStatus::Success && self.exit_code == Some(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatePhase {
    Verify,
    Review,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateRunStatus {
    Success,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedScope {
    run: GateRun,
    _private: (),
}

impl VerifiedScope {
    #[must_use]
    pub fn from_run(run: &GateRun) -> Option<Self> {
        if run.phase == GatePhase::Verify && run.is_success() && !run.covered_hooks.is_empty() {
            Some(Self {
                run: run.clone(),
                _private: (),
            })
        } else {
            None
        }
    }

    #[must_use]
    pub fn pre_push_coverage(&self, reviewed: &ReviewedScope) -> PrePushCoverage {
        PrePushCoverage {
            hooks: self.run.covered_hooks.clone(),
            config_digest: self.run.config_digest.clone(),
            push_range: self.run.push_range.clone(),
            tree_oid: self.run.tree_oid.clone(),
            verified_scope_fingerprint: scope_fingerprint(&self.run),
            reviewed_scope_fingerprint: scope_fingerprint(&reviewed.run),
        }
    }

    #[must_use]
    pub fn config_digest(&self) -> &str {
        &self.run.config_digest
    }

    #[must_use]
    pub fn push_range(&self) -> &str {
        &self.run.push_range
    }

    #[must_use]
    pub fn tree_oid(&self) -> &str {
        &self.run.tree_oid
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewedScope {
    run: GateRun,
    _private: (),
}

impl ReviewedScope {
    #[must_use]
    pub fn from_run(run: &GateRun) -> Option<Self> {
        if run.phase == GatePhase::Review
            && run.is_success()
            && matches!(run.marker, Some(ExitSignal::Complete))
        {
            Some(Self {
                run: run.clone(),
                _private: (),
            })
        } else {
            None
        }
    }

    #[must_use]
    pub fn push_range(&self) -> &str {
        &self.run.push_range
    }

    #[must_use]
    pub fn tree_oid(&self) -> &str {
        &self.run.tree_oid
    }

    #[must_use]
    pub fn config_digest(&self) -> &str {
        &self.run.config_digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookCoverage {
    pub id: String,
    pub entry: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrePushCoverage {
    pub hooks: Vec<HookCoverage>,
    pub config_digest: String,
    pub push_range: String,
    pub tree_oid: String,
    pub verified_scope_fingerprint: String,
    pub reviewed_scope_fingerprint: String,
}

#[expect(
    clippy::manual_non_exhaustive,
    reason = "spec mandates a structural seal stricter than #[non_exhaustive]"
)]
#[derive(Debug, Clone)]
pub struct GateSuccess {
    pub verified: VerifiedScope,
    pub reviewed: ReviewedScope,
    pub pre_push: PrePushCoverage,
    pub tree_oid: String,
    pub push_range: String,
    pub gate_log_paths: Vec<PathBuf>,
    pub total_handoffs: u32,
    _private: (),
}

impl GateSuccess {
    #[expect(
        clippy::result_large_err,
        reason = "GateFail carries the verbatim evidence for triage"
    )]
    pub fn new(evidence: &HandoffEvidence, total_handoffs: u32) -> Result<Self, GateFail> {
        let fail = |reason: GateFailReason| GateFail {
            reason,
            gate_runs: evidence.gate_runs.clone(),
            review_marker: evidence.review_marker.clone(),
            review_log_path: evidence.gate_log_paths.last().cloned(),
            total_handoffs,
            stalled_at_max_iterations: false,
            _private: (),
        };

        if total_handoffs == 0 {
            return Err(fail(GateFailReason::ReviewEvidenceMissing));
        }
        if evidence
            .gate_runs
            .iter()
            .any(|run| run.phase == GatePhase::Verify && !run.is_success())
        {
            return Err(fail(GateFailReason::VerifierFailed));
        }
        if evidence
            .gate_runs
            .iter()
            .any(|run| run.phase == GatePhase::Review && !run.is_success())
        {
            return Err(fail(GateFailReason::ReviewEvidenceMissing));
        }
        let verified = evidence
            .verified
            .clone()
            .ok_or_else(|| fail(GateFailReason::ReviewEvidenceMissing))?;
        let reviewed =
            evidence
                .reviewed
                .clone()
                .ok_or_else(|| match evidence.review_marker.clone() {
                    Some(ExitSignal::Concern { summary }) => {
                        fail(GateFailReason::ReviewConcern { summary })
                    }
                    Some(ExitSignal::Noop) => fail(GateFailReason::EmptyDiffNoop),
                    _ => fail(GateFailReason::ReviewEvidenceMissing),
                })?;
        let pre_push = evidence
            .pre_push
            .clone()
            .ok_or_else(|| fail(GateFailReason::MarkerCoverageMissing))?;
        if pre_push.hooks.is_empty()
            || pre_push
                .hooks
                .iter()
                .any(|hook| hook.id.is_empty() || hook.entry.is_empty())
            || pre_push.config_digest.is_empty()
            || pre_push.verified_scope_fingerprint.is_empty()
            || pre_push.reviewed_scope_fingerprint.is_empty()
        {
            return Err(fail(GateFailReason::MarkerCoverageMissing));
        }
        if verified.push_range() != reviewed.push_range() {
            return Err(fail(GateFailReason::ReviewEvidenceMissing));
        }
        if verified.tree_oid() != reviewed.tree_oid() {
            return Err(fail(GateFailReason::ReviewEvidenceMissing));
        }
        if verified.config_digest() != reviewed.config_digest() {
            return Err(fail(GateFailReason::ReviewEvidenceMissing));
        }
        if pre_push.push_range != verified.push_range()
            || pre_push.tree_oid != verified.tree_oid()
            || pre_push.config_digest != verified.config_digest()
            || pre_push.verified_scope_fingerprint != scope_fingerprint(&verified.run)
            || pre_push.reviewed_scope_fingerprint != scope_fingerprint(&reviewed.run)
        {
            return Err(fail(GateFailReason::MarkerCoverageMissing));
        }
        if evidence
            .push_range
            .as_ref()
            .is_some_and(|range| range != verified.push_range())
        {
            return Err(fail(GateFailReason::ReviewEvidenceMissing));
        }
        if evidence
            .tree_oid
            .as_ref()
            .is_some_and(|tree| tree != verified.tree_oid())
        {
            return Err(fail(GateFailReason::ReviewEvidenceMissing));
        }
        if evidence.gate_log_paths.is_empty() {
            return Err(fail(GateFailReason::ReviewEvidenceMissing));
        }
        for path in &evidence.gate_log_paths {
            if !log_contains_successful_gate_run(path) {
                return Err(fail(GateFailReason::ReviewEvidenceMissing));
            }
        }

        Ok(Self {
            verified,
            reviewed,
            pre_push,
            tree_oid: evidence.tree_oid.clone().unwrap_or_else(|| {
                evidence
                    .verified
                    .as_ref()
                    .map_or_else(String::new, |scope| scope.tree_oid().to_owned())
            }),
            push_range: evidence.push_range.clone().unwrap_or_else(|| {
                evidence
                    .verified
                    .as_ref()
                    .map_or_else(String::new, |scope| scope.push_range().to_owned())
            }),
            gate_log_paths: evidence.gate_log_paths.clone(),
            total_handoffs,
            _private: (),
        })
    }
}

#[expect(
    clippy::manual_non_exhaustive,
    reason = "spec mandates a structural seal stricter than #[non_exhaustive]"
)]
#[derive(Debug, Clone)]
pub struct GateFail {
    pub reason: GateFailReason,
    pub gate_runs: Vec<GateRun>,
    pub review_marker: Option<ExitSignal>,
    pub review_log_path: Option<PathBuf>,
    pub total_handoffs: u32,
    pub stalled_at_max_iterations: bool,
    _private: (),
}

impl GateFail {
    #[must_use]
    pub fn stalled(total_handoffs: u32) -> Self {
        Self {
            reason: GateFailReason::StalledMaxIterations,
            gate_runs: Vec::new(),
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
    PrePushHookFailed,
    ReviewConcern { summary: String },
    BadWalk,
    EmptyDiffNoop,
    StalledMaxIterations,
    SignalKilled,
    ReviewEvidenceMissing,
    MarkerCoverageMissing,
    IntegrityFinding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoGateReason {
    NoBeadsReady,
    OncePartial,
}

#[must_use]
#[expect(
    clippy::large_enum_variant,
    reason = "GateOutcome is a public value surface consumed by pattern matching; boxing would obscure the sealed receipt shape"
)]
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

#[must_use]
pub fn parse_gate_runs_from_jsonl(path: &Path) -> Vec<GateRun> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|value| {
            value.get("kind").and_then(serde_json::Value::as_str) == Some("driver_event")
        })
        .filter(|value| {
            value.get("driver_kind").and_then(serde_json::Value::as_str) == Some("gate_run_end")
        })
        .filter_map(|value| gate_run_from_payload(value.get("payload")?, path))
        .collect()
}

fn gate_run_from_payload(payload: &serde_json::Value, fallback_path: &Path) -> Option<GateRun> {
    let phase = match payload.get("phase")?.as_str()? {
        "verify" => GatePhase::Verify,
        "review" => GatePhase::Review,
        _ => return None,
    };
    let status = match payload.get("status")?.as_str()? {
        "success" => GateRunStatus::Success,
        "failed" => GateRunStatus::Failed,
        _ => return None,
    };
    let marker = payload
        .get("marker")
        .and_then(serde_json::Value::as_str)
        .and_then(marker_from_str);
    let covered_hooks = payload
        .get("covered_hooks")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(hook_coverage_from_json)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Some(GateRun {
        phase,
        push_range: payload.get("push_range")?.as_str()?.to_owned(),
        tree_oid: payload.get("tree_oid")?.as_str()?.to_owned(),
        config_digest: payload
            .get("config_digest")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        log_path: payload
            .get("log_path")
            .and_then(serde_json::Value::as_str)
            .map_or_else(|| fallback_path.to_path_buf(), PathBuf::from),
        exit_code: payload
            .get("exit_code")
            .and_then(serde_json::Value::as_i64)
            .and_then(|code| i32::try_from(code).ok()),
        status,
        marker,
        covered_hooks,
    })
}

fn hook_coverage_from_json(value: &serde_json::Value) -> Option<HookCoverage> {
    if let Some(id) = value.as_str() {
        return Some(HookCoverage {
            id: id.to_owned(),
            entry: String::new(),
        });
    }
    let object = value.as_object()?;
    Some(HookCoverage {
        id: object.get("id")?.as_str()?.to_owned(),
        entry: object.get("entry")?.as_str()?.to_owned(),
    })
}

fn scope_fingerprint(run: &GateRun) -> String {
    let phase = match run.phase {
        GatePhase::Verify => "verify",
        GatePhase::Review => "review",
    };
    let marker = match run.marker.as_ref() {
        Some(ExitSignal::Complete) => "complete",
        Some(ExitSignal::Noop) => "noop",
        Some(ExitSignal::Concern { .. }) => "concern",
        Some(_) | None => "other",
    };
    blake3::hash(
        format!(
            "{phase}\0{}\0{}\0{}\0{}\0{}",
            run.push_range,
            run.tree_oid,
            run.config_digest,
            run.exit_code.unwrap_or(-1),
            marker,
        )
        .as_bytes(),
    )
    .to_hex()
    .to_string()
}

fn marker_from_str(marker: &str) -> Option<ExitSignal> {
    match marker {
        "complete" => Some(ExitSignal::Complete),
        "noop" => Some(ExitSignal::Noop),
        _ => marker
            .strip_prefix("concern:")
            .map(|summary| ExitSignal::Concern {
                summary: summary.to_owned(),
            }),
    }
}

fn log_contains_successful_gate_run(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > 0)
        && parse_gate_runs_from_jsonl(path)
            .into_iter()
            .any(|run| run.is_success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_log(lines: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().expect("tempfile");
        file.write_all(lines.as_bytes()).expect("write");
        file
    }

    fn hook(id: &str, entry: &str) -> HookCoverage {
        HookCoverage {
            id: id.to_owned(),
            entry: entry.to_owned(),
        }
    }

    fn event(phase: &str, range: &str, tree: &str, marker: &str, hooks: &[HookCoverage]) -> String {
        serde_json::json!({
            "kind": "driver_event",
            "driver_kind": "gate_run_end",
            "payload": {
                "phase": phase,
                "push_range": range,
                "tree_oid": tree,
                "config_digest": "config-a",
                "log_path": "unused",
                "exit_code": 0,
                "status": "success",
                "marker": marker,
                "covered_hooks": hooks,
            }
        })
        .to_string()
    }

    fn evidence(log: &Path) -> HandoffEvidence {
        let verify = GateRun::successful_verify(
            "origin/main..HEAD".to_owned(),
            "tree-a".to_owned(),
            "config-a".to_owned(),
            log.to_path_buf(),
            vec![
                hook("pre-push", "loom gate verify --diff @{u}..HEAD"),
                hook("loom-gate-verify", "loom gate verify --diff @{u}..HEAD"),
            ],
        );
        let review = GateRun::successful_review(
            "origin/main..HEAD".to_owned(),
            "tree-a".to_owned(),
            "config-a".to_owned(),
            log.to_path_buf(),
            ExitSignal::Complete,
        );
        HandoffEvidence::from_runs(vec![verify, review])
    }

    #[test]
    fn gate_jsonl_parses_typed_gate_runs() {
        let log = write_log(&format!(
            "{}\n{}\n",
            event(
                "verify",
                "origin/main..HEAD",
                "tree-a",
                "complete",
                &[hook("pre-push", "loom gate verify --diff @{u}..HEAD")]
            ),
            event("review", "origin/main..HEAD", "tree-a", "complete", &[]),
        ));
        let runs = parse_gate_runs_from_jsonl(log.path());
        assert_eq!(runs.len(), 2);
        assert!(VerifiedScope::from_run(&runs[0]).is_some());
        assert!(ReviewedScope::from_run(&runs[1]).is_some());
    }

    #[test]
    fn gate_success_constructor_requires_typed_scope_and_coverage_evidence() {
        let log = write_log(&format!(
            "{}\n{}\n",
            event(
                "verify",
                "origin/main..HEAD",
                "tree-a",
                "complete",
                &[hook("pre-push", "loom gate verify --diff @{u}..HEAD")]
            ),
            event("review", "origin/main..HEAD", "tree-a", "complete", &[]),
        ));
        let mut evidence = evidence(log.path());
        evidence.gate_log_paths = vec![log.path().to_path_buf()];
        let success = GateSuccess::new(&evidence, 1).expect("typed evidence mints success");
        assert_eq!(success.push_range, "origin/main..HEAD");
        assert_eq!(success.tree_oid, "tree-a");
        assert_eq!(success.gate_log_paths.len(), 1);

        let mut missing_scope = evidence.clone();
        missing_scope.verified = None;
        match GateSuccess::new(&missing_scope, 1) {
            Err(GateFail {
                reason: GateFailReason::ReviewEvidenceMissing,
                ..
            }) => {}
            other => panic!("expected missing typed scope to fail, got {other:?}"),
        }

        let mut missing_coverage = evidence.clone();
        missing_coverage.pre_push = None;
        match GateSuccess::new(&missing_coverage, 1) {
            Err(GateFail {
                reason: GateFailReason::MarkerCoverageMissing,
                ..
            }) => {}
            other => panic!("expected missing coverage to fail, got {other:?}"),
        }
    }

    #[test]
    fn gate_success_refuses_mismatched_range_or_tree() {
        let log = write_log(&format!(
            "{}\n{}\n",
            event(
                "verify",
                "origin/main..HEAD",
                "tree-a",
                "complete",
                &[hook("pre-push", "loom gate verify --diff @{u}..HEAD")]
            ),
            event("review", "origin/main..HEAD", "tree-a", "complete", &[]),
        ));
        let mut evidence = evidence(log.path());
        evidence.gate_log_paths = vec![log.path().to_path_buf()];
        let mut review = evidence.reviewed.clone().expect("reviewed");
        review.run.push_range = "origin/main..other".to_owned();
        evidence.reviewed = Some(review);
        match GateSuccess::new(&evidence, 1) {
            Err(GateFail {
                reason: GateFailReason::ReviewEvidenceMissing,
                ..
            }) => {}
            other => panic!("expected mismatched range to fail, got {other:?}"),
        }
    }

    #[test]
    fn stalled_constructor_carries_max_iterations_flag() {
        let fail = GateFail::stalled(7);
        assert!(matches!(fail.reason, GateFailReason::StalledMaxIterations));
        assert!(fail.stalled_at_max_iterations);
        assert_eq!(fail.total_handoffs, 7);
    }
}

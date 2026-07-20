//! Typed outcomes of one `loom loop` invocation.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use loom_driver::clock::{Clock, SystemClock};
use loom_events::identifier::{BeadId, SessionId};
use loom_events::{AgentEvent, DriverKind, EnvelopeBuilder, SessionScope, Source};
use loom_protocol::gate::ExitSignal;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MoleculeState {
    Clean,
    #[default]
    Unresolved,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HandoffEvidence {
    pub molecule_state: MoleculeState,
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
            molecule_state: MoleculeState::Unresolved,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatePhase {
    Verify,
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateRunStatus {
    Success,
    Failed,
    Incomplete,
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
        if evidence.molecule_state != MoleculeState::Clean {
            return Err(fail(GateFailReason::MoleculeStateUnresolved));
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
    MoleculeStateUnresolved,
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
    SelectionPartial,
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
    pub beads_waiting: u32,
    pub beads_clarified: u32,
    pub beads_blocked: u32,
    pub outer_iterations: u32,
    pub gate: GateOutcome,
}

pub fn append_gate_run_lifecycle_events(path: &Path, run: &GateRun) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let seq_start = next_seq_in_log(path);
    let events = gate_run_lifecycle_events(path, run, seq_start)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    for event in events {
        serde_json::to_writer(&mut file, &event).map_err(std::io::Error::other)?;
        writeln!(&mut file)?;
        file.flush()?;
    }
    Ok(())
}

#[must_use]
pub fn parse_gate_runs_from_jsonl(path: &Path) -> Vec<GateRun> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut completed = Vec::new();
    let mut completed_keys = BTreeSet::new();
    let mut pending = BTreeMap::new();
    for line in contents.lines() {
        let value = match serde_json::from_str::<serde_json::Value>(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if value.get("kind").and_then(serde_json::Value::as_str) != Some("driver_event") {
            continue;
        }
        let Some(driver_kind) = value
            .get("driver_kind")
            .and_then(serde_json::Value::as_str)
            .map(DriverKind::from_wire)
        else {
            continue;
        };
        let Some(payload) = value.get("payload") else {
            continue;
        };
        match driver_kind {
            DriverKind::GateRunStart | DriverKind::GateRunScope => {
                if let Some(key) = gate_lifecycle_key(payload) {
                    pending.insert(key, payload.clone());
                }
            }
            DriverKind::GateRunEnd => {
                if let Some(key) = gate_lifecycle_key(payload) {
                    completed_keys.insert(key);
                }
                if let Some(run) = gate_run_from_payload(payload, path) {
                    completed.push(run);
                }
            }
            _ => {}
        }
    }
    for (key, payload) in pending {
        if completed_keys.contains(&key) {
            continue;
        }
        if let Some(run) = incomplete_gate_run_from_payload(&payload, path) {
            completed.push(run);
        }
    }
    completed
}

fn gate_run_lifecycle_events(
    path: &Path,
    run: &GateRun,
    seq_start: u64,
) -> Result<Vec<AgentEvent>, std::io::Error> {
    let mut builder = gate_log_envelope_builder(path, seq_start);
    let phase = gate_phase_wire(run.phase);
    let payload = gate_run_payload(path, run);
    let mut events = vec![
        gate_driver_event(
            &mut builder,
            DriverKind::GateRunStart,
            format!("{phase} gate run started"),
            payload.clone(),
        ),
        gate_driver_event(
            &mut builder,
            DriverKind::GateRunScope,
            format!("{phase} gate run scoped: {}", run.push_range),
            payload.clone(),
        ),
    ];
    if run.covered_hooks.is_empty() {
        events.push(gate_driver_event(
            &mut builder,
            DriverKind::GateRunLane,
            format!("{phase} gate run lane complete"),
            gate_lane_payload(path, run, 0, None),
        ));
    } else {
        for (index, hook) in run.covered_hooks.iter().enumerate() {
            events.push(gate_driver_event(
                &mut builder,
                DriverKind::GateRunLane,
                format!("{phase} gate run lane complete: {}", hook.id),
                gate_lane_payload(path, run, index, Some(hook)),
            ));
        }
    }
    events.push(gate_driver_event(
        &mut builder,
        DriverKind::GateRunEnd,
        format!("{phase} gate run {}", gate_status_wire(run.status)),
        payload,
    ));
    Ok(events)
}

fn gate_driver_event(
    builder: &mut EnvelopeBuilder,
    driver_kind: DriverKind,
    summary: String,
    payload: serde_json::Value,
) -> AgentEvent {
    AgentEvent::DriverEvent {
        envelope: builder.build(),
        driver_kind,
        summary,
        payload,
    }
}

fn gate_run_payload(path: &Path, run: &GateRun) -> serde_json::Value {
    serde_json::json!({
        "run_id": gate_run_id(path, run),
        "phase": gate_phase_wire(run.phase),
        "push_range": &run.push_range,
        "tree_oid": &run.tree_oid,
        "config_digest": &run.config_digest,
        "log_path": path.to_string_lossy(),
        "exit_code": run.exit_code,
        "status": gate_status_wire(run.status),
        "marker": run.marker.as_ref().and_then(marker_to_wire),
        "covered_hooks": &run.covered_hooks,
    })
}

fn gate_lane_payload(
    path: &Path,
    run: &GateRun,
    lane_index: usize,
    hook: Option<&HookCoverage>,
) -> serde_json::Value {
    let mut payload = gate_run_payload(path, run);
    let Some(object) = payload.as_object_mut() else {
        return payload;
    };
    object.insert("lane_index".to_string(), serde_json::json!(lane_index));
    match hook {
        Some(hook) => {
            object.insert("lane_kind".to_string(), serde_json::json!("hook"));
            object.insert(
                "hook".to_string(),
                serde_json::json!({ "id": &hook.id, "entry": &hook.entry }),
            );
        }
        None => {
            object.insert(
                "lane_kind".to_string(),
                serde_json::json!(gate_phase_wire(run.phase)),
            );
        }
    }
    payload
}

fn gate_log_envelope_builder(path: &Path, seq_start: u64) -> EnvelopeBuilder {
    let scope = gate_log_scope(path);
    let clock = SystemClock::new();
    EnvelopeBuilder::with_seq_start(scope, Source::Driver, seq_start, move || {
        clock
            .wall_now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis() as i64)
    })
}

fn gate_log_scope(path: &Path) -> SessionScope {
    if let Ok(contents) = std::fs::read_to_string(path) {
        for line in contents.lines() {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let Some(raw_session) = value.get("session_id").and_then(serde_json::Value::as_str)
            else {
                continue;
            };
            let Ok(session_id) = raw_session.parse::<SessionId>() else {
                continue;
            };
            let raw_bead = value.get("bead_id").and_then(serde_json::Value::as_str);
            if let Some(raw_bead) = raw_bead
                && let Ok(bead_id) = BeadId::new(raw_bead)
            {
                return SessionScope::bead(session_id, bead_id, None, 0);
            }
            return SessionScope::phase(session_id, None);
        }
    }
    SessionScope::phase(gate_log_session_id(path), None)
}

fn gate_log_session_id(path: &Path) -> SessionId {
    let raw = path.to_string_lossy();
    let mut id = String::from("gate");
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() {
            id.push('-');
            id.push(char::from(byte).to_ascii_lowercase());
        }
    }
    SessionId::new(id)
}

fn next_seq_in_log(path: &Path) -> u64 {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return 0;
    };
    let mut max_seq: Option<u64> = None;
    for line in contents.lines() {
        let value = match serde_json::from_str::<serde_json::Value>(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let Some(seq) = value.get("seq").and_then(serde_json::Value::as_u64) else {
            continue;
        };
        max_seq = Some(max_seq.map_or(seq, |max| max.max(seq)));
    }
    max_seq.map_or(0, |seq| seq + 1)
}

fn gate_run_id(path: &Path, run: &GateRun) -> String {
    blake3::hash(
        format!(
            "{}\0{}\0{}\0{}\0{}",
            gate_phase_wire(run.phase),
            run.push_range,
            run.tree_oid,
            run.config_digest,
            path.display(),
        )
        .as_bytes(),
    )
    .to_hex()
    .to_string()
}

fn gate_lifecycle_key(payload: &serde_json::Value) -> Option<String> {
    if let Some(run_id) = payload.get("run_id").and_then(serde_json::Value::as_str)
        && !run_id.is_empty()
    {
        return Some(run_id.to_owned());
    }
    let phase = payload.get("phase")?.as_str()?;
    let push_range = payload.get("push_range")?.as_str()?;
    let tree_oid = payload.get("tree_oid")?.as_str()?;
    let config_digest = payload
        .get("config_digest")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    Some(format!(
        "{phase}\0{push_range}\0{tree_oid}\0{config_digest}"
    ))
}

fn gate_run_from_payload(payload: &serde_json::Value, fallback_path: &Path) -> Option<GateRun> {
    gate_run_from_payload_with_status(payload, fallback_path, None)
}

fn incomplete_gate_run_from_payload(
    payload: &serde_json::Value,
    fallback_path: &Path,
) -> Option<GateRun> {
    gate_run_from_payload_with_status(payload, fallback_path, Some(GateRunStatus::Incomplete))
}

fn gate_run_from_payload_with_status(
    payload: &serde_json::Value,
    fallback_path: &Path,
    status_override: Option<GateRunStatus>,
) -> Option<GateRun> {
    let phase = gate_phase_from_wire(payload.get("phase")?.as_str()?)?;
    let status = match status_override {
        Some(status) => status,
        None => gate_status_from_wire(payload.get("status")?.as_str()?)?,
    };
    let marker = if status == GateRunStatus::Incomplete {
        None
    } else {
        payload
            .get("marker")
            .and_then(serde_json::Value::as_str)
            .and_then(marker_from_str)
    };
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
        exit_code: if status == GateRunStatus::Incomplete {
            None
        } else {
            payload
                .get("exit_code")
                .and_then(serde_json::Value::as_i64)
                .and_then(|code| i32::try_from(code).ok())
        },
        status,
        marker,
        covered_hooks,
    })
}

fn gate_phase_wire(phase: GatePhase) -> &'static str {
    match phase {
        GatePhase::Verify => "verify",
        GatePhase::Review => "review",
    }
}

fn gate_phase_from_wire(phase: &str) -> Option<GatePhase> {
    match phase {
        "verify" => Some(GatePhase::Verify),
        "review" => Some(GatePhase::Review),
        _ => None,
    }
}

fn gate_status_wire(status: GateRunStatus) -> &'static str {
    match status {
        GateRunStatus::Success => "success",
        GateRunStatus::Failed => "failed",
        GateRunStatus::Incomplete => "incomplete",
    }
}

fn gate_status_from_wire(status: &str) -> Option<GateRunStatus> {
    match status {
        "success" => Some(GateRunStatus::Success),
        "failed" => Some(GateRunStatus::Failed),
        "incomplete" => Some(GateRunStatus::Incomplete),
        _ => None,
    }
}

fn marker_to_wire(marker: &ExitSignal) -> Option<String> {
    match marker {
        ExitSignal::Complete => Some("complete".to_string()),
        ExitSignal::Noop => Some("noop".to_string()),
        ExitSignal::Concern { summary } => Some(format!("concern:{summary}")),
        _ => None,
    }
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

pub(crate) fn scope_fingerprint(run: &GateRun) -> String {
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
        let mut evidence = HandoffEvidence::from_runs(vec![verify, review]);
        evidence.molecule_state = MoleculeState::Clean;
        evidence
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
    fn gate_invocations_emit_jsonl_lifecycle_events() {
        let log = NamedTempFile::new().expect("tempfile");
        let run = GateRun::successful_verify(
            "origin/main..HEAD".to_owned(),
            "tree-a".to_owned(),
            "config-a".to_owned(),
            log.path().to_path_buf(),
            vec![hook("pre-push", "loom gate verify --diff @{u}..HEAD")],
        );
        append_gate_run_lifecycle_events(log.path(), &run).expect("write lifecycle events");
        let body = std::fs::read_to_string(log.path()).expect("read log");
        let events = body
            .lines()
            .map(|line| serde_json::from_str::<AgentEvent>(line).expect("agent event json"))
            .collect::<Vec<_>>();
        let kinds = events
            .iter()
            .map(|event| match event {
                AgentEvent::DriverEvent {
                    envelope,
                    driver_kind,
                    ..
                } => {
                    assert_eq!(envelope.source, Source::Driver);
                    driver_kind.as_wire()
                }
                other => panic!("gate log line must be a driver_event: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                "gate_run_start",
                "gate_run_scope",
                "gate_run_lane",
                "gate_run_end",
            ],
        );
        let runs = parse_gate_runs_from_jsonl(log.path());
        assert_eq!(runs.len(), 1);
        assert!(VerifiedScope::from_run(&runs[0]).is_some());
    }

    #[test]
    fn incomplete_gate_event_log_is_not_successful() {
        let log = NamedTempFile::new().expect("tempfile");
        let verify = GateRun::successful_verify(
            "origin/main..HEAD".to_owned(),
            "tree-a".to_owned(),
            "config-a".to_owned(),
            log.path().to_path_buf(),
            vec![hook("pre-push", "loom gate verify --diff @{u}..HEAD")],
        );
        let events =
            gate_run_lifecycle_events(log.path(), &verify, 0).expect("build lifecycle events");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(log.path())
            .expect("open log");
        for event in events {
            if matches!(
                &event,
                AgentEvent::DriverEvent {
                    driver_kind: DriverKind::GateRunEnd,
                    ..
                }
            ) {
                continue;
            }
            serde_json::to_writer(&mut file, &event).expect("write event");
            writeln!(&mut file).expect("write newline");
        }
        drop(file);
        let review = GateRun::successful_review(
            "origin/main..HEAD".to_owned(),
            "tree-a".to_owned(),
            "config-a".to_owned(),
            log.path().to_path_buf(),
            ExitSignal::Complete,
        );
        append_gate_run_lifecycle_events(log.path(), &review).expect("write review events");

        let runs = parse_gate_runs_from_jsonl(log.path());
        let incomplete = runs
            .iter()
            .find(|run| run.phase == GatePhase::Verify)
            .expect("incomplete verify run parsed");
        assert_eq!(incomplete.status, GateRunStatus::Incomplete);
        assert!(!incomplete.is_success());
        assert!(VerifiedScope::from_run(incomplete).is_none());
        let mut evidence = HandoffEvidence::from_runs(runs);
        evidence.molecule_state = MoleculeState::Clean;
        evidence.gate_log_paths = vec![log.path().to_path_buf()];
        match GateSuccess::new(&evidence, 1) {
            Err(GateFail {
                reason: GateFailReason::VerifierFailed,
                ..
            }) => {}
            other => panic!("incomplete gate log must not mint success: {other:?}"),
        }
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
    fn push_gate_evaluates_typed_evidence_and_marker_coverage() {
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
        let mut clean = evidence(log.path());
        clean.gate_log_paths = vec![log.path().to_path_buf()];
        assert!(GateSuccess::new(&clean, 1).is_ok());

        let mut unresolved = clean.clone();
        unresolved.molecule_state = MoleculeState::Unresolved;
        assert!(matches!(
            GateSuccess::new(&unresolved, 1),
            Err(GateFail {
                reason: GateFailReason::MoleculeStateUnresolved,
                ..
            })
        ));

        let mut verify_failed = clean.clone();
        verify_failed.gate_runs[0].status = GateRunStatus::Failed;
        assert!(matches!(
            GateSuccess::new(&verify_failed, 1),
            Err(GateFail {
                reason: GateFailReason::VerifierFailed,
                ..
            })
        ));

        let mut review_concern = clean.clone();
        review_concern.reviewed = None;
        review_concern.review_marker = Some(ExitSignal::Concern {
            summary: "scope concern".to_owned(),
        });
        assert!(matches!(
            GateSuccess::new(&review_concern, 1),
            Err(GateFail {
                reason: GateFailReason::ReviewConcern { .. },
                ..
            })
        ));

        let mut uncovered = clean;
        uncovered.pre_push.as_mut().expect("coverage").hooks.clear();
        assert!(matches!(
            GateSuccess::new(&uncovered, 1),
            Err(GateFail {
                reason: GateFailReason::MarkerCoverageMissing,
                ..
            })
        ));
    }

    #[test]
    fn gate_success_refuses_review_scope_mismatch() {
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

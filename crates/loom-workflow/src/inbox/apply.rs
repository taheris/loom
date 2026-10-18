//! Trusted driver-side apply for accepted tune proposals.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use displaydoc::Display;
use loom_driver::bd::{BdClient, BdError, Bead, UpdateOpts};
use loom_driver::clock::{Clock, SystemClock};
use loom_driver::config::{LoomConfig, LoomConfigError};
use loom_driver::git::{
    ActualPushRange, GitClient, GitError, MergeResult, head_tree_oid_sync, sync_head_commit_sha,
};
use loom_driver::identifier::BeadId;
use loom_driver::logging::phase_log_path;
use loom_gate::{
    GateRun, GateSuccess, HandoffEvidence, MarkerProof, MintError, VerifiedScope,
    append_gate_run_lifecycle_events, parse_gate_runs_from_jsonl,
};
use loom_tune::proposal::State as TuneState;
use serde_json::json;
use thiserror::Error;
use tokio::process::Command;
use tracing::warn;

use super::list::read_manifest_identity;
use crate::r#loop::{REVIEW_EMIT_STDOUT_ENV, REVIEW_PHASE_WHEN_ENV, REVIEW_VERIFIED_LOG_ENV};
use crate::review::{DispatchScope, WalkOutput, WorkspaceFindingValidator};

const TUNE_LABEL: &str = "loom:tune";
const TUNE_STATE_KEY: &str = "loom.tune.state";
const TUNE_BASE_KEY: &str = "loom.tune.base_commit";
const TUNE_BRANCH_KEY: &str = "loom.tune.proposal_branch";
const TUNE_HEAD_KEY: &str = "loom.tune.proposal_head";
const APPLY_FAILURE_KEY: &str = "loom.tune.apply_failure";
const APPLIED_LOG_KEY: &str = "loom.tune.applied_log";
const APPLY_LOOM_BIN_ENV: &str = "LOOM_INBOX_APPLY_LOOM_BIN";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyReport {
    pub proposals: Vec<BeadId>,
    pub push_range: String,
    pub log_path: PathBuf,
}

#[derive(Debug, Display, Error)]
pub enum ApplyError {
    /// config load failed
    Config(#[from] LoomConfigError),
    /// bd CLI failed during tune apply
    Bd(#[from] BdError),
    /// git step failed during tune apply
    Git(#[from] GitError),
    /// tune proposal `{id}` cannot be applied: {reason}
    InvalidProposal { id: BeadId, reason: String },
    /// tune proposal `{id}` is accepted but its canonical/local records are invalid: {reason}
    BlockedProposal { id: BeadId, reason: String },
    /// tune apply requires at least one proposal id
    EmptyBatch,
    /// failed to create tokio runtime for tune apply
    Runtime(#[source] std::io::Error),
    /// tune apply io failed at {path}
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// tune apply batch failed at {kind}: {detail}
    BatchFailed {
        kind: &'static str,
        detail: String,
        log_path: PathBuf,
    },
    /// inbox chat left `.loom/integration` dirty before driver apply: {detail}
    IntegrationDirty { detail: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureKind {
    CherryPickConflict,
    VerifyFailed,
    ReviewFailed,
    PushFailed,
}

impl FailureKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CherryPickConflict => "cherry_pick_conflict",
            Self::VerifyFailed => "verify_failed",
            Self::ReviewFailed => "review_failed",
            Self::PushFailed => "push_failed",
        }
    }
}

struct BatchFailure {
    kind: FailureKind,
    detail: String,
}

impl BatchFailure {
    fn new(kind: FailureKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone)]
struct Proposal {
    id: BeadId,
    repo: PathBuf,
    branch: String,
}

struct AttemptOutcome {
    push_range: String,
}

struct AttemptContext {
    pre_tip: String,
    fetched_branches: Vec<String>,
    log_path: PathBuf,
}

///
/// # Errors
///
/// Returns an error when inbox state, interaction, or persistence fails.
pub fn apply_proposals(
    workspace: &Path,
    proposal_ids: Vec<BeadId>,
) -> Result<ApplyReport, ApplyError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(ApplyError::Runtime)?;
    runtime.block_on(apply_proposals_async(workspace, proposal_ids))
}

///
/// # Errors
///
/// Returns an error when inbox state, interaction, or persistence fails.
pub fn ensure_integration_clean_after_chat(workspace: &Path) -> Result<(), ApplyError> {
    let integration = workspace.join(".loom/integration");
    if !integration.exists() {
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(ApplyError::Runtime)?;
    runtime.block_on(async {
        let git = GitClient::open(workspace)?;
        ensure_integration_clean(&git).await
    })
}

async fn apply_proposals_async(
    workspace: &Path,
    proposal_ids: Vec<BeadId>,
) -> Result<ApplyReport, ApplyError> {
    if proposal_ids.is_empty() {
        return Err(ApplyError::EmptyBatch);
    }
    let config = LoomConfig::load(LoomConfig::resolve_path(workspace))?;
    let git =
        GitClient::open_with_integration_branch(workspace, config.loom.integration_branch.clone())?
            .with_hook_timeout(config.loom.git_hook_timeout());
    ensure_integration_clean(&git).await?;
    let bd = BdClient::<loom_driver::bd::TokioRunner>::new();
    let proposals = validate_proposals(workspace, &bd, &proposal_ids).await?;
    let pre_tip = git.integration_commit_sha().await?.to_string();
    let log_path = apply_log_path(workspace)?;
    let mut ctx = AttemptContext {
        pre_tip: pre_tip.clone(),
        fetched_branches: Vec::new(),
        log_path: log_path.clone(),
    };
    match attempt_batch(&git, &proposals, &mut ctx).await {
        Ok(outcome) => {
            cleanup_branches(&git, &ctx.fetched_branches).await?;
            mark_applied(&bd, &proposals, &log_path).await?;
            write_apply_log(
                &log_path,
                &json!({
                    "status": "applied",
                    "proposals": proposal_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
                    "push_range": outcome.push_range.clone(),
                }),
            )?;
            Ok(ApplyReport {
                proposals: proposal_ids,
                push_range: outcome.push_range,
                log_path,
            })
        }
        Err(failure) => {
            abort_attempt(&git, &ctx).await?;
            mark_apply_failed(&bd, &proposals, failure.kind, &failure.detail, &log_path).await?;
            Err(ApplyError::BatchFailed {
                kind: failure.kind.as_str(),
                detail: failure.detail,
                log_path,
            })
        }
    }
}

async fn validate_proposals(
    workspace: &Path,
    bd: &BdClient,
    ids: &[BeadId],
) -> Result<Vec<Proposal>, ApplyError> {
    let mut proposals = Vec::with_capacity(ids.len());
    for id in ids {
        let bead = bd.show(id).await?;
        match validate_proposal(workspace, bead).await {
            Ok(proposal) => proposals.push(proposal),
            Err(ApplyError::BlockedProposal { id, reason }) => {
                block_invalid_proposal(bd, &id, &reason).await?;
                return Err(ApplyError::BlockedProposal { id, reason });
            }
            Err(source) => return Err(source),
        }
    }
    Ok(proposals)
}

async fn validate_proposal(workspace: &Path, bead: Bead) -> Result<Proposal, ApplyError> {
    if !is_tune_bead(&bead) {
        return invalid(bead.id, "bead is not a tune proposal");
    }
    let state = metadata_string(&bead.metadata, TUNE_STATE_KEY);
    if state.as_deref() != Some("accepted") {
        return invalid(
            bead.id,
            format!(
                "state is `{}`; expected `accepted`",
                state.unwrap_or_else(|| "<missing>".to_string())
            ),
        );
    }
    let base = required_metadata(&bead, TUNE_BASE_KEY)?;
    let branch = required_metadata(&bead, TUNE_BRANCH_KEY)?;
    let head = required_metadata(&bead, TUNE_HEAD_KEY)?;
    let envelope = workspace.join(".loom/tune").join(bead.id.as_str());
    let repo = envelope.join("repo");
    require_path(&bead.id, &repo, "proposal repo")?;
    let manifest_path = envelope.join("manifest.json");
    require_file(&bead.id, &manifest_path, "manifest")?;
    let manifest =
        read_manifest_identity(&manifest_path).ok_or_else(|| ApplyError::BlockedProposal {
            id: bead.id.clone(),
            reason: format!("manifest is corrupt at {}", manifest_path.display()),
        })?;
    if !manifest.agrees_for_apply(&bead, TuneState::Accepted, &base, &branch, &head) {
        return blocked(
            bead.id,
            "manifest disagrees with canonical tune bead metadata",
        );
    }
    let proposal_git = GitClient::open(&repo)?;
    let branch_head = proposal_git
        .resolve_commit_sha(&branch)
        .await
        .map_err(|source| ApplyError::BlockedProposal {
            id: bead.id.clone(),
            reason: format!("proposal branch `{branch}` is not reachable: {source}"),
        })?;
    let expected_head = proposal_git
        .resolve_commit_sha(&head)
        .await
        .map_err(|source| ApplyError::BlockedProposal {
            id: bead.id.clone(),
            reason: format!("proposal head `{head}` is not reachable: {source}"),
        })?;
    if branch_head != expected_head {
        return blocked(
            bead.id,
            format!("proposal branch `{branch}` points to {branch_head}, expected {expected_head}"),
        );
    }
    Ok(Proposal {
        id: bead.id,
        repo,
        branch,
    })
}

async fn block_invalid_proposal(
    bd: &BdClient,
    id: &BeadId,
    reason: &str,
) -> Result<(), ApplyError> {
    bd.update(
        id,
        UpdateOpts {
            status: Some(loom_driver::bd::Status::Blocked),
            add_labels: vec!["loom:blocked".to_owned()],
            notes: Some(format!("tune proposal validation blocked apply: {reason}")),
            set_metadata: vec![(TUNE_STATE_KEY.to_owned(), "blocked".to_owned())],
            ..UpdateOpts::default()
        },
    )
    .await?;
    Ok(())
}

async fn attempt_batch(
    git: &GitClient,
    proposals: &[Proposal],
    ctx: &mut AttemptContext,
) -> Result<AttemptOutcome, BatchFailure> {
    for proposal in proposals {
        let destination = format!("loom/apply/{}", proposal.id);
        git.fetch_branch_from_path(&proposal.repo, &proposal.branch, &destination)
            .await
            .map_err(|source| {
                BatchFailure::new(
                    FailureKind::CherryPickConflict,
                    format!(
                        "fetch {} from {} failed: {source}",
                        proposal.branch,
                        proposal.repo.display()
                    ),
                )
            })?;
        ctx.fetched_branches.push(destination.clone());
        match git.merge_branch(&destination).await.map_err(|source| {
            BatchFailure::new(
                FailureKind::CherryPickConflict,
                format!("merge {} failed: {source}", proposal.id),
            )
        })? {
            MergeResult::Ok => {}
            MergeResult::Conflict { detail, files, .. } => {
                let file_list = files
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                return Err(BatchFailure::new(
                    FailureKind::CherryPickConflict,
                    format!("{detail}\nconflicts: {}", file_list.join(", ")),
                ));
            }
        }
    }
    let actual = git
        .prepare_actual_push_range()
        .await
        .map_err(|source| BatchFailure::new(FailureKind::PushFailed, source.to_string()))?;
    let diff_range = &actual.range;
    let loom_bin = loom_bin().map_err(|source| {
        BatchFailure::new(
            FailureKind::VerifyFailed,
            format!("resolve loom binary: {source}"),
        )
    })?;
    let (verify_run, verified) = run_gate_verify(&loom_bin, git, &actual, &ctx.log_path)
        .await
        .map_err(|detail| BatchFailure::new(FailureKind::VerifyFailed, detail))?;
    let review_run = run_gate_review(
        &loom_bin,
        &git.loom_workspace(),
        &verified,
        &verify_run.log_path,
    )
    .await
    .map_err(|detail| BatchFailure::new(FailureKind::ReviewFailed, detail))?;
    require_current_scope(git, &actual, verified.config_digest())
        .await
        .map_err(|detail| BatchFailure::new(FailureKind::ReviewFailed, detail))?;
    if parse_gate_runs_from_jsonl(&verify_run.log_path) != [verify_run.clone()] {
        return Err(BatchFailure::new(
            FailureKind::VerifyFailed,
            "verification evidence changed during review",
        ));
    }
    let mut evidence = HandoffEvidence::from_runs(vec![verify_run, review_run]);
    evidence.molecule_state = loom_gate::MoleculeState::Clean;
    evidence.push_range = Some(diff_range.clone());
    evidence.tree_oid = Some(actual.tree_oid.to_string());
    let success = GateSuccess::new(&evidence, 1)
        .map_err(|fail| BatchFailure::new(FailureKind::ReviewFailed, fail.reason.to_string()))?;
    match MarkerProof::mint(success, &git.loom_workspace(), &SystemClock::new()) {
        Ok(_) => {}
        Err(error @ MintError::Write { .. }) => warn!(
            ?error,
            "inbox tune apply: marker write failed; pre-push falls through to slow verification",
        ),
        Err(error) => {
            return Err(BatchFailure::new(
                FailureKind::ReviewFailed,
                error.to_string(),
            ));
        }
    }
    git.push_once()
        .await
        .map_err(|source| BatchFailure::new(FailureKind::PushFailed, source.to_string()))?;
    Ok(AttemptOutcome {
        push_range: diff_range.clone(),
    })
}

async fn run_gate_verify(
    loom_bin: &Path,
    git: &GitClient,
    actual: &ActualPushRange,
    log_path: &Path,
) -> Result<(GateRun, VerifiedScope), String> {
    let integration = git.loom_workspace();
    remove_marker(&integration).map_err(|source| source.to_string())?;
    let config_digest =
        pre_commit_config_digest(&integration).map_err(|source| source.to_string())?;
    let hooks = loom_gate::pre_push_hook_coverage_from_config(&integration)
        .map_err(|source| source.to_string())?;
    let output = Command::new(loom_bin)
        .current_dir(&integration)
        .arg("gate")
        .arg("verify")
        .arg("--diff")
        .arg(&actual.range)
        .output()
        .await
        .map_err(|source| source.to_string())?;
    if !output.status.success() {
        return Err(command_detail("loom gate verify", &output));
    }
    require_current_scope(git, actual, &config_digest).await?;
    git.run_pre_push_chain()
        .await
        .map_err(|source| source.to_string())?;
    require_current_scope(git, actual, &config_digest).await?;
    let run = GateRun::successful_verify(
        actual.range.clone(),
        actual.tree_oid.to_string(),
        config_digest,
        log_path.to_path_buf(),
        hooks,
    );
    append_gate_run_lifecycle_events(log_path, &run).map_err(|source| source.to_string())?;
    let runs = parse_gate_runs_from_jsonl(log_path);
    if runs != [run.clone()] {
        return Err("verification produced no unique completed scope evidence".to_owned());
    }
    let verified = VerifiedScope::from_run(&run)
        .ok_or_else(|| "verification produced no completed scope evidence".to_owned())?;
    Ok((run, verified))
}

async fn require_current_scope(
    git: &GitClient,
    actual: &ActualPushRange,
    config_digest: &str,
) -> Result<(), String> {
    let integration = git.loom_workspace();
    ensure_integration_clean(git)
        .await
        .map_err(|source| source.to_string())?;
    let tree = head_tree_oid_sync(&integration).map_err(|source| source.to_string())?;
    let head = sync_head_commit_sha(&integration).map_err(|source| source.to_string())?;
    let digest = pre_commit_config_digest(&integration).map_err(|source| source.to_string())?;
    if tree != actual.tree_oid
        || actual.range.rsplit_once("..").map(|(_, to)| to) != Some(head.as_str())
        || digest != config_digest
    {
        return Err("gate evidence does not match the current push range/tree/config".to_owned());
    }
    Ok(())
}

async fn run_gate_review(
    loom_bin: &Path,
    integration: &Path,
    verified: &VerifiedScope,
    verify_log: &Path,
) -> Result<GateRun, String> {
    let (review_log, when) =
        allocate_review_log(integration).map_err(|source| source.to_string())?;
    let output = Command::new(loom_bin)
        .current_dir(integration)
        .env(REVIEW_VERIFIED_LOG_ENV, verify_log)
        .env(REVIEW_PHASE_WHEN_ENV, when.to_string())
        .env(REVIEW_EMIT_STDOUT_ENV, "1")
        .arg("gate")
        .arg("review")
        .arg("--diff")
        .arg(verified.push_range())
        .output()
        .await
        .map_err(|source| source.to_string())?;
    admit_review_output(&output, integration, verified, &review_log)
}

fn admit_review_output(
    output: &std::process::Output,
    integration: &Path,
    verified: &VerifiedScope,
    review_log: &Path,
) -> Result<GateRun, String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let walk = WalkOutput::from_stdout(
        &stdout,
        DispatchScope::PerBead,
        &WorkspaceFindingValidator::new(integration),
    );
    let config = LoomConfig::load(LoomConfig::resolve_path(integration))
        .map_err(|source| format!("review config: {source:#}"))?;
    let outcome = crate::review::classify_review_phase_with_suppressions(
        &walk,
        output.status.code().unwrap_or(-1),
        &config.suppress,
    );
    if !output.status.success() || !matches!(outcome, crate::review::ReviewOutcome::Complete) {
        return Err(format!(
            "{}\n{outcome:?}",
            command_detail("loom gate review", output)
        ));
    }
    let runs = parse_gate_runs_from_jsonl(review_log);
    let [run] = runs.as_slice() else {
        return Err("review produced no unique completed scope evidence".to_owned());
    };
    let reviewed = loom_gate::ReviewedScope::from_run(run).ok_or_else(|| {
        format!(
            "review produced no completed scope evidence in {}",
            review_log.display()
        )
    })?;
    if run.log_path != review_log
        || reviewed.push_range() != verified.push_range()
        || reviewed.tree_oid() != verified.tree_oid()
        || reviewed.config_digest() != verified.config_digest()
    {
        return Err("review evidence does not match this attempt's verified scope".to_owned());
    }
    Ok(run.clone())
}

fn allocate_review_log(workspace: &Path) -> Result<(PathBuf, u128), std::io::Error> {
    let logs_root = workspace.join(".loom/logs");
    let mut when = SystemClock::new().wall_now();
    loop {
        let path = phase_log_path(&logs_root, "review", when);
        fs::create_dir_all(logs_root.join("review"))?;
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => {
                let millis = when
                    .duration_since(UNIX_EPOCH)
                    .map_err(std::io::Error::other)?
                    .as_millis();
                return Ok((path, millis));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                when += Duration::from_secs(1);
            }
            Err(error) => return Err(error),
        }
    }
}

async fn abort_attempt(git: &GitClient, ctx: &AttemptContext) -> Result<(), ApplyError> {
    git.reset_integration_to(&ctx.pre_tip).await?;
    cleanup_branches(git, &ctx.fetched_branches).await?;
    remove_marker(&git.loom_workspace())?;
    ensure_integration_clean(git).await?;
    Ok(())
}

async fn cleanup_branches(git: &GitClient, branches: &[String]) -> Result<(), ApplyError> {
    git.checkout_integration().await?;
    for branch in branches {
        git.delete_branch(branch).await?;
    }
    Ok(())
}

async fn ensure_integration_clean(git: &GitClient) -> Result<(), ApplyError> {
    let porcelain = git.status_porcelain_at(&git.loom_workspace()).await?;
    if porcelain.trim().is_empty() {
        return Ok(());
    }
    Err(ApplyError::IntegrationDirty { detail: porcelain })
}

async fn mark_applied(
    bd: &BdClient,
    proposals: &[Proposal],
    log_path: &Path,
) -> Result<(), ApplyError> {
    for proposal in proposals {
        bd.update(
            &proposal.id,
            UpdateOpts {
                remove_labels: vec!["loom:blocked".to_string()],
                set_metadata: vec![
                    (TUNE_STATE_KEY.to_string(), "applied".to_string()),
                    (
                        APPLIED_LOG_KEY.to_string(),
                        log_path.to_string_lossy().into_owned(),
                    ),
                ],
                ..UpdateOpts::default()
            },
        )
        .await?;
        bd.close(&proposal.id, Some("applied tune proposal"))
            .await?;
    }
    Ok(())
}

async fn mark_apply_failed(
    bd: &BdClient,
    proposals: &[Proposal],
    kind: FailureKind,
    detail: &str,
    log_path: &Path,
) -> Result<(), ApplyError> {
    let diagnostic = json!({
        "kind": kind.as_str(),
        "detail": detail,
        "log_path": log_path.to_string_lossy(),
    });
    write_apply_log(
        log_path,
        &json!({
            "status": "apply_failed",
            "kind": kind.as_str(),
            "detail": detail,
            "proposals": proposals.iter().map(|proposal| proposal.id.to_string()).collect::<Vec<_>>(),
        }),
    )?;
    for proposal in proposals {
        bd.update(
            &proposal.id,
            UpdateOpts {
                status: Some(loom_driver::bd::Status::Blocked),
                add_labels: vec!["loom:blocked".to_string()],
                notes: Some(format!(
                    "tune apply failed ({kind}): {detail}\nlog: {log}",
                    kind = kind.as_str(),
                    log = log_path.display(),
                )),
                set_metadata: vec![
                    (TUNE_STATE_KEY.to_string(), "apply_failed".to_string()),
                    (APPLY_FAILURE_KEY.to_string(), diagnostic.to_string()),
                ],
                ..UpdateOpts::default()
            },
        )
        .await?;
    }
    Ok(())
}

fn remove_marker(workspace: &Path) -> Result<(), ApplyError> {
    let path = workspace.join(loom_gate::MARKER_PATH);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(ApplyError::Io { path, source }),
    }
}

fn pre_commit_config_digest(workspace: &Path) -> Result<String, std::io::Error> {
    let path = workspace.join(".pre-commit-config.yaml");
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn apply_log_path(workspace: &Path) -> Result<PathBuf, ApplyError> {
    let millis = SystemClock::new()
        .wall_now()
        .duration_since(UNIX_EPOCH)
        .map_err(|source| ApplyError::Io {
            path: workspace.join(".loom/logs/inbox"),
            source: std::io::Error::other(source),
        })?
        .as_millis();
    let dir = workspace.join(".loom/logs/inbox");
    fs::create_dir_all(&dir).map_err(|source| ApplyError::Io {
        path: dir.clone(),
        source,
    })?;
    for suffix in 0..1000_u16 {
        let path = dir.join(format!("apply-{millis}-{suffix}.jsonl"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(ApplyError::Io { path, source }),
        }
    }
    Err(ApplyError::Io {
        path: dir,
        source: std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate tune apply log",
        ),
    })
}

fn write_apply_log(path: &Path, value: &serde_json::Value) -> Result<(), ApplyError> {
    use std::io::Write as _;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| ApplyError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| ApplyError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    serde_json::to_writer(&mut file, value).map_err(|source| ApplyError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::other(source),
    })?;
    writeln!(&mut file).map_err(|source| ApplyError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn loom_bin() -> Result<PathBuf, std::io::Error> {
    match std::env::var_os(APPLY_LOOM_BIN_ENV) {
        Some(path) => Ok(PathBuf::from(path)),
        None => std::env::current_exe(),
    }
}

fn command_detail(name: &str, output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!(
        "{name} exited {status}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        status = output.status,
    )
}

fn is_tune_bead(bead: &Bead) -> bool {
    bead.labels.iter().any(|label| label.as_str() == TUNE_LABEL)
        || bead
            .metadata
            .keys()
            .any(|key| key.starts_with("loom.tune."))
}

fn metadata_string(metadata: &BTreeMap<String, serde_json::Value>, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn required_metadata(bead: &Bead, key: &str) -> Result<String, ApplyError> {
    metadata_string(&bead.metadata, key).ok_or_else(|| ApplyError::BlockedProposal {
        id: bead.id.clone(),
        reason: format!("missing metadata `{key}`"),
    })
}

fn require_path(id: &BeadId, path: &Path, label: &str) -> Result<(), ApplyError> {
    if path.is_dir() {
        return Ok(());
    }
    Err(ApplyError::BlockedProposal {
        id: id.clone(),
        reason: format!("{label} missing at {}", path.display()),
    })
}

fn require_file(id: &BeadId, path: &Path, label: &str) -> Result<(), ApplyError> {
    if path.is_file() {
        return Ok(());
    }
    Err(ApplyError::BlockedProposal {
        id: id.clone(),
        reason: format!("{label} missing at {}", path.display()),
    })
}

fn invalid<T>(id: BeadId, reason: impl Into<String>) -> Result<T, ApplyError> {
    Err(ApplyError::InvalidProposal {
        id,
        reason: reason.into(),
    })
}

fn blocked<T>(id: BeadId, reason: impl Into<String>) -> Result<T, ApplyError> {
    Err(ApplyError::BlockedProposal {
        id,
        reason: reason.into(),
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt as _;

    use loom_gate::HookCoverage;
    use loom_protocol::gate::ExitSignal;

    use super::*;

    #[test]
    fn tune_apply_review_handoff_requires_resolved_paired_messages() {
        let dir = tempfile::tempdir().unwrap();
        let verify_run = GateRun::successful_verify(
            "base..head".into(),
            "tree".into(),
            "config".into(),
            dir.path().join("verify.jsonl"),
            vec![HookCoverage {
                id: "hook".into(),
                entry: "entry".into(),
            }],
        );
        let verified = VerifiedScope::from_run(&verify_run).unwrap();
        let review_log = dir.path().join("review.jsonl");
        let review_run = GateRun::successful_review(
            "base..head".into(),
            "tree".into(),
            "config".into(),
            review_log.clone(),
            ExitSignal::Complete,
        );
        append_gate_run_lifecycle_events(&review_log, &review_run).unwrap();
        for (wire, admitted) in [
            ("LOOM_COMPLETE\n", true),
            ("LOOM_FINDING: {broken}\nLOOM_COMPLETE\n", false),
            (
                "LOOM_CLARIFY: {\"decisions\":[\"lm-choice\"]}\nLOOM_COMPLETE\n",
                false,
            ),
            ("LOOM_CONCERN: {\"summary\":\"missing findings\"}\n", false),
            (
                "LOOM_FINDING: {\"token\":\"spec-coherence-fail\",\"route\":\"deferred\",\"bonds\":[\"unknown\"],\"target\":{\"kind\":\"Criterion\",\"spec\":\"unknown\",\"anchor\":\"missing\"},\"evidence\":\"unresolved\"}\nLOOM_COMPLETE\n",
                false,
            ),
        ] {
            let output = std::process::Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: wire.as_bytes().to_vec(),
                stderr: Vec::new(),
            };
            let result = admit_review_output(&output, dir.path(), &verified, &review_log);
            assert_eq!(result.is_ok(), admitted, "{wire}: {result:?}");
        }
    }

    #[test]
    fn tune_apply_reserves_fresh_evidence_logs() {
        let dir = tempfile::tempdir().unwrap();
        let (first, _) = allocate_review_log(dir.path()).unwrap();
        std::fs::write(&first, "old attempt").unwrap();
        let (second, _) = allocate_review_log(dir.path()).unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read_to_string(first).unwrap(), "old attempt");
        assert_eq!(std::fs::read(second).unwrap(), [] as [u8; 0]);
        assert_ne!(
            apply_log_path(dir.path()).unwrap(),
            apply_log_path(dir.path()).unwrap()
        );
    }
}

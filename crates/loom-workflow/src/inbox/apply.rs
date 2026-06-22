//! Trusted driver-side apply for accepted tune proposals.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use displaydoc::Display;
use loom_driver::bd::{BdClient, BdError, Bead, UpdateOpts};
use loom_driver::clock::{Clock, SystemClock};
use loom_driver::config::{LoomConfig, LoomConfigError};
use loom_driver::git::{GitClient, GitError, MergeResult, head_tree_oid_sync};
use loom_driver::identifier::BeadId;
use loom_gate::{
    GateRun, GateSuccess, HandoffEvidence, HookCoverage, MarkerProof,
    append_gate_run_lifecycle_events,
};
use loom_protocol::gate::{ExitSignal, parse_exit_signal};
use serde_json::json;
use thiserror::Error;
use tokio::process::Command;
use tracing::warn;

const TUNE_LABEL: &str = "loom:tune";
const TUNE_STATE_KEY: &str = "loom.tune.state";
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
    fn as_str(self) -> &'static str {
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
    let bd = BdClient::new();
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
                json!({
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
        proposals.push(validate_proposal(workspace, bead).await?);
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
    let branch = required_metadata(&bead, TUNE_BRANCH_KEY)?;
    let head = required_metadata(&bead, TUNE_HEAD_KEY)?;
    let envelope = workspace.join(".loom/tune").join(bead.id.as_str());
    let repo = envelope.join("repo");
    require_path(&bead.id, &repo, "proposal repo")?;
    require_file(&bead.id, &envelope.join("manifest.json"), "manifest")?;
    let proposal_git = GitClient::open(&repo)?;
    let branch_head = proposal_git
        .resolve_commit_sha(&branch)
        .await
        .map_err(|source| ApplyError::InvalidProposal {
            id: bead.id.clone(),
            reason: format!("proposal branch `{branch}` is not reachable: {source}"),
        })?;
    let expected_head = proposal_git
        .resolve_commit_sha(&head)
        .await
        .map_err(|source| ApplyError::InvalidProposal {
            id: bead.id.clone(),
            reason: format!("proposal head `{head}` is not reachable: {source}"),
        })?;
    if branch_head != expected_head {
        return invalid(
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
    let diff_range = format!("{}..HEAD", ctx.pre_tip);
    let loom_bin = loom_bin().map_err(|source| {
        BatchFailure::new(
            FailureKind::VerifyFailed,
            format!("resolve loom binary: {source}"),
        )
    })?;
    run_gate_verify(&loom_bin, &git.loom_workspace(), &diff_range)
        .await
        .map_err(|detail| BatchFailure::new(FailureKind::VerifyFailed, detail))?;
    run_gate_review(&loom_bin, &git.loom_workspace(), &diff_range)
        .await
        .map_err(|detail| BatchFailure::new(FailureKind::ReviewFailed, detail))?;
    if let Err(detail) = mint_marker(git, &diff_range, &ctx.log_path) {
        warn!(
            %detail,
            "inbox tune apply: marker mint failed; pre-push falls through to slow verification",
        );
    }
    git.push_once()
        .await
        .map_err(|source| BatchFailure::new(FailureKind::PushFailed, source.to_string()))?;
    Ok(AttemptOutcome {
        push_range: diff_range,
    })
}

async fn run_gate_verify(
    loom_bin: &Path,
    integration: &Path,
    diff_range: &str,
) -> Result<(), String> {
    let output = Command::new(loom_bin)
        .current_dir(integration)
        .arg("gate")
        .arg("verify")
        .arg("--diff")
        .arg(diff_range)
        .output()
        .await
        .map_err(|source| source.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    Err(command_detail("loom gate verify", &output))
}

async fn run_gate_review(
    loom_bin: &Path,
    integration: &Path,
    diff_range: &str,
) -> Result<(), String> {
    let output = Command::new(loom_bin)
        .current_dir(integration)
        .arg("gate")
        .arg("review")
        .arg("--diff")
        .arg(diff_range)
        .output()
        .await
        .map_err(|source| source.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let marker = parse_exit_signal(&stdout);
    if output.status.success() && matches!(marker, Some(ExitSignal::Complete)) {
        return Ok(());
    }
    Err(command_detail("loom gate review", &output))
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
        json!({
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
                status: Some("blocked".to_string()),
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

fn mint_marker(git: &GitClient, diff_range: &str, log_path: &Path) -> Result<(), String> {
    let marker_workspace = git.loom_workspace();
    let tree = head_tree_oid_sync(&marker_workspace)
        .map_err(|source| source.to_string())?
        .to_string();
    let config_digest =
        pre_commit_config_digest(&marker_workspace).map_err(|source| source.to_string())?;
    append_gate_run_lifecycle_events(
        log_path,
        &GateRun::successful_verify(
            diff_range.to_string(),
            tree.clone(),
            config_digest.clone(),
            log_path.to_path_buf(),
            pre_push_hook_coverage(),
        ),
    )
    .map_err(|source| source.to_string())?;
    append_gate_run_lifecycle_events(
        log_path,
        &GateRun::successful_review(
            diff_range.to_string(),
            tree,
            config_digest,
            log_path.to_path_buf(),
            ExitSignal::Complete,
        ),
    )
    .map_err(|source| source.to_string())?;
    let evidence = HandoffEvidence::from_runs(loom_gate::parse_gate_runs_from_jsonl(log_path));
    let success = GateSuccess::new(&evidence, 1).map_err(|fail| format!("{:?}", fail.reason))?;
    MarkerProof::mint(success, &marker_workspace, &SystemClock::new())
        .map_err(|source| source.to_string())?;
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

fn pre_push_hook_coverage() -> Vec<HookCoverage> {
    [
        ("nix-flake-check", "skip-if-missing nix -- nix flake check"),
        (
            "cargo-clippy",
            "cargo clippy --workspace --all-targets -- -D warnings",
        ),
        (
            "loom-gate-verify-diff",
            "loom gate verify --diff @{u}..HEAD",
        ),
        ("container-smoke", "skip-if-missing nix -- nix run .#test"),
    ]
    .into_iter()
    .map(|(id, entry)| HookCoverage {
        id: id.to_owned(),
        entry: entry.to_owned(),
    })
    .collect()
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
    Ok(dir.join(format!("apply-{millis}.jsonl")))
}

fn write_apply_log(path: &Path, value: serde_json::Value) -> Result<(), ApplyError> {
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
    serde_json::to_writer(&mut file, &value).map_err(|source| ApplyError::Io {
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
    metadata_string(&bead.metadata, key).ok_or_else(|| ApplyError::InvalidProposal {
        id: bead.id.clone(),
        reason: format!("missing metadata `{key}`"),
    })
}

fn require_path(id: &BeadId, path: &Path, label: &str) -> Result<(), ApplyError> {
    if path.is_dir() {
        return Ok(());
    }
    Err(ApplyError::InvalidProposal {
        id: id.clone(),
        reason: format!("{label} missing at {}", path.display()),
    })
}

fn require_file(id: &BeadId, path: &Path, label: &str) -> Result<(), ApplyError> {
    if path.is_file() {
        return Ok(());
    }
    Err(ApplyError::InvalidProposal {
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

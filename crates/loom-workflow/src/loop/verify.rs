//! Two-pass commit-signature verification shared by the sequential
//! (`production`) and parallel (`parallel`) per-bead integration paths.
//!
//! `specs/harness.md` § Verdict Gate mandates that **every** landed bead
//! verify commit signatures in two passes — pass 1 over the fetched worker
//! commits, pass 2 over the rebased/rewritten commits — before the
//! ff-merge moves the integration branch. Both the sequential `run_bead`
//! and the parallel `merge_back_one` integration steps drive the same
//! [`GitClient::verify_commit_range`] check, emit the same
//! `signature-verification-failed` driver event, and delete the transient
//! `loom/<id>` ref on rejection. Factoring the pass into one helper keeps
//! the two paths from drifting (the parallel path previously skipped
//! verification entirely).

use loom_driver::git::{GitClient, SignatureCheck};
use loom_driver::identifier::BeadId;
use loom_events::DriverKind;
use tracing::warn;

use super::driver_emit::BeadEmit;
use super::error::LoopError;

/// Which side of the per-bead integration a signature-verification pass
/// covers. The two variants are the closed, codebase-owned set the
/// verdict gate enumerates (`specs/harness.md` § Verdict Gate phases 2 &
/// 4); modelling them as an enum rather than a `&str` keeps a typo'd side
/// label from silently corrupting the operator-facing routing detail
/// (RS-17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyPass {
    /// Pass 1 — the fetched worker commits. A rejected signature here
    /// means the wrapix container's signing setup is broken; the bead
    /// routes to `loom:blocked` (worker-side).
    Worker,
    /// Pass 2 — the rebased commits the integration rewrite produced. A
    /// rejected signature here means loom's own (driver-side) signing
    /// setup is broken; the bead routes to `loom:blocked` (driver-side).
    Driver,
}

impl VerifyPass {
    /// Operator-facing side label that rides into the
    /// `signature-verification-failed ({side}-side)` reason string, the
    /// `warn!` field, and the `side` field of the
    /// `SignatureVerificationFailed` JSONL payload.
    pub fn side(self) -> &'static str {
        match self {
            VerifyPass::Worker => "worker",
            VerifyPass::Driver => "driver",
        }
    }
}

/// Run one `git verify-commit` pass over `range` in the loom workspace.
///
/// Returns `Ok(None)` when the pass verified (or was skipped because no
/// signing key resolved). Returns `Ok(Some(reason))` on a rejected
/// signature — the operator-facing reason string. On that path the
/// `SignatureVerificationFailed` driver event is emitted (when `emit` is
/// `Some`), a `warn!` is logged, and the transient `branch` is deleted
/// unconditionally so a later dispatch's fetch starts clean. The caller
/// wraps the reason into its own outcome type
/// ([`AgentOutcome::SignatureVerificationFailed`] for the sequential
/// path, [`BatchResult::AgentBlocked`] for the parallel path).
///
/// [`AgentOutcome::SignatureVerificationFailed`]: super::outcome::AgentOutcome::SignatureVerificationFailed
/// [`BatchResult::AgentBlocked`]: super::parallel::BatchResult::AgentBlocked
pub async fn verify_pass(
    git: &GitClient,
    emit: Option<&mut BeadEmit>,
    bead: &BeadId,
    branch: &str,
    range: &str,
    pass: VerifyPass,
) -> Result<Option<String>, LoopError> {
    match git.verify_commit_range(range).await? {
        SignatureCheck::Skipped | SignatureCheck::Verified => Ok(None),
        SignatureCheck::Failed { commit, detail } => {
            let side = pass.side();
            let reason = format!(
                "signature-verification-failed ({side}-side): \
                 git verify-commit rejected {commit} in range {range} — {detail}",
            );
            warn!(
                bead = %bead,
                branch,
                side,
                commit = %commit,
                "signature verification failed — routing to loom:blocked",
            );
            if let Some(emit) = emit {
                emit.emit(
                    DriverKind::SignatureVerificationFailed,
                    &format!("signature verification failed ({side}-side): {commit}"),
                    serde_json::json!({
                        "bead_id": bead.to_string(),
                        "branch": branch,
                        "side": side,
                        "commit": commit,
                        "range": range,
                        "detail": detail,
                    }),
                );
            }
            // `loom/<id>` ref deleted unconditionally on this exit path. A
            // successful rebase (pass 2) leaves the workspace on the rewritten
            // bead branch, so return to the integration branch first — git
            // refuses to delete the checked-out branch, and this restores the
            // untouched integration line as the checked-out state. On pass 1
            // the workspace is already on the integration branch (no-op).
            git.checkout_integration().await?;
            git.delete_branch(branch).await?;
            Ok(Some(reason))
        }
    }
}

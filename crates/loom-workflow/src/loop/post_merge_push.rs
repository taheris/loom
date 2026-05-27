//! Driver-side per-bead push of the freshly-merged driver branch.
//!
//! Per-bead workspaces clone from the driver workdir, so a bead's
//! `git push` inside its container only reaches the local parent — not
//! GitHub. After `merge_branch` folds the bead's work into the driver
//! branch the driver itself must push `main` to the real `origin` and
//! sync the beads remote, otherwise per-bead state accumulates locally
//! and only the molecule-end review-phase push ever publishes it.
//!
//! Push failure preserves the bead workspace (matching the
//! merge-conflict path's semantics) so a transient network blip is
//! recoverable on the next iteration rather than letting the local /
//! remote divergence pile up silently.

use std::path::{Path, PathBuf};

use loom_driver::git::GitClient;
use tokio::process::Command;

use super::error::LoopError;

/// Program invoked to sync the beads remote after `git push`. Defaults to
/// `beads-push` on `PATH`; tests override with a stub script via
/// `with_beads_push_program`.
pub const DEFAULT_BEADS_PUSH_PROGRAM: &str = "beads-push";

/// Push `main` to GitHub then sync the beads remote, in order. The push
/// is run from `workspace` so the configured `origin` is the GitHub remote
/// the driver workdir owns (not the local parent the bead workspace
/// cloned from).
///
/// `beads_push_program` selects the binary used for the beads sync —
/// production callers pass `DEFAULT_BEADS_PUSH_PROGRAM`; tests pass a
/// stub script path.
pub async fn push_merged_main_then_beads(
    git: &GitClient,
    workspace: &Path,
    beads_push_program: &Path,
) -> Result<(), LoopError> {
    git.push().await?;
    run_beads_push(beads_push_program, workspace).await
}

/// Spawn `<beads_push_program>` with `current_dir = workspace`. Used by
/// the per-bead push gate after `git push` succeeds; the program defaults
/// to `beads-push` on `PATH` and tests override it to keep CI from
/// shelling out to the real beads remote.
pub async fn run_beads_push(program: &Path, workspace: &Path) -> Result<(), LoopError> {
    let mut cmd = Command::new(program);
    cmd.current_dir(workspace);
    let output = cmd.output().await?;
    if !output.status.success() {
        return Err(LoopError::BeadsPushFailed(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    Ok(())
}

/// Convert the default program name into a [`PathBuf`] so callers can
/// hand it to [`push_merged_main_then_beads`] without re-stringifying.
pub fn default_beads_push_program() -> PathBuf {
    PathBuf::from(DEFAULT_BEADS_PUSH_PROGRAM)
}

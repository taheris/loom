//! Typed git surface backed by a `gix` + `git` CLI hybrid.
//!
//! `GitClient` is the only module in the workspace permitted to import `gix`
//! or invoke the `git` binary. Read-only operations (status, diff, refs,
//! commit graph, worktree iteration) flow through `gix`; worktree mutation
//! (`worktree add -b`, `worktree remove`, `worktree prune`) and merge-back
//! shell out to `git` because the corresponding `gix` paths are still
//! unchecked in `crate-status.md`.
//!
//! See the *Worktree Parallelism / Git operations* section of
//! `specs/harness.md` for the operation/backend split rationale.

mod client;
mod error;
mod signing;

pub use client::{
    CreatedWorktree, GitClient, MergeResult, StatusEntry, StatusKind, WorktreeInfo,
    bare_origin_path, clone_loom_workspace, head_tree_oid_sync, init_test_repo,
    init_test_repo_with_integration, init_test_repo_with_integration_branch, read_origin_url,
    status_porcelain_sync, sync_head_commit_sha,
};
pub use error::GitError;
pub use loom_protocol::oid::{GitOid, ParseGitOidError};
pub use signing::{
    WRAPIX_DEPLOY_KEY_ENV, WRAPIX_SIGNING_KEY_ENV, enable_rerere, resolve_deploy_key,
    resolve_signing_key, write_signing_config, write_signing_config_for_container,
};

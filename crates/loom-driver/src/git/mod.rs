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
mod hooks;
mod signing;

pub use client::{
    ActualPushRange, CreatedWorktree, FastForwardOutcome, GitClient, MergeResult, RebaseOutcome,
    SignatureCheck, StatusEntry, StatusKind, WorktreeInfo, bare_origin_path, clone_loom_workspace,
    commit_all_in, fast_forward_loom_workspace_to_origin, head_tree_oid_sync, init_test_repo,
    init_test_repo_with_integration, init_test_repo_with_integration_branch, read_origin_url,
    status_porcelain_sync, sync_head_commit_sha, sync_rev_parse,
};
pub use error::GitError;
pub use hooks::{
    WRIX_PREK_HOOKS_ENV, resolve_prek_hooks_path, resolve_prek_hooks_path_for_workspace,
    validate_hooks_config, write_hooks_config,
};
// `GitOid` lives in the public-contract leaf `loom-protocol::oid` so the
// typed retry-context surface in `loom-templates`
// (`PreviousFailure::IntegrationConflict { new_base_sha: GitOid }`) can
// name it without a `loom-driver` dependency. Re-exported here so existing
// `loom_driver::git::GitOid` callers stay unchanged.
pub use loom_protocol::oid::{GitOid, ParseGitOidError};
pub use signing::{
    WRIX_DEPLOY_KEY_ENV, WRIX_SIGNING_KEY_ENV, enable_rerere, reconcile_signing_config,
    resolve_deploy_key, resolve_signing_key, write_signing_config,
};

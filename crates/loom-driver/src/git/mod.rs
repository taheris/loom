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

pub use client::{
    CreatedWorktree, GitClient, MergeResult, StatusEntry, StatusKind, WorktreeInfo,
    bare_origin_path, clone_loom_workspace, init_test_repo, read_origin_url,
};
pub use error::GitError;

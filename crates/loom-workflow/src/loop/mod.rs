//! `loom loop` — per-bead execution loop.
//!
//! Implements the sequential (`--parallel 1`) shape of the loop command per
//! `specs/harness.md` "Command set" / "Process Architecture" / "Loop UX
//! & Logging". The loop:
//!
//! 1. resolves the per-bead profile from the bead's `profile:X` label (or a
//!    `--profile` override) and builds a typed [`SpawnConfig`](
//!    loom_driver::agent::SpawnConfig);
//! 2. renders the [`LoopContext`](loom_templates::run::LoopContext) prompt with
//!    the bead's id/title/description, threading the previous-failure body
//!    (truncated to 4000 chars) on retries;
//! 3. spawns `wrapix spawn --spawn-config <file> --stdio` via an
//!    [`AgentBackend`](loom_driver::agent::AgentBackend) and tees the
//!    [`AgentEvent`](loom_driver::agent::AgentEvent) stream into the terminal
//!    renderer + per-bead JSONL log;
//! 4. on agent failure retries with `previous_failure` injected up to
//!    `max_retries` (default 2), then applies the `loom:clarify` label;
//! 5. on bead success observes the agent's own `bd close` — the driver
//!    never closes a dispatched bead (closure is the agent's job per the
//!    verdict-gate `bd-closed` observable);
//! 6. on molecule completion (no more ready beads) execs the FR1
//!    handoff: `loom gate verify --diff <molecule.base_commit>..HEAD`
//!    then `loom gate review --diff <molecule.base_commit>..HEAD` —
//!    continuous mode only; scope is the molecule's own diff (not
//!    `--tree`). The outer loop then re-polls `bd ready` and iterates
//!    on any newly-ready fix-up beads, bounded by
//!    `[loop] max_iterations`.
//!
//! `--parallel N > 1` (worktree parallelism) lives in [`parallel`]. The
//! sequential and parallel paths share the [`AgentOutcome`] / retry vocabulary
//! but split on dispatch: sequential spawns one container on the driver
//! branch; parallel spawns N containers in disjoint worktrees and merges
//! finished branches sequentially.

mod context;
mod error;
mod outcome;
mod parallel;
mod parallelism;
mod production;
mod profile;
mod retry;
mod runner;
mod spawn;
mod tree_clean;

pub use context::{LoopContextInputs, build_loop_context, render_loop_prompt};
pub use error::LoopError;
pub use outcome::{AgentOutcome, BeadResult, SessionResult};
pub use parallel::{
    BatchOutcome, BatchResult, BatchSlot, WorktreeBead, create_worktrees, merge_back,
    run_concurrent_spawns, run_parallel_batch,
};
pub use parallelism::{Parallelism, ParallelismError};
pub use production::{
    ProductionAgentLoopController, format_unknown_profile_error, list_open_for_spec,
};
pub use profile::{DEFAULT_PROFILE, resolve_profile, resolve_profile_image};
pub use retry::{RetryDecision, RetryPolicy};
pub use runner::{
    AgentLoopController, INFRA_PREFLIGHT_CAUSE, INFRA_REPEATED_CAUSE, LoopMode, LoopSummary,
    UNKNOWN_PROFILE_CAUSE, run_loop,
};
pub use spawn::build_spawn_config_from_manifest;
pub use tree_clean::{TREE_NOT_CLEAN_CAP, dirty_paths_from_porcelain};

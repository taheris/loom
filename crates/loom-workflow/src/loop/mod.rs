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
//! 3. spawns `wrix spawn --spawn-config <file> --stdio` via an
//!    [`AgentBackend`](loom_driver::agent::AgentBackend) and tees the
//!    [`AgentEvent`](loom_driver::agent::AgentEvent) stream into the terminal
//!    renderer + per-bead JSONL log;
//! 4. on agent failure retries with `previous_failure` injected up to
//!    `max_retries` (default 2), then applies the `loom:clarify` label;
//! 5. on bead success observes the agent's own `bd close` — the driver
//!    never closes a dispatched bead (closure is the agent's job per the
//!    verdict-gate `bd-closed` observable);
//! 6. after ready work drains, execs the FR1 handoff:
//!    `loom gate verify --diff <molecule.base_commit>..HEAD` then
//!    `loom gate review --diff <molecule.base_commit>..HEAD`; scope is
//!    the molecule's own diff (not `--tree`). The outer loop then re-polls
//!    `bd ready` and iterates on any newly-ready fix-up beads, bounded by
//!    `[loop] max_iterations`.
//!
//! `--parallel N > 1` (worktree parallelism) lives in [`parallel`]. The
//! sequential and parallel paths share the [`AgentOutcome`] / retry vocabulary
//! but split on dispatch: sequential spawns one container on the driver
//! branch; parallel spawns N containers in disjoint worktrees and merges
//! finished branches sequentially.

mod context;
mod driver_emit;
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
mod verify;

pub use context::{LoopContextInputs, build_loop_context, render_loop_prompt};
pub use error::LoopError;
pub use loom_gate::{
    GateFail, GateFailReason, GateOutcome, GateSuccess, HandoffEvidence, LoopOutcome, NoGateReason,
};
pub use outcome::{AgentOutcome, BeadResult, InfraDiagnostic, SessionResult};
pub use parallel::{
    BatchInfraFailure, BatchOutcome, BatchResult, BatchSlot, WorktreeBead, create_worktrees,
    merge_back, merge_back_with_logs, run_concurrent_spawns, run_parallel_batch,
    run_parallel_batch_with_logs,
};
pub use parallelism::{Parallelism, ParallelismError};
pub use production::{
    ProductionAgentLoopController, REVIEW_EMIT_STDOUT_ENV, REVIEW_PHASE_WHEN_ENV,
    REVIEW_SPEC_LABEL_ENV, classify_session, format_unknown_profile_error,
    format_unknown_runtime_for_profile_error, list_open_for_spec,
};
pub use profile::{DEFAULT_PROFILE, resolve_profile, resolve_profile_image};
pub use retry::{RetryDecision, RetryPolicy};
pub use runner::{
    AgentLoopController, CONFLICT_RETRY_LABEL, INFRA_INTERRUPTED_CAUSE, INFRA_PREFLIGHT_CAUSE,
    INVALID_SPAWN_CONFIG_CAUSE, InfraRetryPolicy, MISSING_AGENT_BINARY_CAUSE,
    UNKNOWN_PROFILE_CAUSE, UNKNOWN_RUNTIME_FOR_PROFILE_CAUSE, WORKSPACE_RECOVERY_FAILED_CAUSE,
    run_loop, run_loop_with_infra_policy,
};
pub use spawn::{build_spawn_config_from_manifest, dolt_socket_mount, sccache_mount};
pub use tree_clean::{TREE_NOT_CLEAN_CAP, dirty_paths_from_porcelain};
pub use verify::{VerifyPass, verify_pass};

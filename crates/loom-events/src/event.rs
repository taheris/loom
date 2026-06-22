use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::identifier::{BeadId, MoleculeId, ProfileName, SpecLabel, ToolCallId};

/// Driver-side event subtype carried on [`AgentEvent::DriverEvent`].
///
/// On the wire `driver_kind` is a snake_case string for forward
/// compatibility — older consumers see unknown kinds as
/// [`DriverKind::Other`] rather than failing deserialization. Producers
/// pass the enum, so they cannot typo a known kind; consumers match
/// exhaustively over the spec-enumerated arms with a single catch-all
/// `Other` for additive growth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverKind {
    VerdictGate,
    RetryDispatch,
    PushGateWalk,
    PushGateRefuse,
    PushGateClean,
    ContainerSpawn,
    ContainerOom,
    InfraFailure,
    /// Per-call token accounting emitted by `llm` after every
    /// `complete*` so SaaS billing pipelines tail the live event stream
    /// instead of re-parsing provider responses.
    TokenUsage,
    /// Direct tool output was written to the per-session offload sink.
    /// Payload fields: `tool`, `total_bytes`.
    Offload,
    /// Observability signal emitted by `llm`'s
    /// `DuplicateResultObserver` when an agent's tool result payload
    /// byte-equals an earlier result in the same session. Payload
    /// fields: `original_call_id`, `repeated_call_id`, `bytes_wasted`.
    DuplicateToolResult,
    /// Observability signal emitted by `llm`'s `DoomLoopObserver`
    /// when 3 of the last 5 entries for a given `(CallKey, ResultHash)`
    /// window are identical (stage 1) or `stage_2_after_stage_1` more
    /// identical pairs follow stage 1 (stage 2). Payload fields:
    /// `stage`, `tool`, `params`, `call_id`.
    DoomLoopTripped,
    /// Emitted by the review phase's push-gate `Clean` branch after the
    /// code + beads push succeed: when an epic's children are all
    /// closed and the molecule's review passed, the driver calls `bd
    /// close <epic-id>` and emits one event per closed epic. Payload
    /// field: `epic_id`. Nested epics close inside-out in one pass; the
    /// driver emits one event per close, ordered child-before-parent.
    EpicAutoClosed,
    /// Bead branch pushed from the per-bead clone back to the driver
    /// origin so the run-phase merge-back can fold it into `main`.
    /// Payload fields: `bead_id`, `branch`, `worktree_path`.
    BeadBranchPushed,
    /// Rebase + ff-only merge of a bead branch into the driver branch
    /// succeeded. Payload fields: `bead_id`, `branch`, `main_sha`.
    MergeOk,
    /// Rebase aborted: the bead's branch conflicted with the driver
    /// branch and was preserved for human resolution. Routed to
    /// `AgentOutcome::Blocked`. Payload fields: `bead_id`, `branch`,
    /// `worktree_path`.
    MergeConflict,
    /// Driver-side rebase of a bead branch onto the integration branch
    /// conflicted textually; the rebase was aborted and the bead routed
    /// to the single integration-conflict retry (or to `loom:clarify` on
    /// the second conflict). Payload fields: `bead_id`, `branch`,
    /// `worktree_path`, `detail`, `new_base_sha`, `files`.
    IntegrationConflict,
    /// `git verify-commit` rejected a fetched (pass 1, worker-side) or
    /// rebased (pass 2, driver-side) commit during the per-bead
    /// integration step. Routed to `loom:blocked` with cause
    /// `signature-verification-failed`. Payload fields: `bead_id`,
    /// `branch`, `side`, `commit`, `range`, `detail`.
    SignatureVerificationFailed,
    /// `remove_worktree` + `delete_branch` both succeeded after a clean
    /// merge. Payload fields: `bead_id`, `branch`, `worktree_path`.
    WorktreeCleanupOk,
    /// `git status --porcelain` against the per-bead workspace was not
    /// empty after the agent emitted `LOOM_COMPLETE`. Routed to a
    /// next-retry `TreeNotClean` stash. Payload fields: `bead_id`,
    /// `dirty_paths` (capped list of paths).
    TreeNotClean,
    /// Gate invocation accepted and lifecycle logging began.
    GateRunStart,
    /// Gate invocation scope resolved to concrete inputs.
    GateRunScope,
    /// Gate invocation reported progress for one execution lane.
    GateRunLane,
    /// Gate invocation finished and serialized its `GateRun` summary.
    GateRunEnd,
    /// Gate invocation was skipped before running verifier work.
    GateRunSkipped,
    /// Forward-compat fallback: any wire `driver_kind` string that does
    /// not match a known variant lands here. Known variants never fall
    /// through.
    Other(String),
}

impl DriverKind {
    /// Snake_case wire representation. `Other` round-trips the carried
    /// string verbatim so unknown producers stay legible in JSONL logs.
    pub fn as_wire(&self) -> &str {
        match self {
            DriverKind::VerdictGate => "verdict_gate",
            DriverKind::RetryDispatch => "retry_dispatch",
            DriverKind::PushGateWalk => "push_gate_walk",
            DriverKind::PushGateRefuse => "push_gate_refuse",
            DriverKind::PushGateClean => "push_gate_clean",
            DriverKind::ContainerSpawn => "container_spawn",
            DriverKind::ContainerOom => "container_oom",
            DriverKind::InfraFailure => "infra_failure",
            DriverKind::TokenUsage => "token_usage",
            DriverKind::Offload => "offload",
            DriverKind::DuplicateToolResult => "duplicate_tool_result",
            DriverKind::DoomLoopTripped => "doom_loop_tripped",
            DriverKind::EpicAutoClosed => "epic_auto_closed",
            DriverKind::BeadBranchPushed => "bead_branch_pushed",
            DriverKind::MergeOk => "merge_ok",
            DriverKind::MergeConflict => "merge_conflict",
            DriverKind::IntegrationConflict => "integration_conflict",
            DriverKind::SignatureVerificationFailed => "signature_verification_failed",
            DriverKind::WorktreeCleanupOk => "worktree_cleanup_ok",
            DriverKind::TreeNotClean => "tree_not_clean",
            DriverKind::GateRunStart => "gate_run_start",
            DriverKind::GateRunScope => "gate_run_scope",
            DriverKind::GateRunLane => "gate_run_lane",
            DriverKind::GateRunEnd => "gate_run_end",
            DriverKind::GateRunSkipped => "gate_run_skipped",
            DriverKind::Other(s) => s.as_str(),
        }
    }

    /// Parse a wire string into the enum. Known variants take precedence
    /// over `Other`; unknown strings land in `Other`.
    pub fn from_wire(s: &str) -> Self {
        match s {
            "verdict_gate" => DriverKind::VerdictGate,
            "retry_dispatch" => DriverKind::RetryDispatch,
            "push_gate_walk" => DriverKind::PushGateWalk,
            "push_gate_refuse" => DriverKind::PushGateRefuse,
            "push_gate_clean" => DriverKind::PushGateClean,
            "container_spawn" => DriverKind::ContainerSpawn,
            "container_oom" => DriverKind::ContainerOom,
            "infra_failure" => DriverKind::InfraFailure,
            "token_usage" => DriverKind::TokenUsage,
            "offload" => DriverKind::Offload,
            "duplicate_tool_result" => DriverKind::DuplicateToolResult,
            "doom_loop_tripped" => DriverKind::DoomLoopTripped,
            "epic_auto_closed" => DriverKind::EpicAutoClosed,
            "bead_branch_pushed" => DriverKind::BeadBranchPushed,
            "merge_ok" => DriverKind::MergeOk,
            "merge_conflict" => DriverKind::MergeConflict,
            "integration_conflict" => DriverKind::IntegrationConflict,
            "signature_verification_failed" => DriverKind::SignatureVerificationFailed,
            "worktree_cleanup_ok" => DriverKind::WorktreeCleanupOk,
            "tree_not_clean" => DriverKind::TreeNotClean,
            "gate_run_start" => DriverKind::GateRunStart,
            "gate_run_scope" => DriverKind::GateRunScope,
            "gate_run_lane" => DriverKind::GateRunLane,
            "gate_run_end" => DriverKind::GateRunEnd,
            "gate_run_skipped" => DriverKind::GateRunSkipped,
            other => DriverKind::Other(other.to_string()),
        }
    }
}

impl std::fmt::Display for DriverKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire())
    }
}

impl Serialize for DriverKind {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for DriverKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // `Cow<str>` borrows zero-copy from a `&str`-backed deserializer
        // (the JSONL streaming case) and owns the bytes when the
        // deserializer can only supply a `String` (e.g.
        // `serde_json::from_value`). Either path lands in `from_wire`.
        let s = std::borrow::Cow::<'_, str>::deserialize(d)?;
        Ok(DriverKind::from_wire(&s))
    }
}

/// Common envelope every [`AgentEvent`] carries. Serialized flat at the
/// top level via `#[serde(flatten)]` — consumers see one discriminator
/// (`kind`) plus the envelope fields plus variant-specific payload, all
/// at the same nesting level. No nested `message_update { delta: { ... } }`
/// wrappers — every consumer dispatches with one `match` (Rust) or one
/// `switch (event.kind)` (TypeScript).
///
/// `seq` is monotonic per `(bead_id, spawn)` pair: the producer side
/// (parser or driver-event emitter) maintains a per-session counter and
/// stamps each emitted event with the next value. Replay code groups
/// events into runs by sorting on `(bead_id, seq)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub bead_id: BeadId,
    /// Optional because driver-emitted events tied to a molecule that has
    /// no bead-level scope may omit it.
    pub molecule_id: Option<MoleculeId>,
    /// Iteration counter for the bead's molecule — bumped each time
    /// `loom review` enters another `loom loop` round.
    pub iteration: u32,
    pub source: Source,
    /// Unix-epoch milliseconds when the event was produced.
    pub ts_ms: i64,
    /// Monotonic per-bead-spawn counter. `0` at session start.
    pub seq: u64,
}

/// Where the event originated. Driver-side events (verdict gate, push
/// gate, infra failures) carry `Driver`; agent-side events (tool calls,
/// message deltas, etc.) carry `Agent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Agent,
    Driver,
}

/// Backend-neutral event flowing from a running agent up to the workflow
/// engine. Both pi and claude line parsers normalize their wire messages
/// into this enum — once an `AgentEvent` flows downstream no code knows
/// which backend produced it.
///
/// `Serialize` is derived so the on-disk JSONL log file is the same
/// event stream the terminal renderer consumes (see [`crate::lib`]
/// consumers). The matching `Deserialize` impl lets `loom logs` replay
/// its own JSONL output through the same enum it wrote. Each variant
/// is a struct-style `#[serde(flatten)]`-onto-envelope, so the wire
/// shape is flat and every consumer dispatches on `kind`. Unknown
/// `kind` values fail deserialization — `loom logs` is the only
/// intended consumer today, and a quietly-dropped variant is worse than
/// a loud failure when the log format drifts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    /// Session start — the first event in any agent log. Carries the
    /// per-spawn metadata the renderer/log-replayer needs to label the
    /// stream. `schema_version` lets readers reject incompatible wire
    /// shapes.
    AgentStart {
        #[serde(flatten)]
        envelope: EventEnvelope,
        /// Wire-format schema version. Adding new variants or fields is
        /// minor (consumers ignore unknowns). Renaming / removing /
        /// repurposing fields requires bumping this.
        schema_version: u32,
        /// Bead title, mirrored at session start for renderer headers.
        title: String,
        /// Profile (`base`, `rust`, …) the bead is running under.
        profile: ProfileName,
        /// Spec label this session belongs to.
        spec_label: SpecLabel,
        /// Unix-epoch milliseconds the session began. Distinct from
        /// `envelope.ts_ms` (which stamps the event itself) so a single
        /// log replay can recover both "when the session started" and
        /// "when this start event was emitted".
        started_at_ms: i64,
        /// `Task` parent for subagent sessions; `None` for top-level.
        parent_tool_call_id: Option<ToolCallId>,
    },

    /// Agent session ended — paired with [`AgentEvent::AgentStart`].
    /// `SessionComplete` is the cost-aware closer; `agent_end` is a
    /// lifecycle marker the pi protocol emits before its result line.
    AgentEnd {
        #[serde(flatten)]
        envelope: EventEnvelope,
    },

    /// Multi-turn session opened a new turn. Paired with
    /// [`AgentEvent::TurnEnd`].
    TurnStart {
        #[serde(flatten)]
        envelope: EventEnvelope,
    },

    /// Streaming text fragment from the agent.
    TextDelta {
        #[serde(flatten)]
        envelope: EventEnvelope,
        text: String,
    },

    /// Closes a `text_delta` stream — paired terminator for the
    /// streaming assistant message.
    TextEnd {
        #[serde(flatten)]
        envelope: EventEnvelope,
    },

    /// Streaming "thinking" fragment (assistant's internal reasoning
    /// before the visible reply, when the backend exposes it).
    ThinkingDelta {
        #[serde(flatten)]
        envelope: EventEnvelope,
        text: String,
    },

    /// Closes a `thinking_delta` stream.
    ThinkingEnd {
        #[serde(flatten)]
        envelope: EventEnvelope,
    },

    /// Streaming tool-call argument fragment — the agent has decided to
    /// call a tool but is still emitting its JSON params.
    ToolcallDelta {
        #[serde(flatten)]
        envelope: EventEnvelope,
        id: ToolCallId,
        delta: String,
    },

    /// Agent invoked a tool.
    ToolCall {
        #[serde(flatten)]
        envelope: EventEnvelope,
        id: ToolCallId,
        tool: String,
        params: serde_json::Value,
        /// Set when this tool call is nested inside a `Task` subagent
        /// invocation — the renderer indents nested calls under their
        /// parent. Populated by the parser's per-session `Task` stack.
        /// `None` for top-level calls.
        #[serde(default)]
        parent_tool_call_id: Option<ToolCallId>,
    },

    /// Tool execution completed.
    ToolResult {
        #[serde(flatten)]
        envelope: EventEnvelope,
        id: ToolCallId,
        output: String,
        is_error: bool,
    },

    /// In-flight tool update (long-running tool emitting progress lines).
    ToolProgress {
        #[serde(flatten)]
        envelope: EventEnvelope,
        id: ToolCallId,
        text: String,
    },

    /// Agent finished one turn (a multi-turn session may emit several).
    TurnEnd {
        #[serde(flatten)]
        envelope: EventEnvelope,
    },

    /// Agent session completed — the underlying process is exiting or
    /// the final result line was observed.
    SessionComplete {
        #[serde(flatten)]
        envelope: EventEnvelope,
        exit_code: i32,
        cost_usd: Option<f64>,
    },

    /// Agent context compaction has begun.
    CompactionStart {
        #[serde(flatten)]
        envelope: EventEnvelope,
        reason: CompactionReason,
    },

    /// Agent context compaction has ended; `aborted` distinguishes
    /// "compacted successfully" from "compaction abandoned".
    CompactionEnd {
        #[serde(flatten)]
        envelope: EventEnvelope,
        aborted: bool,
    },

    /// Agent backend signaled an auto-retry attempt (pi's auto_retry,
    /// claude's transient-error retries).
    AutoRetry {
        #[serde(flatten)]
        envelope: EventEnvelope,
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        error_message: String,
    },

    /// Agent reported an error mid-stream (does not necessarily end the
    /// session — a `SessionComplete` may follow).
    Error {
        #[serde(flatten)]
        envelope: EventEnvelope,
        message: String,
    },

    /// Driver-side catch-all for events the workflow engine emits about
    /// its own behavior (verdict gate decisions, push gate walks, infra
    /// failures). [`DriverKind`] keeps the wire string forward-compatible
    /// — unknown kinds deserialize to [`DriverKind::Other`] rather than
    /// failing — while still giving consumers an exhaustive `match` over
    /// the spec-enumerated arms. Renderers dispatch on the variant;
    /// `Other` falls through to `→ <driver_kind>: <summary>`.
    DriverEvent {
        #[serde(flatten)]
        envelope: EventEnvelope,
        driver_kind: DriverKind,
        summary: String,
        payload: serde_json::Value,
    },
}

impl AgentEvent {
    /// Borrow the common envelope. All variants carry one — exhaustive
    /// match keeps this in sync as new variants land.
    pub fn envelope(&self) -> &EventEnvelope {
        match self {
            AgentEvent::AgentStart { envelope, .. }
            | AgentEvent::AgentEnd { envelope }
            | AgentEvent::TurnStart { envelope }
            | AgentEvent::TurnEnd { envelope }
            | AgentEvent::TextDelta { envelope, .. }
            | AgentEvent::TextEnd { envelope }
            | AgentEvent::ThinkingDelta { envelope, .. }
            | AgentEvent::ThinkingEnd { envelope }
            | AgentEvent::ToolcallDelta { envelope, .. }
            | AgentEvent::ToolCall { envelope, .. }
            | AgentEvent::ToolResult { envelope, .. }
            | AgentEvent::ToolProgress { envelope, .. }
            | AgentEvent::SessionComplete { envelope, .. }
            | AgentEvent::CompactionStart { envelope, .. }
            | AgentEvent::CompactionEnd { envelope, .. }
            | AgentEvent::AutoRetry { envelope, .. }
            | AgentEvent::Error { envelope, .. }
            | AgentEvent::DriverEvent { envelope, .. } => envelope,
        }
    }

    /// Join a parser-emitted [`ParsedAgentEvent`] with the per-spawn
    /// `envelope` the session layer owns. This is the **only** way to
    /// construct an `AgentEvent` from a parser's output — the type
    /// system enforces RS-12's "parsers cannot emit unstamped events"
    /// rule by giving them no path that produces `AgentEvent` directly.
    pub fn from_parsed(parsed: ParsedAgentEvent, envelope: EventEnvelope) -> Self {
        match parsed {
            ParsedAgentEvent::AgentEnd => AgentEvent::AgentEnd { envelope },
            ParsedAgentEvent::TurnStart => AgentEvent::TurnStart { envelope },
            ParsedAgentEvent::TextDelta { text } => AgentEvent::TextDelta { envelope, text },
            ParsedAgentEvent::TextEnd => AgentEvent::TextEnd { envelope },
            ParsedAgentEvent::ThinkingDelta { text } => {
                AgentEvent::ThinkingDelta { envelope, text }
            }
            ParsedAgentEvent::ThinkingEnd => AgentEvent::ThinkingEnd { envelope },
            ParsedAgentEvent::ToolcallDelta { id, delta } => AgentEvent::ToolcallDelta {
                envelope,
                id,
                delta,
            },
            ParsedAgentEvent::ToolCall {
                id,
                tool,
                params,
                parent_tool_call_id,
            } => AgentEvent::ToolCall {
                envelope,
                id,
                tool,
                params,
                parent_tool_call_id,
            },
            ParsedAgentEvent::ToolResult {
                id,
                output,
                is_error,
            } => AgentEvent::ToolResult {
                envelope,
                id,
                output,
                is_error,
            },
            ParsedAgentEvent::ToolProgress { id, text } => {
                AgentEvent::ToolProgress { envelope, id, text }
            }
            ParsedAgentEvent::TurnEnd => AgentEvent::TurnEnd { envelope },
            ParsedAgentEvent::SessionComplete {
                exit_code,
                cost_usd,
            } => AgentEvent::SessionComplete {
                envelope,
                exit_code,
                cost_usd,
            },
            ParsedAgentEvent::CompactionStart { reason } => {
                AgentEvent::CompactionStart { envelope, reason }
            }
            ParsedAgentEvent::CompactionEnd { aborted } => {
                AgentEvent::CompactionEnd { envelope, aborted }
            }
            ParsedAgentEvent::AutoRetry {
                attempt,
                max_attempts,
                delay_ms,
                error_message,
            } => AgentEvent::AutoRetry {
                envelope,
                attempt,
                max_attempts,
                delay_ms,
                error_message,
            },
            ParsedAgentEvent::Error { message } => AgentEvent::Error { envelope, message },
            ParsedAgentEvent::TokenUsage {
                model,
                input,
                output,
                cache_read,
                cache_write,
            } => {
                let mut env = envelope;
                env.source = Source::Driver;
                let summary = format!(
                    "{model} input={input} output={output} cache_read={cache_read} \
                     cache_write={cache_write}",
                );
                let payload = serde_json::json!({
                    "model": model,
                    "input": input,
                    "output": output,
                    "cache_read": cache_read,
                    "cache_write": cache_write,
                });
                AgentEvent::DriverEvent {
                    envelope: env,
                    driver_kind: DriverKind::TokenUsage,
                    summary,
                    payload,
                }
            }
            ParsedAgentEvent::Offload { tool, total_bytes } => {
                let mut env = envelope;
                env.source = Source::Driver;
                let summary = format!("{tool} offloaded {total_bytes} bytes");
                let payload = serde_json::json!({
                    "tool": tool,
                    "total_bytes": total_bytes,
                });
                AgentEvent::DriverEvent {
                    envelope: env,
                    driver_kind: DriverKind::Offload,
                    summary,
                    payload,
                }
            }
        }
    }
}

/// Parser-emitted event prior to envelope stamping. The parser layer has
/// no visibility into the live bead / molecule / iteration / source /
/// ts_ms / seq context — the session layer joins this payload with the
/// per-spawn [`EventEnvelope`] via [`AgentEvent::from_parsed`].
///
/// `AgentStart` is driver-emitted and never appears here. The Direct
/// backend's runner is uniquely positioned to surface driver facts such
/// as [`DriverKind::TokenUsage`] alongside agent-emitted events, so the
/// parser carries driver-event-equivalent variants; `from_parsed` lifts
/// them into [`AgentEvent::DriverEvent`] with `Source::Driver` regardless
/// of the envelope's configured source.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedAgentEvent {
    AgentEnd,
    TurnStart,
    TextDelta {
        text: String,
    },
    TextEnd,
    ThinkingDelta {
        text: String,
    },
    ThinkingEnd,
    ToolcallDelta {
        id: ToolCallId,
        delta: String,
    },
    ToolCall {
        id: ToolCallId,
        tool: String,
        params: serde_json::Value,
        parent_tool_call_id: Option<ToolCallId>,
    },
    ToolResult {
        id: ToolCallId,
        output: String,
        is_error: bool,
    },
    ToolProgress {
        id: ToolCallId,
        text: String,
    },
    TurnEnd,
    SessionComplete {
        exit_code: i32,
        cost_usd: Option<f64>,
    },
    CompactionStart {
        reason: CompactionReason,
    },
    CompactionEnd {
        aborted: bool,
    },
    AutoRetry {
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        error_message: String,
    },
    Error {
        message: String,
    },
    /// Per-call token accounting emitted by the Direct backend's runner
    /// after every `complete*`. Lifted to `AgentEvent::DriverEvent` with
    /// `driver_kind: DriverKind::TokenUsage` and `source: Source::Driver`
    /// by [`AgentEvent::from_parsed`].
    TokenUsage {
        model: String,
        input: u32,
        output: u32,
        cache_read: u32,
        cache_write: u32,
    },
    /// Successful Direct tool output offload emitted by the runner.
    /// Lifted to `AgentEvent::DriverEvent` with
    /// `driver_kind: DriverKind::Offload` and `source: Source::Driver`.
    Offload {
        tool: String,
        total_bytes: usize,
    },
}

/// Per-session monotonic envelope factory. Threads bead/molecule
/// identity, iteration count, and source through every event the
/// session emits, and stamps each one with the next `seq` value.
///
/// The `ts_ms` clock is injected so tests can pin time. Production
/// callers pass a closure that returns the current `SystemTime` as unix
/// millis; tests pass a counter-backed stub. Keeping the clock as a
/// function (not a trait) avoids pulling tokio/chrono into the leaf
/// `loom-events` crate.
pub struct EnvelopeBuilder {
    bead_id: BeadId,
    molecule_id: Option<MoleculeId>,
    iteration: u32,
    source: Source,
    seq: u64,
    now_ms: Box<dyn FnMut() -> i64 + Send>,
}

impl EnvelopeBuilder {
    /// New builder with `seq` starting at 0. `now` returns unix-epoch
    /// milliseconds — typically `|| { SystemTime::now().duration_since(UNIX_EPOCH).as_millis() as i64 }`
    /// at the driver boundary; tests pass a closure over a counter.
    pub fn new<F>(
        bead_id: BeadId,
        molecule_id: Option<MoleculeId>,
        iteration: u32,
        source: Source,
        now_ms: F,
    ) -> Self
    where
        F: FnMut() -> i64 + Send + 'static,
    {
        Self::with_seq_start(bead_id, molecule_id, iteration, source, 0, now_ms)
    }

    /// New builder that resumes from an explicit `seq_start` rather
    /// than restarting at zero. Used by the run-phase verdict gate
    /// when it picks up the seq counter from the spawn closure's
    /// last-emitted event so the per-spawn stream stays
    /// strictly-increasing across the closure-controller boundary.
    pub fn with_seq_start<F>(
        bead_id: BeadId,
        molecule_id: Option<MoleculeId>,
        iteration: u32,
        source: Source,
        seq_start: u64,
        now_ms: F,
    ) -> Self
    where
        F: FnMut() -> i64 + Send + 'static,
    {
        Self {
            bead_id,
            molecule_id,
            iteration,
            source,
            seq: seq_start,
            now_ms: Box::new(now_ms),
        }
    }

    /// Build the next envelope. `seq` advances by 1 each call. Named
    /// `build` (not `next`) to avoid the `Iterator::next` shadowing
    /// confusion clippy flags.
    pub fn build(&mut self) -> EventEnvelope {
        let source = self.source;
        self.build_with_source(source)
    }

    /// Build the next envelope with `source` overriding the builder's
    /// configured source. Used when a single `EnvelopeBuilder` emits
    /// both agent-sourced events (the streamed parser output) and
    /// driver-sourced events (container lifecycle / infra failure)
    /// inside the same session — the seq counter must keep advancing
    /// across both streams so replay can order them.
    pub fn build_with_source(&mut self, source: Source) -> EventEnvelope {
        let ts_ms = (self.now_ms)();
        let envelope = EventEnvelope {
            bead_id: self.bead_id.clone(),
            molecule_id: self.molecule_id.clone(),
            iteration: self.iteration,
            source,
            ts_ms,
            seq: self.seq,
        };
        self.seq += 1;
        envelope
    }

    /// Borrow the current seq counter without advancing it. Tests use
    /// this to assert monotonicity.
    pub fn current_seq(&self) -> u64 {
        self.seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builder() -> EnvelopeBuilder {
        let mut clock = 0_i64;
        EnvelopeBuilder::new(
            BeadId::new("lm-test").expect("valid id"),
            None,
            0,
            Source::Agent,
            move || {
                clock += 1;
                clock
            },
        )
    }

    /// Every `AgentEvent` variant carries the same envelope fields. Test
    /// the schema by serializing one of each and asserting the top-level
    /// JSON keys include the six envelope fields (plus `kind`).
    #[test]
    fn common_envelope_fields_present_on_every_variant() {
        let mut b = builder();
        let samples: Vec<AgentEvent> = vec![
            AgentEvent::TextDelta {
                envelope: b.build(),
                text: "x".into(),
            },
            AgentEvent::ToolCall {
                envelope: b.build(),
                id: ToolCallId::new("t1"),
                tool: "Read".into(),
                params: serde_json::Value::Null,
                parent_tool_call_id: None,
            },
            AgentEvent::ToolResult {
                envelope: b.build(),
                id: ToolCallId::new("t1"),
                output: String::new(),
                is_error: false,
            },
            AgentEvent::TurnEnd {
                envelope: b.build(),
            },
            AgentEvent::SessionComplete {
                envelope: b.build(),
                exit_code: 0,
                cost_usd: None,
            },
            AgentEvent::CompactionStart {
                envelope: b.build(),
                reason: CompactionReason::ContextLimit,
            },
            AgentEvent::CompactionEnd {
                envelope: b.build(),
                aborted: false,
            },
            AgentEvent::Error {
                envelope: b.build(),
                message: "boom".into(),
            },
        ];
        for event in &samples {
            let v = serde_json::to_value(event).expect("serialize");
            let obj = v.as_object().expect("event serializes as object");
            for key in ["kind", "bead_id", "iteration", "source", "ts_ms", "seq"] {
                assert!(
                    obj.contains_key(key),
                    "event missing envelope key `{key}`: {event:?}\nserialized: {v}",
                );
            }
            // molecule_id is `Option`; it's serialized as `null` when None
            // but the key is still present.
            assert!(
                obj.contains_key("molecule_id"),
                "event missing envelope key `molecule_id`: {event:?}",
            );
        }
    }

    /// `agent_start` carries the extras spec calls out: schema_version,
    /// title, profile, spec_label, started_at_ms, parent_tool_call_id.
    #[test]
    fn agent_start_fields_present() {
        let mut b = builder();
        let event = AgentEvent::AgentStart {
            envelope: b.build(),
            schema_version: 1,
            title: "smoke".into(),
            profile: ProfileName::new("base"),
            spec_label: SpecLabel::new("harness"),
            started_at_ms: 1_700_000_000_000,
            parent_tool_call_id: None,
        };
        let v = serde_json::to_value(&event).expect("serialize");
        let obj = v.as_object().expect("object");
        for key in [
            "schema_version",
            "title",
            "profile",
            "spec_label",
            "started_at_ms",
            "parent_tool_call_id",
        ] {
            assert!(obj.contains_key(key), "agent_start missing `{key}`: {v}",);
        }
        assert_eq!(obj["kind"], "agent_start");
        assert_eq!(obj["schema_version"], 1);
    }

    /// G2 — every variant must serialize-then-deserialize back to the
    /// same value. Catches any `#[serde(flatten)]` / `#[serde(tag)]`
    /// interaction bugs that would corrupt the wire shape.
    #[test]
    fn agent_event_deserialize_round_trip() {
        let mut b = builder();
        let samples: Vec<AgentEvent> = vec![
            AgentEvent::AgentStart {
                envelope: b.build(),
                schema_version: 1,
                title: "smoke".into(),
                profile: ProfileName::new("base"),
                spec_label: SpecLabel::new("harness"),
                started_at_ms: 1_700_000_000_000,
                parent_tool_call_id: None,
            },
            AgentEvent::TextDelta {
                envelope: b.build(),
                text: "hello\nworld".into(),
            },
            AgentEvent::ToolCall {
                envelope: b.build(),
                id: ToolCallId::new("t1"),
                tool: "Read".into(),
                params: serde_json::json!({"file_path": "src/lib.rs"}),
                parent_tool_call_id: None,
            },
            AgentEvent::ToolResult {
                envelope: b.build(),
                id: ToolCallId::new("t1"),
                output: "ok".into(),
                is_error: false,
            },
            AgentEvent::TurnEnd {
                envelope: b.build(),
            },
            AgentEvent::SessionComplete {
                envelope: b.build(),
                exit_code: 0,
                cost_usd: Some(0.5),
            },
            AgentEvent::CompactionStart {
                envelope: b.build(),
                reason: CompactionReason::ContextLimit,
            },
            AgentEvent::CompactionEnd {
                envelope: b.build(),
                aborted: false,
            },
            AgentEvent::Error {
                envelope: b.build(),
                message: "boom".into(),
            },
        ];
        for event in samples {
            let json = serde_json::to_string(&event).expect("serialize");
            let back: AgentEvent = serde_json::from_str(&json).unwrap_or_else(|e| {
                panic!("round-trip parse failed for {event:?}: {e}\njson={json}")
            });
            assert_eq!(back, event, "round-trip mismatch\njson={json}");
        }
    }

    /// G2 — the wire shape is flat: one `kind` discriminator + envelope
    /// fields + variant-specific payload, all at the same nesting level.
    /// No `delta: { ... }` sub-objects. Pin this with an explicit
    /// per-variant JSON shape check.
    #[test]
    fn flat_variant_shape_has_no_nested_envelopes() {
        let mut b = builder();
        let event = AgentEvent::ToolCall {
            envelope: b.build(),
            id: ToolCallId::new("t1"),
            tool: "Read".into(),
            params: serde_json::json!({"file_path": "src/lib.rs"}),
            parent_tool_call_id: None,
        };
        let v = serde_json::to_value(&event).expect("serialize");
        let obj = v.as_object().expect("object");
        // Top-level must have envelope fields directly — no nesting.
        for key in [
            "kind",
            "bead_id",
            "iteration",
            "source",
            "ts_ms",
            "seq",
            "id",
            "tool",
            "params",
        ] {
            assert!(obj.contains_key(key), "flat key `{key}` missing from {v}",);
        }
        // Anti-test: there must NOT be any wrapping `delta`/`payload`/
        // `assistantMessageEvent` keys that would indicate nesting.
        for forbidden in ["delta", "payload", "assistantMessageEvent"] {
            assert!(
                !obj.contains_key(forbidden),
                "forbidden wrapper key `{forbidden}` present in {v}",
            );
        }
    }

    /// The flat payload field set for every `AgentEvent` variant matches
    /// the table in specs/events.md: common envelope fields plus only the
    /// variant's documented payload fields.
    #[test]
    fn agent_event_payload_fields_match_spec() {
        let mut b = builder();
        let samples: Vec<(&str, AgentEvent, &[&str])> = vec![
            (
                "agent_start",
                AgentEvent::AgentStart {
                    envelope: b.build(),
                    schema_version: 1,
                    title: "smoke".into(),
                    profile: ProfileName::new("base"),
                    spec_label: SpecLabel::new("harness"),
                    started_at_ms: 1_700_000_000_000,
                    parent_tool_call_id: None,
                },
                &[
                    "schema_version",
                    "title",
                    "profile",
                    "spec_label",
                    "started_at_ms",
                    "parent_tool_call_id",
                ],
            ),
            (
                "agent_end",
                AgentEvent::AgentEnd {
                    envelope: b.build(),
                },
                &[],
            ),
            (
                "turn_start",
                AgentEvent::TurnStart {
                    envelope: b.build(),
                },
                &[],
            ),
            (
                "text_delta",
                AgentEvent::TextDelta {
                    envelope: b.build(),
                    text: "hello".into(),
                },
                &["text"],
            ),
            (
                "text_end",
                AgentEvent::TextEnd {
                    envelope: b.build(),
                },
                &[],
            ),
            (
                "thinking_delta",
                AgentEvent::ThinkingDelta {
                    envelope: b.build(),
                    text: "thinking".into(),
                },
                &["text"],
            ),
            (
                "thinking_end",
                AgentEvent::ThinkingEnd {
                    envelope: b.build(),
                },
                &[],
            ),
            (
                "toolcall_delta",
                AgentEvent::ToolcallDelta {
                    envelope: b.build(),
                    id: ToolCallId::new("tc-1"),
                    delta: "{".into(),
                },
                &["id", "delta"],
            ),
            (
                "tool_call",
                AgentEvent::ToolCall {
                    envelope: b.build(),
                    id: ToolCallId::new("tc-1"),
                    tool: "Read".into(),
                    params: serde_json::json!({"path": "Cargo.toml"}),
                    parent_tool_call_id: None,
                },
                &["id", "tool", "params", "parent_tool_call_id"],
            ),
            (
                "tool_result",
                AgentEvent::ToolResult {
                    envelope: b.build(),
                    id: ToolCallId::new("tc-1"),
                    output: "ok".into(),
                    is_error: false,
                },
                &["id", "output", "is_error"],
            ),
            (
                "tool_progress",
                AgentEvent::ToolProgress {
                    envelope: b.build(),
                    id: ToolCallId::new("tc-1"),
                    text: "running".into(),
                },
                &["id", "text"],
            ),
            (
                "turn_end",
                AgentEvent::TurnEnd {
                    envelope: b.build(),
                },
                &[],
            ),
            (
                "session_complete",
                AgentEvent::SessionComplete {
                    envelope: b.build(),
                    exit_code: 0,
                    cost_usd: Some(0.25),
                },
                &["exit_code", "cost_usd"],
            ),
            (
                "compaction_start",
                AgentEvent::CompactionStart {
                    envelope: b.build(),
                    reason: CompactionReason::ContextLimit,
                },
                &["reason"],
            ),
            (
                "compaction_end",
                AgentEvent::CompactionEnd {
                    envelope: b.build(),
                    aborted: false,
                },
                &["aborted"],
            ),
            (
                "auto_retry",
                AgentEvent::AutoRetry {
                    envelope: b.build(),
                    attempt: 1,
                    max_attempts: 3,
                    delay_ms: 100,
                    error_message: "retry".into(),
                },
                &["attempt", "max_attempts", "delay_ms", "error_message"],
            ),
            (
                "error",
                AgentEvent::Error {
                    envelope: b.build(),
                    message: "boom".into(),
                },
                &["message"],
            ),
            (
                "driver_event",
                AgentEvent::DriverEvent {
                    envelope: b.build_with_source(Source::Driver),
                    driver_kind: DriverKind::VerdictGate,
                    summary: "summary".into(),
                    payload: serde_json::json!({"detail": 1}),
                },
                &["driver_kind", "summary", "payload"],
            ),
        ];
        let common = [
            "kind",
            "bead_id",
            "molecule_id",
            "iteration",
            "source",
            "ts_ms",
            "seq",
        ];
        for (kind, event, payload_fields) in samples {
            let value = serde_json::to_value(&event).expect("serialize");
            let object = value.as_object().expect("event object");
            assert_eq!(object["kind"], kind);
            let actual = object
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>();
            let expected = common
                .iter()
                .chain(payload_fields.iter())
                .copied()
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(
                actual, expected,
                "payload fields drifted for {kind}: {value}"
            );
        }
    }

    /// Gate lifecycle values are typed `DriverKind` arms carried by the
    /// existing `driver_event.driver_kind` field, not new top-level event
    /// variants.
    #[test]
    fn driver_kind_typed_enum_carries_gate_lifecycle_values() {
        let kinds = [
            (DriverKind::GateRunStart, "gate_run_start"),
            (DriverKind::GateRunScope, "gate_run_scope"),
            (DriverKind::GateRunLane, "gate_run_lane"),
            (DriverKind::GateRunEnd, "gate_run_end"),
            (DriverKind::GateRunSkipped, "gate_run_skipped"),
        ];
        let mut b = builder();
        for (driver_kind, wire) in kinds {
            let event = AgentEvent::DriverEvent {
                envelope: b.build_with_source(Source::Driver),
                driver_kind: driver_kind.clone(),
                summary: format!("{wire} summary"),
                payload: serde_json::json!({"run_id": "r1"}),
            };
            let value = serde_json::to_value(&event).expect("serialize");
            assert_eq!(value["kind"], "driver_event");
            assert_eq!(value["driver_kind"], wire);
            assert_ne!(value["kind"], wire);
            let parsed: AgentEvent = serde_json::from_value(value).expect("deserialize");
            match parsed {
                AgentEvent::DriverEvent {
                    driver_kind: parsed_kind,
                    envelope,
                    ..
                } => {
                    assert_eq!(parsed_kind, driver_kind);
                    assert_eq!(envelope.source, Source::Driver);
                }
                other => panic!("gate lifecycle kind became a top-level variant: {other:?}"),
            }
        }
    }

    /// G2 — unknown `kind` values must fail deserialization loudly. The
    /// log format is small and well-known; a silent skip on unknown
    /// variants would mask the on-disk format drifting from the in-code
    /// enum. Other producers (driver-side `driver_event` from G3) must
    /// declare themselves as variants here before they appear in logs.
    #[test]
    fn unknown_variants_fail_with_a_loud_error() {
        let bogus = serde_json::json!({
            "kind": "this_kind_does_not_exist_yet",
            "bead_id": "lm-test",
            "molecule_id": null,
            "iteration": 0,
            "source": "agent",
            "ts_ms": 0,
            "seq": 0
        });
        let res: Result<AgentEvent, _> = serde_json::from_value(bogus);
        assert!(
            res.is_err(),
            "unknown `kind` must fail to deserialize — got {res:?}",
        );
    }

    /// G3 — every spec variant lands as a real `AgentEvent` arm. Verifies
    /// by deserialing one of each kind tag; the type round-trip is
    /// already covered in `agent_event_deserialize_round_trip`. The
    /// stub-promoted `test_per_tool_summary_cells` dispatcher reads from
    /// this — H3 will extend it to assert renderer behavior.
    #[test]
    fn every_spec_variant_present() {
        let kinds = [
            "agent_start",
            "agent_end",
            "turn_start",
            "turn_end",
            "session_complete",
            "text_delta",
            "text_end",
            "thinking_delta",
            "thinking_end",
            "toolcall_delta",
            "tool_call",
            "tool_result",
            "tool_progress",
            "compaction_start",
            "compaction_end",
            "auto_retry",
            "error",
            "driver_event",
        ];
        assert_eq!(kinds.len(), 18, "spec mandates exactly 18 variants");
    }

    /// G3 — `driver_event` accepts arbitrary `driver_kind` strings;
    /// adding new kinds is additive on the wire and does NOT require a
    /// schema bump. Deserializing two distinct kinds proves this.
    #[test]
    fn driver_event_accepts_unknown_driver_kind() {
        for kind in ["push_gate_walk", "completely_made_up_kind"] {
            let json = serde_json::json!({
                "kind": "driver_event",
                "bead_id": "lm-test",
                "molecule_id": null,
                "iteration": 0,
                "source": "driver",
                "ts_ms": 0,
                "seq": 0,
                "driver_kind": kind,
                "summary": "summary text",
                "payload": {"detail": 42}
            });
            let event: AgentEvent = serde_json::from_value(json)
                .unwrap_or_else(|e| panic!("driver_event with kind={kind} failed: {e}"));
            match event {
                AgentEvent::DriverEvent { driver_kind, .. } => {
                    assert_eq!(driver_kind.as_wire(), kind);
                }
                other => panic!("expected DriverEvent, got {other:?}"),
            }
        }
    }

    /// Every spec-enumerated `driver_kind` round-trips as a `DriverEvent`
    /// carrying `source: "driver"`. Pins the event-schema contract:
    /// verdict gate, retry dispatch, push gate (walk/refuse/clean), and
    /// container lifecycle (spawn/oom) plus the catch-all infra failure
    /// all live as additive `driver_kind` strings under the same variant.
    /// Acts as the rust-side check that the emission sites have a wire
    /// shape to emit into.
    #[test]
    fn driver_kinds_present_for_spec_emission_sites() {
        let kinds = [
            "verdict_gate",
            "retry_dispatch",
            "push_gate_walk",
            "push_gate_refuse",
            "push_gate_clean",
            "container_spawn",
            "container_oom",
            "infra_failure",
            "token_usage",
            "offload",
            "duplicate_tool_result",
            "doom_loop_tripped",
            "epic_auto_closed",
            "bead_branch_pushed",
            "merge_ok",
            "merge_conflict",
            "integration_conflict",
            "signature_verification_failed",
            "worktree_cleanup_ok",
            "tree_not_clean",
            "gate_run_start",
            "gate_run_scope",
            "gate_run_lane",
            "gate_run_end",
            "gate_run_skipped",
        ];
        for kind in kinds {
            let json = serde_json::json!({
                "kind": "driver_event",
                "bead_id": "lm-test",
                "molecule_id": null,
                "iteration": 0,
                "source": "driver",
                "ts_ms": 0,
                "seq": 0,
                "driver_kind": kind,
                "summary": format!("{kind} summary"),
                "payload": {}
            });
            let event: AgentEvent = serde_json::from_value(json)
                .unwrap_or_else(|e| panic!("driver_event kind={kind} failed: {e}"));
            match event {
                AgentEvent::DriverEvent {
                    envelope,
                    driver_kind,
                    summary,
                    ..
                } => {
                    assert_eq!(driver_kind.as_wire(), kind);
                    assert_eq!(
                        envelope.source,
                        Source::Driver,
                        "driver-emitted events carry Source::Driver",
                    );
                    assert!(
                        !summary.is_empty(),
                        "summary always present so unknown-kind renderer fallback works",
                    );
                }
                other => panic!("expected DriverEvent for kind={kind}, got {other:?}"),
            }
        }
    }

    /// `EnvelopeBuilder::build` advances `seq` by exactly 1 each call.
    /// Replay code reorders events by `(bead_id, seq)`; off-by-one or
    /// reset bugs in the producer would break replay silently.
    #[test]
    fn seq_advances_monotonically() {
        let mut b = builder();
        let seqs: Vec<u64> = (0..10).map(|_| b.build().seq).collect();
        let expected: Vec<u64> = (0..10).collect();
        assert_eq!(seqs, expected);
    }

    /// `build_with_source` overrides the builder's configured source for
    /// one envelope while still advancing the shared seq counter. This
    /// is the path the session driver uses to interleave driver-sourced
    /// container/infra events with the agent-sourced parser stream
    /// without spinning up a second builder.
    #[test]
    fn build_with_source_overrides_source_and_shares_seq() {
        let mut b = builder();
        let agent_env = b.build();
        assert_eq!(agent_env.source, Source::Agent);
        assert_eq!(agent_env.seq, 0);
        let driver_env = b.build_with_source(Source::Driver);
        assert_eq!(driver_env.source, Source::Driver);
        assert_eq!(
            driver_env.seq, 1,
            "build_with_source shares the same seq counter as build",
        );
        let next_agent = b.build();
        assert_eq!(next_agent.source, Source::Agent);
        assert_eq!(next_agent.seq, 2);
    }

    /// RS-17 — every spec-enumerated wire string maps to its known
    /// `DriverKind` variant and round-trips back to the same wire string.
    /// Unknown strings deserialize to `DriverKind::Other` so additive
    /// growth on the producer side never breaks older consumers.
    #[test]
    fn driver_kind_round_trips_known_and_unknown_wire_strings() {
        let known = [
            ("verdict_gate", DriverKind::VerdictGate),
            ("retry_dispatch", DriverKind::RetryDispatch),
            ("push_gate_walk", DriverKind::PushGateWalk),
            ("push_gate_refuse", DriverKind::PushGateRefuse),
            ("push_gate_clean", DriverKind::PushGateClean),
            ("container_spawn", DriverKind::ContainerSpawn),
            ("container_oom", DriverKind::ContainerOom),
            ("infra_failure", DriverKind::InfraFailure),
            ("token_usage", DriverKind::TokenUsage),
            ("offload", DriverKind::Offload),
            ("duplicate_tool_result", DriverKind::DuplicateToolResult),
            ("doom_loop_tripped", DriverKind::DoomLoopTripped),
            ("epic_auto_closed", DriverKind::EpicAutoClosed),
            ("bead_branch_pushed", DriverKind::BeadBranchPushed),
            ("merge_ok", DriverKind::MergeOk),
            ("merge_conflict", DriverKind::MergeConflict),
            ("integration_conflict", DriverKind::IntegrationConflict),
            (
                "signature_verification_failed",
                DriverKind::SignatureVerificationFailed,
            ),
            ("worktree_cleanup_ok", DriverKind::WorktreeCleanupOk),
            ("tree_not_clean", DriverKind::TreeNotClean),
            ("gate_run_start", DriverKind::GateRunStart),
            ("gate_run_scope", DriverKind::GateRunScope),
            ("gate_run_lane", DriverKind::GateRunLane),
            ("gate_run_end", DriverKind::GateRunEnd),
            ("gate_run_skipped", DriverKind::GateRunSkipped),
        ];
        for (wire, variant) in known {
            assert_eq!(DriverKind::from_wire(wire), variant);
            assert_eq!(variant.as_wire(), wire);
            let json = serde_json::to_string(&variant).expect("ser");
            let back: DriverKind = serde_json::from_str(&json).expect("de");
            assert_eq!(back, variant);
        }
        let unknown = DriverKind::from_wire("push_gate_exhausted");
        assert_eq!(
            unknown,
            DriverKind::Other("push_gate_exhausted".to_string()),
        );
        let json = serde_json::to_string(&unknown).expect("ser");
        assert_eq!(json, "\"push_gate_exhausted\"");
        let back: DriverKind = serde_json::from_str(&json).expect("de");
        assert_eq!(back, unknown);
    }

    /// `AgentEvent::from_parsed` joins each `ParsedAgentEvent` variant
    /// with the supplied envelope without altering the payload. This is
    /// the type-system gate on RS-12 — parsers cannot reach a stamped
    /// `AgentEvent` except via this constructor.
    #[test]
    fn from_parsed_round_trips_payload_fields() {
        let mut b = builder();
        let env = b.build();
        let parsed = ParsedAgentEvent::ToolCall {
            id: ToolCallId::new("t1"),
            tool: "Bash".into(),
            params: serde_json::json!({"command": "ls"}),
            parent_tool_call_id: None,
        };
        match AgentEvent::from_parsed(parsed, env.clone()) {
            AgentEvent::ToolCall {
                envelope,
                id,
                tool,
                params,
                parent_tool_call_id,
            } => {
                assert_eq!(envelope, env);
                assert_eq!(id.as_str(), "t1");
                assert_eq!(tool, "Bash");
                assert_eq!(params["command"], "ls");
                assert!(parent_tool_call_id.is_none());
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    /// `ParsedAgentEvent::TokenUsage` lifts into `AgentEvent::DriverEvent`
    /// with `driver_kind: TokenUsage` and overrides the envelope's
    /// `source` to `Source::Driver` regardless of the configured source.
    /// SaaS billing pipelines tail the live event stream and rely on this
    /// per-call accounting frame having driver provenance.
    #[test]
    fn from_parsed_token_usage_emits_driver_event_with_driver_source() {
        let mut b = builder();
        let mut env = b.build();
        env.source = Source::Agent;
        let parsed = ParsedAgentEvent::TokenUsage {
            model: "claude-sonnet-4-6".to_string(),
            input: 1_000,
            output: 250,
            cache_read: 600,
            cache_write: 400,
        };
        match AgentEvent::from_parsed(parsed, env) {
            AgentEvent::DriverEvent {
                envelope,
                driver_kind,
                summary,
                payload,
            } => {
                assert_eq!(driver_kind, DriverKind::TokenUsage);
                assert_eq!(envelope.source, Source::Driver);
                assert_eq!(payload["model"], "claude-sonnet-4-6");
                assert_eq!(payload["input"], 1_000);
                assert_eq!(payload["output"], 250);
                assert_eq!(payload["cache_read"], 600);
                assert_eq!(payload["cache_write"], 400);
                assert!(
                    payload.get("cost_cents").is_none(),
                    "payload must not carry cost_cents per spec: {payload}",
                );
                assert!(summary.contains("claude-sonnet-4-6"));
            }
            other => panic!("expected DriverEvent, got {other:?}"),
        }
    }

    /// `ParsedAgentEvent::Offload` lifts into `AgentEvent::DriverEvent`
    /// with `driver_kind: Offload`, preserving the tool name and byte
    /// count the Direct tool context recorded at the offload point.
    #[test]
    fn from_parsed_offload_emits_driver_event_with_tool_and_byte_count() {
        let mut b = builder();
        let mut env = b.build();
        env.source = Source::Agent;
        let parsed = ParsedAgentEvent::Offload {
            tool: "Read".to_string(),
            total_bytes: 42,
        };
        match AgentEvent::from_parsed(parsed, env) {
            AgentEvent::DriverEvent {
                envelope,
                driver_kind,
                summary,
                payload,
            } => {
                assert_eq!(driver_kind, DriverKind::Offload);
                assert_eq!(envelope.source, Source::Driver);
                assert_eq!(payload["tool"], "Read");
                assert_eq!(payload["total_bytes"], 42);
                assert!(summary.contains("Read"));
                assert!(summary.contains("42"));
            }
            other => panic!("expected DriverEvent, got {other:?}"),
        }
    }
}

/// Why the agent compacted its context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionReason {
    /// Approaching or exceeded the model context limit.
    ContextLimit,
    /// User (or driver) explicitly requested compaction.
    UserRequested,
    /// Reason was not present or did not match a known value.
    Unknown,
}

//! `DoomLoopObserver` — detects when the agent calls the same tool
//! with the same params *and* the same result repeated, then escalates
//! via a two-stage `Steer` -> `Abort` response.
//!
//! Detection keys on `(CallKey, ResultHash)` where `CallKey =
//! (tool_name, canonical_params)` (canonical JSON per RFC 8785 JCS,
//! normalized numbers) and `ResultHash` is `BLAKE3-16(canonical
//! result)` — shared with `DuplicateResultObserver` via
//! [`super::result_hasher::ResultHasher`].

use std::collections::{HashMap, VecDeque};

use loom_events::identifier::ToolCallId;
use loom_events::{AgentEvent, EventSink, SessionCommand};
use serde_json::Value;

use super::result_hasher::{CallKey, ResultHash, ResultHasher};

/// Per-observer configuration. Mirrors the `[agent.doom_loop]` TOML block
/// the binary's `LoomConfig` exposes — consumers driving
/// [`crate::Conversation`] directly construct the same shape and pass it
/// in via [`crate::Conversation::doom_loop`] (or rely on
/// [`DoomLoopConfig::default`] which matches the spec defaults).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoomLoopConfig {
    /// When false, the observer is omitted from
    /// [`crate::Conversation`]'s default sink chain entirely.
    pub enabled: bool,
    /// Sliding-window size in `(CallKey, ResultHash)` entries.
    pub window: u32,
    /// Identical-pair count within the window that trips stage 1.
    pub threshold: u32,
    /// Additional identical pairs required after stage 1 before stage 2
    /// emits an `Abort`.
    pub stage_2_after_stage_1: u32,
}

impl Default for DoomLoopConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            window: 5,
            threshold: 3,
            stage_2_after_stage_1: 3,
        }
    }
}

/// Observability payload drained by [`DoomLoopObserver::take_pending`]
/// and lifted into a `DriverKind::DoomLoopTripped` `AgentEvent` by the
/// sink-chain wiring. The observer cannot synthesize the wire event
/// itself — it carries no `EnvelopeBuilder` — so it surfaces the
/// payload-shaped struct and lets the chain assemble the event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoomLoopTripped {
    /// Stage that fired: `1` for `Steer`, `2` for `Abort`.
    pub stage: u8,
    /// Tool name from the originating `ToolCall`.
    pub tool: String,
    /// Canonical params from the originating `ToolCall`.
    pub params: Value,
    /// `ToolCallId` of the result that closed the detection.
    pub call_id: ToolCallId,
}

/// Per-`CallKey` escalation state. Transitions are
/// `NotTripped → Stage1 → Stage2`; `Stage2` is terminal until
/// `CompactionEnd` resets all observer state.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StageState {
    NotTripped,
    Stage1 {
        trigger: ResultHash,
        additional: u32,
    },
    Stage2,
}

/// Observer state for one session. Resets on `CompactionEnd`; does NOT
/// reset on `TurnEnd` — agent doom loops routinely span turns and
/// compaction is the actual context reset.
pub struct DoomLoopObserver {
    hasher: ResultHasher,
    window: u32,
    threshold: u32,
    stage_2_after_stage_1: u32,
    /// `ToolCall` payloads waiting to be paired with their matching
    /// `ToolResult`. Cleared on `CompactionEnd`.
    pending_calls: HashMap<ToolCallId, PendingCall>,
    /// Per-`CallKey` sliding window of recent `ResultHash` values.
    windows: HashMap<CallKey, VecDeque<ResultHash>>,
    /// Per-`CallKey` escalation state.
    stages: HashMap<CallKey, StageState>,
    /// `SessionCommand`s drained on `react()`.
    pending_commands: Vec<SessionCommand>,
    /// `DoomLoopTripped` payloads drained on `take_pending()`.
    pending_tripped: Vec<DoomLoopTripped>,
}

#[derive(Debug, Clone)]
struct PendingCall {
    tool: String,
    params: Value,
}

impl DoomLoopObserver {
    /// Construct an observer with documented defaults: 3-of-5 window,
    /// 3 additional pairs after stage 1 before stage 2.
    pub fn new() -> Self {
        Self {
            hasher: ResultHasher::new(),
            window: 5,
            threshold: 3,
            stage_2_after_stage_1: 3,
            pending_calls: HashMap::new(),
            windows: HashMap::new(),
            stages: HashMap::new(),
            pending_commands: Vec::new(),
            pending_tripped: Vec::new(),
        }
    }

    /// Override the sliding-window size.
    pub fn with_window(mut self, n: u32) -> Self {
        self.window = n;
        self
    }

    /// Override the identical-pair threshold for stage 1.
    pub fn with_threshold(mut self, n: u32) -> Self {
        self.threshold = n;
        self
    }

    /// Override the additional-pair gap before stage 2 fires.
    pub fn with_stage_2_after_stage_1(mut self, n: u32) -> Self {
        self.stage_2_after_stage_1 = n;
        self
    }

    /// Construct an observer with knobs sourced from `config`. The
    /// `enabled` flag is consulted by [`crate::Conversation`]'s builder —
    /// it's irrelevant here because the caller already decided to
    /// materialise the observer.
    pub fn from_config(config: &DoomLoopConfig) -> Self {
        Self::new()
            .with_window(config.window)
            .with_threshold(config.threshold)
            .with_stage_2_after_stage_1(config.stage_2_after_stage_1)
    }

    /// Borrow the shared hasher.
    pub fn hasher(&self) -> &ResultHasher {
        &self.hasher
    }

    /// Read-only access to the configured window size.
    pub fn window(&self) -> u32 {
        self.window
    }

    /// Read-only access to the configured stage-1 threshold.
    pub fn threshold(&self) -> u32 {
        self.threshold
    }

    /// Read-only access to the configured stage-2 gap.
    pub fn stage_2_after_stage_1(&self) -> u32 {
        self.stage_2_after_stage_1
    }

    /// Drain detected escalations. The sink-chain wiring calls this
    /// after each non-streaming event, lifts each payload into a
    /// `DriverKind::DoomLoopTripped` `AgentEvent`, and fans the event
    /// into the rest of the chain.
    pub fn take_pending(&mut self) -> Vec<DoomLoopTripped> {
        std::mem::take(&mut self.pending_tripped)
    }

    fn reset(&mut self) {
        self.pending_calls.clear();
        self.windows.clear();
        self.stages.clear();
    }

    fn record_call(&mut self, id: &ToolCallId, tool: &str, params: &Value) {
        self.pending_calls.insert(
            id.clone(),
            PendingCall {
                tool: tool.to_owned(),
                params: params.clone(),
            },
        );
    }

    fn process_result(&mut self, id: &ToolCallId, output: &str) {
        let Some(call) = self.pending_calls.get(id).cloned() else {
            return;
        };
        let result_value = parse_output(output);
        let call_key = ResultHasher::call_key(&call.tool, &call.params);
        let hash = ResultHasher::result_hash(&result_value);
        let window_cap = self.window as usize;

        let window = self.windows.entry(call_key.clone()).or_default();
        window.push_back(hash);
        while window.len() > window_cap {
            window.pop_front();
        }
        let window_snapshot: Vec<ResultHash> = window.iter().copied().collect();

        let state = self
            .stages
            .entry(call_key.clone())
            .or_insert(StageState::NotTripped)
            .clone();

        match state {
            StageState::NotTripped => {
                if let Some(trigger) = trigger_hash(&window_snapshot, self.threshold) {
                    self.stages.insert(
                        call_key,
                        StageState::Stage1 {
                            trigger,
                            additional: 0,
                        },
                    );
                    self.fire_stage_1(&call.tool, &call.params, id);
                }
            }
            StageState::Stage1 {
                trigger,
                additional,
            } => {
                if hash == trigger {
                    let next = additional + 1;
                    if next >= self.stage_2_after_stage_1 {
                        self.stages.insert(call_key, StageState::Stage2);
                        self.fire_stage_2(&call.tool, &call.params, id);
                    } else {
                        self.stages.insert(
                            call_key,
                            StageState::Stage1 {
                                trigger,
                                additional: next,
                            },
                        );
                    }
                }
            }
            StageState::Stage2 => {}
        }
    }

    fn fire_stage_1(&mut self, tool: &str, params: &Value, call_id: &ToolCallId) {
        let message = format!(
            "doom-loop suspected for tool `{tool}`: result and params have been \
             identical {threshold} times in the last {window} calls. \
             The session will abort if {budget} more identical calls land for \
             this tool. Reconsider this approach, vary params or strategy, or \
             escalate by emitting LOOM_BLOCKED.",
            threshold = self.threshold,
            window = self.window,
            budget = self.stage_2_after_stage_1,
        );
        self.pending_commands.push(SessionCommand::Steer(message));
        self.pending_tripped.push(DoomLoopTripped {
            stage: 1,
            tool: tool.to_owned(),
            params: params.clone(),
            call_id: call_id.clone(),
        });
    }

    fn fire_stage_2(&mut self, tool: &str, params: &Value, call_id: &ToolCallId) {
        let reason = format!("doom-loop: {tool}");
        self.pending_commands.push(SessionCommand::Abort(reason));
        self.pending_tripped.push(DoomLoopTripped {
            stage: 2,
            tool: tool.to_owned(),
            params: params.clone(),
            call_id: call_id.clone(),
        });
    }
}

impl Default for DoomLoopObserver {
    fn default() -> Self {
        Self::new()
    }
}

impl EventSink for DoomLoopObserver {
    fn emit(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::CompactionEnd { .. } => {
                self.reset();
            }
            AgentEvent::ToolCall {
                id, tool, params, ..
            } => {
                self.record_call(id, tool, params);
            }
            AgentEvent::ToolResult { id, output, .. } => {
                self.process_result(id, output);
            }
            _ => {}
        }
    }

    fn react(&mut self) -> Vec<SessionCommand> {
        std::mem::take(&mut self.pending_commands)
    }
}

fn parse_output(output: &str) -> Value {
    serde_json::from_str::<Value>(output).unwrap_or_else(|_| Value::String(output.to_owned()))
}

/// Return the `ResultHash` that occurs at least `threshold` times in
/// `window`, or `None` if no hash crosses the threshold. The first
/// qualifying hash in deque order wins; ties resolve to the
/// earliest-seen.
fn trigger_hash(window: &[ResultHash], threshold: u32) -> Option<ResultHash> {
    let threshold = threshold as usize;
    let mut counts: Vec<(ResultHash, usize)> = Vec::new();
    for hash in window {
        if let Some(entry) = counts.iter_mut().find(|(h, _)| h == hash) {
            entry.1 += 1;
        } else {
            counts.push((*hash, 1));
        }
    }
    counts
        .into_iter()
        .find(|(_, count)| *count >= threshold)
        .map(|(hash, _)| hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    use loom_events::event::{CompactionReason, EventEnvelope, Source};
    use loom_events::identifier::BeadId;
    use serde_json::json;

    fn envelope(seq: u64) -> EventEnvelope {
        EventEnvelope {
            bead_id: BeadId::new("wx-test").expect("valid bead id"),
            molecule_id: None,
            iteration: 0,
            source: Source::Agent,
            ts_ms: seq as i64,
            seq,
        }
    }

    fn tool_call(seq: u64, id: &str, tool: &str, params: Value) -> AgentEvent {
        AgentEvent::ToolCall {
            envelope: envelope(seq),
            id: ToolCallId::new(id),
            tool: tool.to_owned(),
            params,
            parent_tool_call_id: None,
        }
    }

    fn tool_result(seq: u64, id: &str, output: &str) -> AgentEvent {
        AgentEvent::ToolResult {
            envelope: envelope(seq),
            id: ToolCallId::new(id),
            output: output.to_owned(),
            is_error: false,
        }
    }

    fn turn_end(seq: u64) -> AgentEvent {
        AgentEvent::TurnEnd {
            envelope: envelope(seq),
        }
    }

    fn compaction_end(seq: u64) -> AgentEvent {
        AgentEvent::CompactionEnd {
            envelope: envelope(seq),
            aborted: false,
        }
    }

    fn drive_call(
        obs: &mut DoomLoopObserver,
        seq: &mut u64,
        id: &str,
        params: Value,
        output: &str,
    ) {
        obs.emit(&tool_call(*seq, id, "read_file", params));
        *seq += 1;
        obs.emit(&tool_result(*seq, id, output));
        *seq += 1;
    }

    /// Detection keys on `(CallKey, ResultHash)` — the same params with
    /// reordered keys form the same `CallKey`, and the same canonical
    /// result forms the same `ResultHash`, so three reordered-but-equal
    /// pairs trip stage 1 just like three byte-identical pairs do.
    #[test]
    fn doom_loop_key_uses_canonical_call_args_and_result_hash() {
        let mut obs = DoomLoopObserver::new();
        let mut seq = 0u64;
        let params_a = json!({"path": "/etc/hosts", "limit": 10});
        let params_b = json!({"limit": 10, "path": "/etc/hosts"});
        let result_a = r#"{"a":1,"b":2}"#;
        let result_b = r#"{"b":2,"a":1}"#;

        drive_call(&mut obs, &mut seq, "call-1", params_a.clone(), result_a);
        drive_call(&mut obs, &mut seq, "call-2", params_b.clone(), result_b);
        drive_call(&mut obs, &mut seq, "call-3", params_a.clone(), result_a);

        let commands = obs.react();
        assert_eq!(commands.len(), 1, "stage 1 fires under canonical equality");
        assert!(matches!(commands[0], SessionCommand::Steer(_)));
    }

    /// Stage 1 fires exactly when 3 of the last 5 entries in the
    /// per-CallKey window are identical pairs.
    #[test]
    fn doom_loop_stage_1_fires_at_3_of_5_identical() {
        let mut obs = DoomLoopObserver::new();
        let mut seq = 0u64;
        let params = json!({"path": "/etc/hosts"});
        let same = r#"{"content":"127.0.0.1 localhost"}"#;
        let different = r#"{"content":"different"}"#;

        drive_call(&mut obs, &mut seq, "call-1", params.clone(), same);
        drive_call(&mut obs, &mut seq, "call-2", params.clone(), different);
        assert!(obs.react().is_empty(), "1 identical pair never trips");
        drive_call(&mut obs, &mut seq, "call-3", params.clone(), same);
        assert!(obs.react().is_empty(), "2 identical pairs never trips");
        drive_call(&mut obs, &mut seq, "call-4", params.clone(), same);
        let commands = obs.react();
        assert_eq!(commands.len(), 1, "third identical pair trips stage 1");
        assert!(matches!(commands[0], SessionCommand::Steer(_)));
    }

    /// Stage 1's `Steer` names the tool, declares the budget before
    /// abort, and invites `LOOM_BLOCKED` escalation.
    #[test]
    fn doom_loop_stage_1_steer_names_tool_budget_and_escalation_path() {
        let mut obs = DoomLoopObserver::new();
        let mut seq = 0u64;
        let params = json!({"path": "/etc/hosts"});
        let same = r#"{"content":"x"}"#;
        for id in ["a", "b", "c"] {
            drive_call(&mut obs, &mut seq, id, params.clone(), same);
        }
        let commands = obs.react();
        let SessionCommand::Steer(message) = &commands[0] else {
            panic!("expected Steer, got {commands:?}");
        };
        assert!(
            message.contains("read_file"),
            "steer must name the tool: {message:?}",
        );
        assert!(
            message.contains("LOOM_BLOCKED"),
            "steer must invite LOOM_BLOCKED escalation: {message:?}",
        );
        assert!(
            message.contains('3'),
            "steer must declare the configured budget number: {message:?}",
        );
        assert!(
            message.to_lowercase().contains("identical"),
            "steer must call out the identical pattern: {message:?}",
        );
    }

    /// Stage 1 also lands a `DoomLoopTripped { stage: 1, ... }` payload
    /// in the observability bin for the sink-chain wiring to lift into
    /// a `DriverKind::DoomLoopTripped` event.
    #[test]
    fn doom_loop_stage_1_emits_driver_event() {
        let mut obs = DoomLoopObserver::new();
        let mut seq = 0u64;
        let params = json!({"path": "/etc/hosts"});
        let same = r#"{"content":"x"}"#;
        for id in ["a", "b", "c"] {
            drive_call(&mut obs, &mut seq, id, params.clone(), same);
        }
        let tripped = obs.take_pending();
        assert_eq!(tripped.len(), 1);
        assert_eq!(tripped[0].stage, 1);
        assert_eq!(tripped[0].tool, "read_file");
        assert_eq!(tripped[0].params, params);
        assert_eq!(tripped[0].call_id.as_str(), "c");
    }

    /// Stage 2 requires `stage_2_after_stage_1` more identical pairs
    /// after stage 1 trips, with that knob configurable. Stage 1 trips
    /// on the 3rd identical call; stage 2 fires on the (3+N)th.
    #[test]
    fn doom_loop_stage_2_requires_configurable_extra_pairs_after_stage_1() {
        let mut obs = DoomLoopObserver::new().with_stage_2_after_stage_1(2);
        let mut seq = 0u64;
        let params = json!({"path": "/etc/hosts"});
        let same = r#"{"content":"x"}"#;

        drive_call(&mut obs, &mut seq, "c1", params.clone(), same);
        drive_call(&mut obs, &mut seq, "c2", params.clone(), same);
        drive_call(&mut obs, &mut seq, "c3", params.clone(), same);
        let stage_1 = obs.react();
        assert!(matches!(stage_1.as_slice(), [SessionCommand::Steer(_)]));

        drive_call(&mut obs, &mut seq, "c4", params.clone(), same);
        let after_one = obs.react();
        assert!(
            after_one.is_empty(),
            "one extra identical pair must not fire stage 2 (need 2): {after_one:?}",
        );

        drive_call(&mut obs, &mut seq, "c5", params.clone(), same);
        let stage_2 = obs.react();
        assert_eq!(stage_2.len(), 1);
        match &stage_2[0] {
            SessionCommand::Abort(reason) => {
                assert_eq!(reason, "doom-loop: read_file");
            }
            other => panic!("expected Abort, got {other:?}"),
        }
    }

    /// Stage 2 also emits a `DoomLoopTripped { stage: 2, ... }` payload.
    #[test]
    fn doom_loop_stage_2_emits_driver_event() {
        let mut obs = DoomLoopObserver::new().with_stage_2_after_stage_1(1);
        let mut seq = 0u64;
        let params = json!({"path": "/etc/hosts"});
        let same = r#"{"content":"x"}"#;

        for id in ["c1", "c2", "c3", "c4"] {
            drive_call(&mut obs, &mut seq, id, params.clone(), same);
        }
        let tripped = obs.take_pending();
        let stages: Vec<u8> = tripped.iter().map(|t| t.stage).collect();
        assert_eq!(stages, vec![1, 2]);
        assert_eq!(tripped[1].tool, "read_file");
        assert_eq!(tripped[1].params, params);
        assert_eq!(tripped[1].call_id.as_str(), "c4");
    }

    /// State persists across `TurnEnd` (doom loops span turns) and
    /// clears on `CompactionEnd`.
    #[test]
    fn doom_loop_resets_on_compaction_end_not_turn_end() {
        let mut obs = DoomLoopObserver::new();
        let mut seq = 0u64;
        let params = json!({"path": "/etc/hosts"});
        let same = r#"{"content":"x"}"#;

        drive_call(&mut obs, &mut seq, "c1", params.clone(), same);
        drive_call(&mut obs, &mut seq, "c2", params.clone(), same);

        obs.emit(&turn_end(seq));
        seq += 1;
        drive_call(&mut obs, &mut seq, "c3", params.clone(), same);
        let across_turn = obs.react();
        assert!(
            matches!(across_turn.as_slice(), [SessionCommand::Steer(_)]),
            "TurnEnd must NOT reset state — third identical pair across turns still trips stage 1",
        );

        let _ = obs.take_pending();
        obs.emit(&compaction_end(seq));
        seq += 1;

        drive_call(&mut obs, &mut seq, "c4", params.clone(), same);
        drive_call(&mut obs, &mut seq, "c5", params.clone(), same);
        assert!(
            obs.react().is_empty(),
            "post-CompactionEnd: 2 identical pairs must not trip yet",
        );
        drive_call(&mut obs, &mut seq, "c6", params.clone(), same);
        let post_compaction = obs.react();
        assert!(
            matches!(post_compaction.as_slice(), [SessionCommand::Steer(_)]),
            "post-CompactionEnd: third identical pair trips again from a clean state",
        );
    }

    /// `CompactionEnd` also clears the pending-calls map so a stale
    /// `ToolCall`/`ToolResult` cross-pair cannot leak across the reset.
    #[test]
    fn compaction_end_clears_pending_calls() {
        let mut obs = DoomLoopObserver::new();
        obs.emit(&tool_call(0, "orphan", "read_file", json!({"path": "/"})));
        obs.emit(&AgentEvent::CompactionEnd {
            envelope: envelope(1),
            aborted: false,
        });
        obs.emit(&tool_result(2, "orphan", r#"{"content":"x"}"#));
        let tripped = obs.take_pending();
        assert!(
            tripped.is_empty(),
            "stale ToolCall must not pair with post-reset ToolResult",
        );
        let commands = obs.react();
        assert!(commands.is_empty());
        let _ = CompactionReason::ContextLimit;
    }

    /// Once stage 2 has fired the observer must not keep emitting
    /// further `Abort`s for the same `CallKey` even if identical
    /// pairs continue landing.
    #[test]
    fn doom_loop_stage_2_is_terminal_for_call_key() {
        let mut obs = DoomLoopObserver::new().with_stage_2_after_stage_1(1);
        let mut seq = 0u64;
        let params = json!({"path": "/etc/hosts"});
        let same = r#"{"content":"x"}"#;

        for id in ["c1", "c2", "c3", "c4"] {
            drive_call(&mut obs, &mut seq, id, params.clone(), same);
        }
        let _ = obs.react();
        let _ = obs.take_pending();

        drive_call(&mut obs, &mut seq, "c5", params.clone(), same);
        assert!(
            obs.react().is_empty(),
            "stage 2 already fired — further identical pairs must not re-abort",
        );
        assert!(obs.take_pending().is_empty());
    }
}

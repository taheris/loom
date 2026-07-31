//! Observe repeated canonical tool results without steering the session.

use std::collections::HashMap;

use loom_events::identifier::ToolCallId;
use loom_events::{AgentEvent, EventSink, SessionCommand};

use super::result_hasher::{ResultFingerprint, ResultHash, ResultHasher};

/// Default `min_bytes` threshold below which short results are skipped
/// — keeps the dedup map from being dominated by trivially-short
/// payloads like `"ok"`.
pub const DEFAULT_MIN_BYTES: u32 = 256;

/// Configuration for [`DuplicateResultObserver`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub struct DuplicateResultConfig {
    /// When false, the observer is omitted from
    /// [`crate::Conversation`]'s default sink chain entirely.
    pub enabled: bool,
    /// Skip results whose canonical payload is shorter than this.
    pub min_bytes: u32,
}

impl Default for DuplicateResultConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_bytes: DEFAULT_MIN_BYTES,
        }
    }
}

/// Duplicate result awaiting delivery as an observability event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateDetection {
    /// `ToolCallId` of the first tool call whose canonical result
    /// hashed to this `ResultHash` (the call billed for the canonical
    /// work the duplicate is repeating).
    pub original_call_id: ToolCallId,
    /// `ToolCallId` of the later tool call whose canonical result
    /// duplicated `original_call_id`'s payload.
    pub repeated_call_id: ToolCallId,
    /// Canonical-payload byte length of the duplicate — the amount of
    /// downstream context the agent burned re-fetching information
    /// already in transcript.
    pub bytes_wasted: u64,
}

/// Observer state for one session. State resets on `CompactionEnd`.
pub struct DuplicateResultObserver {
    /// Shared canonicalization + hashing pipeline (same instance shape
    /// the `DoomLoopObserver` uses).
    hasher: ResultHasher,
    /// Skip results below this byte count.
    min_bytes: u32,
    /// First-seen winner per result hash. Resets on `CompactionEnd`.
    seen: HashMap<ResultHash, ToolCallId>,
    /// Detected duplicates awaiting drain by the sink-chain wiring.
    pending: Vec<DuplicateDetection>,
}

impl DuplicateResultObserver {
    /// Construct an observer with documented defaults
    /// (`min_bytes = 256`).
    pub fn new() -> Self {
        Self {
            hasher: ResultHasher::new(),
            min_bytes: DEFAULT_MIN_BYTES,
            seen: HashMap::new(),
            pending: Vec::new(),
        }
    }

    /// Override the `min_bytes` threshold.
    #[must_use]
    pub const fn with_min_bytes(mut self, n: u32) -> Self {
        self.min_bytes = n;
        self
    }

    /// Construct an observer with knobs sourced from `config`. The
    /// `enabled` flag is consulted by [`crate::Conversation`]'s builder —
    /// it's irrelevant here because the caller already decided to
    /// materialise the observer.
    pub fn from_config(config: DuplicateResultConfig) -> Self {
        Self::new().with_min_bytes(config.min_bytes)
    }

    /// Borrow the shared hasher.
    pub const fn hasher(&self) -> &ResultHasher {
        &self.hasher
    }

    /// Read-only access to the configured threshold.
    pub const fn min_bytes(&self) -> u32 {
        self.min_bytes
    }

    /// Drain detected duplicates. The sink-chain wiring calls this
    /// after each non-streaming event, lifts each detection into a
    /// `DriverKind::DuplicateToolResult` `AgentEvent`, and fans the
    /// event into the rest of the chain.
    pub fn take_pending(&mut self) -> Vec<DuplicateDetection> {
        std::mem::take(&mut self.pending)
    }

    /// Read-only count of `ResultHash` entries currently tracked. Tests
    /// observe state-reset semantics through this without depending on
    /// the internal `HashMap`.
    pub fn seen_len(&self) -> usize {
        self.seen.len()
    }

    /// Observe a tool result whose canonical fingerprint was computed
    /// by the conversation loop's shared hashing pass.
    pub fn observe_tool_result(&mut self, id: &ToolCallId, fingerprint: ResultFingerprint) {
        let bytes = fingerprint.canonical_len();
        if usize::try_from(self.min_bytes).map_or(true, |min_bytes| bytes < min_bytes) {
            return;
        }
        let hash = fingerprint.hash();
        if let Some(original) = self.seen.get(&hash) {
            self.pending.push(DuplicateDetection {
                original_call_id: original.clone(),
                repeated_call_id: id.clone(),
                bytes_wasted: u64::try_from(bytes).unwrap_or(u64::MAX),
            });
        } else {
            self.seen.insert(hash, id.clone());
        }
    }
}

impl Default for DuplicateResultObserver {
    fn default() -> Self {
        Self::new()
    }
}

impl EventSink for DuplicateResultObserver {
    fn emit(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::CompactionEnd { .. } => {
                self.seen.clear();
            }
            AgentEvent::ToolResult { id, output, .. } => {
                match ResultHasher::output_fingerprint(output) {
                    Ok(fingerprint) => self.observe_tool_result(id, fingerprint),
                    Err(error) => {
                        tracing::warn!(
                            tool_call_id = %id,
                            error = ?error,
                            "duplicate-result observer skipped uncanonicalizable payload"
                        );
                    }
                }
            }
            _ => {}
        }
    }

    fn react(&mut self) -> Vec<SessionCommand> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use loom_events::event::{EventEnvelope, Source};
    use loom_events::identifier::{BeadId, SessionId};
    use serde_json::Value;

    fn envelope(seq: u64) -> EventEnvelope {
        EventEnvelope {
            session_id: SessionId::new("sess-duplicate").unwrap(),
            bead_id: Some(BeadId::new("lm-test").expect("valid bead id")),
            molecule_id: None,
            iteration: Some(0),
            source: Source::Agent,
            ts_ms: i64::try_from(seq).expect("test sequence fits i64"),
            seq,
        }
    }

    fn tool_result(seq: u64, id: &str, output: &str) -> AgentEvent {
        AgentEvent::ToolResult {
            envelope: envelope(seq),
            id: ToolCallId::new(id).unwrap(),
            output: output.to_owned(),
            is_error: false,
        }
    }

    fn compaction_end(seq: u64) -> AgentEvent {
        AgentEvent::CompactionEnd {
            envelope: envelope(seq),
            aborted: false,
        }
    }

    fn big_payload(tag: &str) -> String {
        let filler = "x".repeat(512);
        format!("{{\"tag\":\"{tag}\",\"filler\":\"{filler}\"}}")
    }

    #[test]
    fn duplicate_result_react_always_returns_empty() {
        let mut obs = DuplicateResultObserver::new();
        let payload = big_payload("a");
        obs.emit(&tool_result(0, "call-1", &payload));
        obs.emit(&tool_result(1, "call-2", &payload));
        assert!(obs.react().is_empty());
        let _ = obs.take_pending();
        assert!(obs.react().is_empty());
    }

    #[test]
    fn duplicate_result_first_seen_wins_subsequent_emit() {
        let mut obs = DuplicateResultObserver::new();
        let payload = big_payload("dup");
        obs.emit(&tool_result(0, "call-1", &payload));
        obs.emit(&tool_result(1, "call-2", &payload));
        obs.emit(&tool_result(2, "call-3", &payload));
        let detections = obs.take_pending();
        assert_eq!(detections.len(), 2);
        assert_eq!(detections[0].original_call_id.as_str(), "call-1");
        assert_eq!(detections[0].repeated_call_id.as_str(), "call-2");
        assert_eq!(detections[1].original_call_id.as_str(), "call-1");
        assert_eq!(detections[1].repeated_call_id.as_str(), "call-3");
    }

    #[test]
    fn duplicate_result_ignores_payloads_below_min_bytes() {
        let mut obs = DuplicateResultObserver::new().with_min_bytes(1024);
        let payload = big_payload("a");
        obs.emit(&tool_result(0, "call-1", &payload));
        obs.emit(&tool_result(1, "call-2", &payload));
        assert!(obs.take_pending().is_empty());
        assert_eq!(obs.seen_len(), 0);
    }

    #[test]
    fn duplicate_result_detection_payload_carries_bytes_wasted() {
        let mut obs = DuplicateResultObserver::new();
        let payload = big_payload("size-check");
        let canonical_len = ResultHasher::canonical_len(
            &serde_json::from_str::<Value>(&payload).expect("payload parses"),
        )
        .expect("payload canonicalizes");
        obs.emit(&tool_result(0, "first", &payload));
        obs.emit(&tool_result(1, "second", &payload));
        let detections = obs.take_pending();
        assert_eq!(detections.len(), 1);
        assert_eq!(detections[0].bytes_wasted, canonical_len as u64);
    }

    #[test]
    fn detection_keys_on_canonical_payload_not_string_form() {
        let mut obs = DuplicateResultObserver::new();
        let a = format!("{{\"a\":1,\"b\":2,\"filler\":\"{}\"}}", "y".repeat(512));
        let b = format!("{{\"b\":2,\"a\":1,\"filler\":\"{}\"}}", "y".repeat(512));
        obs.emit(&tool_result(0, "call-1", &a));
        obs.emit(&tool_result(1, "call-2", &b));
        let detections = obs.take_pending();
        assert_eq!(detections.len(), 1);
        assert_eq!(detections[0].original_call_id.as_str(), "call-1");
        assert_eq!(detections[0].repeated_call_id.as_str(), "call-2");
    }

    #[test]
    fn distinct_payloads_do_not_dedup() {
        let mut obs = DuplicateResultObserver::new();
        obs.emit(&tool_result(0, "call-1", &big_payload("a")));
        obs.emit(&tool_result(1, "call-2", &big_payload("b")));
        assert!(obs.take_pending().is_empty());
        assert_eq!(obs.seen_len(), 2);
    }

    #[test]
    fn duplicate_result_resets_on_compaction_end() {
        let mut obs = DuplicateResultObserver::new();
        let payload = big_payload("c");
        obs.emit(&tool_result(0, "call-1", &payload));
        assert_eq!(obs.seen_len(), 1);
        obs.emit(&compaction_end(1));
        assert_eq!(obs.seen_len(), 0);
        obs.emit(&tool_result(2, "call-2", &payload));
        assert!(
            obs.take_pending().is_empty(),
            "post-compaction repeat must be treated as the new first-seen",
        );
    }

    #[test]
    fn non_json_output_still_dedups_as_string_value() {
        let mut obs = DuplicateResultObserver::new();
        let big_text = "z".repeat(512);
        obs.emit(&tool_result(0, "call-1", &big_text));
        obs.emit(&tool_result(1, "call-2", &big_text));
        let detections = obs.take_pending();
        assert_eq!(detections.len(), 1);
        assert_eq!(detections[0].original_call_id.as_str(), "call-1");
        assert_eq!(detections[0].repeated_call_id.as_str(), "call-2");
    }

    #[test]
    fn ignores_unrelated_event_kinds() {
        let mut obs = DuplicateResultObserver::new();
        obs.emit(&AgentEvent::TurnEnd {
            envelope: envelope(0),
        });
        obs.emit(&AgentEvent::TextDelta {
            envelope: envelope(1),
            text: "x".repeat(1024),
        });
        assert_eq!(obs.seen_len(), 0);
        assert!(obs.take_pending().is_empty());
    }
}

//! Shared helpers for appending driver-side merge/push/cleanup events
//! into the per-bead JSONL the spawn closure already wrote to.
//!
//! Both the sequential `ProductionAgentLoopController` and the
//! parallel `merge_back_one` path need to land driver events
//! (`bead_branch_pushed`, `merge_ok`, …) in the same log file the
//! agent's session events live in so operators tailing the loop see
//! the dispatch-to-dispatch gap as named steps. The spawn closure
//! picks its own per-attempt `when` timestamp internally; this module
//! recovers the path by most-recently-modified mtime rather than
//! reconstructing it from `(label, bead_id, when)`.

use std::path::{Path, PathBuf};

use loom_driver::clock::{Clock, SystemClock};
use loom_driver::logging::{BeadOutcome, LogSink};
use loom_events::identifier::{BeadId, SessionId, SpecLabel};
use loom_events::{AgentEvent, DriverKind, EnvelopeBuilder, SessionScope, Source};
use tracing::warn;

/// Per-bead emit target: the resolved log path plus a `Source::Driver`
/// envelope builder whose `seq` advances across every driver event
/// emitted during this attempt.
pub struct BeadEmit {
    pub log_path: PathBuf,
    pub builder: EnvelopeBuilder,
}

impl BeadEmit {
    /// Construct an emit target for `bead_id` against the JSONL file
    /// the spawn closure just finished writing to. Returns `None`
    /// when no matching `<bead_id>-*.jsonl` is present under
    /// `<logs_root>/<label>/` — the controller falls back to a silent
    /// no-op rather than fabricating a path that no consumer will
    /// tail.
    ///
    /// The envelope builder's `seq` counter resumes from the spawn
    /// closure's last event so SSE consumers tracking `(session_id,
    /// seq)` see one strictly-increasing per-spawn stream rather
    /// than two overlapping ones — the read-then-seed costs one
    /// short file read at the closure-controller boundary in
    /// exchange for the wire contract holding.
    pub fn for_bead(logs_root: &Path, label: &SpecLabel, bead_id: &BeadId) -> Option<Self> {
        let log_path = find_latest_bead_log(logs_root, label, bead_id)?;
        let resume_seq = max_seq_in_log(&log_path).map_or(0, |s| s + 1);
        let session_id = session_id_in_log(&log_path).unwrap_or_else(|| {
            SessionId::new(format!("{}-append", bead_id.as_str().replace('.', "-")))
        });
        let clock = SystemClock::new();
        let builder = EnvelopeBuilder::with_seq_start(
            SessionScope::bead(session_id, bead_id.clone(), None, 0),
            Source::Driver,
            resume_seq,
            move || {
                clock
                    .wall_now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |duration| duration.as_millis() as i64)
            },
        );
        Some(Self { log_path, builder })
    }

    /// Append one driver event using the stored envelope builder so
    /// `seq` advances by 1 per emission across the attempt's
    /// merge/push/cleanup window.
    pub fn emit(&mut self, kind: DriverKind, summary: &str, payload: serde_json::Value) {
        let envelope = self.builder.build();
        let event = AgentEvent::DriverEvent {
            envelope,
            driver_kind: kind,
            summary: summary.to_string(),
            payload,
        };
        match LogSink::open_at_path_append(&self.log_path) {
            Ok(mut sink) => {
                if let Err(e) = sink.emit(&event) {
                    warn!(
                        error = %e,
                        path = %self.log_path.display(),
                        "driver event emit failed",
                    );
                }
                if let Err(e) = sink.finish(BeadOutcome::Done) {
                    warn!(
                        error = %e,
                        path = %self.log_path.display(),
                        "driver event sink finish failed",
                    );
                }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    path = %self.log_path.display(),
                    "driver event sink open failed",
                );
            }
        }
    }
}

/// Scan `log_path` for the largest `seq` value across all JSONL
/// lines. Used to resume the driver-event envelope builder so
/// driver-side emissions continue the spawn closure's per-spawn
/// counter rather than restarting at zero.
fn max_seq_in_log(log_path: &Path) -> Option<u64> {
    let body = std::fs::read_to_string(log_path).ok()?;
    body.lines()
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
            value.get("seq").and_then(|v| v.as_u64())
        })
        .max()
}

fn session_id_in_log(log_path: &Path) -> Option<SessionId> {
    let body = match std::fs::read_to_string(log_path) {
        Ok(body) => body,
        Err(e) => {
            warn!(
                error = %e,
                path = %log_path.display(),
                "driver event session id scan failed",
            );
            return None;
        }
    };
    body.lines().find_map(|line| {
        let value = match serde_json::from_str::<serde_json::Value>(line.trim()) {
            Ok(value) => value,
            Err(_) => return None,
        };
        let raw = value.get("session_id")?.as_str()?;
        let Ok(session_id) = raw.parse::<SessionId>() else {
            return None;
        };
        Some(session_id)
    })
}

fn find_latest_bead_log(logs_root: &Path, label: &SpecLabel, bead_id: &BeadId) -> Option<PathBuf> {
    let dir = logs_root.join(label.as_str());
    let prefix = format!("{}-", bead_id.as_str());
    let entries = std::fs::read_dir(&dir).ok()?;
    let mut best: Option<(PathBuf, std::time::SystemTime)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !name_str.starts_with(&prefix) || !name_str.ends_with(".jsonl") {
            continue;
        }
        let mtime = match entry.metadata().and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => continue,
        };
        match &best {
            Some((_, prev)) if mtime <= *prev => continue,
            _ => best = Some((entry.path(), mtime)),
        }
    }
    best.map(|(p, _)| p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn find_latest_picks_most_recently_modified_matching_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let logs_root = dir.path();
        let label = SpecLabel::new("alpha");
        let bead = BeadId::new("lm-1").expect("bead id");
        let spec_dir = logs_root.join("alpha");
        fs::create_dir_all(&spec_dir).expect("mkdir");
        let old = spec_dir.join("lm-1-19700101T000000Z.jsonl");
        let new = spec_dir.join("lm-1-19700101T000100Z.jsonl");
        let other = spec_dir.join("lm-2-19700101T000100Z.jsonl");
        // Backdate the older files to UNIX_EPOCH so the mtime
        // ordering is deterministic without relying on filesystem
        // clock granularity or a real-time sleep.
        for path in [&old, &other] {
            fs::write(path, "").unwrap();
            std::fs::File::open(path)
                .expect("open for set_modified")
                .set_modified(std::time::UNIX_EPOCH)
                .expect("backdate mtime");
        }
        fs::write(&new, "").unwrap();
        let found = find_latest_bead_log(logs_root, &label, &bead).expect("found");
        assert_eq!(found, new);
    }

    #[test]
    fn find_latest_returns_none_when_no_match() {
        let dir = tempfile::tempdir().expect("tempdir");
        let label = SpecLabel::new("alpha");
        let bead = BeadId::new("lm-x").expect("bead id");
        let spec_dir = dir.path().join("alpha");
        fs::create_dir_all(&spec_dir).expect("mkdir");
        fs::write(spec_dir.join("lm-other-19700101T000000Z.jsonl"), "").unwrap();
        assert!(find_latest_bead_log(dir.path(), &label, &bead).is_none());
    }
}

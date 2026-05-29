use std::path::{Path, PathBuf};
use std::time::SystemTime;

use loom_events::identifier::{BeadId, SpecLabel};

pub use super::time::format_utc_timestamp;

/// Resolve the per-bead JSONL log path under
/// `<logs_root>/<spec-label>/<bead-id>-<utc-timestamp>.jsonl`.
///
/// `logs_root` is typically `<workspace>/.loom/logs`. Per-bead — not
/// per-session — so concurrent batches never interleave inside a single file
/// (see `specs/harness.md` *Run UX & Logging*).
///
/// The function is pure: it does not create directories or files. Callers
/// (`LogSink::open_in`) handle directory creation.
///
/// ```
/// use loom_events::identifier::{BeadId, SpecLabel};
/// use loom_render::bead_log_path;
/// use std::path::Path;
/// use std::time::{Duration, UNIX_EPOCH};
///
/// let path = bead_log_path(
///     Path::new("/ws/.loom/logs"),
///     &SpecLabel::new("harness"),
///     &BeadId::new("lm-3hhwq.9").unwrap(),
///     UNIX_EPOCH + Duration::from_secs(1777811445),
/// );
/// assert_eq!(
///     path,
///     Path::new("/ws/.loom/logs/harness/lm-3hhwq.9-20260503T123045Z.jsonl"),
/// );
/// ```
pub fn bead_log_path(
    logs_root: &Path,
    spec_label: &SpecLabel,
    bead_id: &BeadId,
    when: SystemTime,
) -> PathBuf {
    let stamp = format_utc_timestamp(when);
    logs_root
        .join(spec_label.as_str())
        .join(format!("{}-{}.jsonl", bead_id.as_str(), stamp))
}

/// Resolve the per-phase JSONL log path under
/// `<logs_root>/<spec-label>/<phase>-<utc-timestamp>.jsonl`.
///
/// `loom todo`, `loom plan`, and `loom review` operate against a spec rather
/// than a single bead, so their event streams live alongside per-bead logs
/// under the same `<spec-label>/` directory but use the phase name as the
/// file-stem prefix. The function is pure: callers handle directory creation
/// (see [`crate::logging::LogSink::open_phase_at`]).
pub fn phase_log_path(
    logs_root: &Path,
    spec_label: &SpecLabel,
    phase: &str,
    when: SystemTime,
) -> PathBuf {
    let stamp = format_utc_timestamp(when);
    logs_root
        .join(spec_label.as_str())
        .join(format!("{phase}-{stamp}.jsonl"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn nests_under_spec_label_and_includes_utc_stamp() {
        let path = bead_log_path(
            Path::new("/x/.loom/logs"),
            &SpecLabel::new("alpha"),
            &BeadId::new("lm-1").expect("valid bead id"),
            UNIX_EPOCH + Duration::from_secs(0),
        );
        assert_eq!(
            path,
            Path::new("/x/.loom/logs/alpha/lm-1-19700101T000000Z.jsonl"),
        );
    }

    #[test]
    fn distinct_spec_labels_yield_distinct_directories() {
        let root = Path::new("/r");
        let when = UNIX_EPOCH + Duration::from_secs(1777811445);
        let bead = BeadId::new("lm-1").expect("valid bead id");
        let p_a = bead_log_path(root, &SpecLabel::new("a"), &bead, when);
        let p_b = bead_log_path(root, &SpecLabel::new("b"), &bead, when);
        assert_ne!(p_a.parent(), p_b.parent());
    }

    #[test]
    fn distinct_beads_in_same_spec_yield_distinct_files() {
        let root = Path::new("/r");
        let when = UNIX_EPOCH + Duration::from_secs(1777811445);
        let label = SpecLabel::new("a");
        let bead_a = BeadId::new("lm-1").expect("valid bead id");
        let bead_b = BeadId::new("lm-2").expect("valid bead id");
        let p_a = bead_log_path(root, &label, &bead_a, when);
        let p_b = bead_log_path(root, &label, &bead_b, when);
        assert_eq!(p_a.parent(), p_b.parent());
        assert_ne!(p_a.file_name(), p_b.file_name());
    }

    #[test]
    fn phase_log_path_uses_phase_name_as_file_stem_prefix() {
        let path = phase_log_path(
            Path::new("/x/.loom/logs"),
            &SpecLabel::new("alpha"),
            "todo",
            UNIX_EPOCH + Duration::from_secs(1777811445),
        );
        assert_eq!(
            path,
            Path::new("/x/.loom/logs/alpha/todo-20260503T123045Z.jsonl"),
        );
    }

    #[test]
    fn phase_log_path_nests_under_spec_label() {
        let p = phase_log_path(
            Path::new("/r"),
            &SpecLabel::new("harness"),
            "check",
            UNIX_EPOCH,
        );
        assert_eq!(p.parent(), Some(Path::new("/r/harness")));
    }
}

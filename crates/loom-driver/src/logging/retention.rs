use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tracing::debug;

/// Outcome of one [`sweep_retention`] invocation. Caller is welcome to
/// discard it; the sweep itself is best-effort and tolerates errors.
#[derive(Debug, Default, Clone)]
pub struct RetentionReport {
    pub deleted: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, std::io::ErrorKind)>,
    pub skipped_recent: u32,
}

/// Walk every regular file under `logs_root` once and delete any whose mtime
/// is older than `retention_days` days. Best-effort: per-file failures are
/// logged at `debug!` and accumulated into [`RetentionReport::failed`] but do
/// not abort the sweep.
///
/// `retention_days = 0` disables sweeping entirely (returns an empty report).
/// A missing `logs_root` is treated as "nothing to sweep".
///
/// `now` is supplied by the caller — production code obtains it once at the
/// binary entry point (via the wall clock) and passes it down; tests pass
/// synthetic timestamps to express "this file is N days old" without aging
/// real time.
///
/// Spec NF-10 forbids ignoring errors silently; the report carries every
/// failure and the caller may surface counts at `info!`. The function never
/// returns `Err` — it returns `Ok` with a report describing what happened.
pub fn sweep_retention_at(
    logs_root: &Path,
    retention_days: u32,
    now: SystemTime,
) -> RetentionReport {
    let mut report = RetentionReport::default();
    if retention_days == 0 {
        debug!(
            target: "loom_driver::logging::retention",
            "retention_days = 0 — sweep disabled"
        );
        return report;
    }
    if !logs_root.exists() {
        return report;
    }
    let cutoff = now.checked_sub(Duration::from_secs(retention_days as u64 * 86_400));
    let cutoff = match cutoff {
        Some(c) => c,
        None => {
            debug!(
                target: "loom_driver::logging::retention",
                retention_days,
                "cutoff would precede UNIX epoch — skipping sweep"
            );
            return report;
        }
    };
    walk_and_sweep(logs_root, cutoff, &mut report);
    debug!(
        target: "loom_driver::logging::retention",
        deleted = report.deleted.len(),
        failed = report.failed.len(),
        skipped_recent = report.skipped_recent,
        "log retention sweep complete"
    );
    report
}

fn walk_and_sweep(dir: &Path, cutoff: SystemTime, report: &mut RetentionReport) {
    let entries = match std::fs::read_dir(dir) {
        Ok(it) => it,
        Err(e) => {
            debug!(
                target: "loom_driver::logging::retention",
                dir = %dir.display(),
                kind = ?e.kind(),
                "read_dir failed; skipping subtree"
            );
            report.failed.push((dir.to_path_buf(), e.kind()));
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(e) => {
                report.failed.push((path, e.kind()));
                continue;
            }
        };
        if file_type.is_dir() {
            walk_and_sweep(&path, cutoff, report);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let mtime = match entry.metadata().and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(e) => {
                report.failed.push((path, e.kind()));
                continue;
            }
        };
        if mtime >= cutoff {
            report.skipped_recent += 1;
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => report.deleted.push(path),
            Err(e) => {
                debug!(
                    target: "loom_driver::logging::retention",
                    path = %path.display(),
                    kind = ?e.kind(),
                    "delete failed — best-effort sweep continues"
                );
                report.failed.push((path, e.kind()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    fn touch(path: &Path, body: &str) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).expect("mkdir");
        }
        fs::write(path, body).expect("write");
    }

    fn set_mtime(path: &Path, when: SystemTime) {
        let f = fs::File::options()
            .write(true)
            .open(path)
            .expect("open for mtime");
        f.set_modified(when).expect("set_modified");
    }

    #[test]
    fn deletes_files_older_than_cutoff() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let old = now - Duration::from_secs(20 * 86_400);
        let recent = now - Duration::from_secs(2 * 86_400);

        let p_old = dir.path().join("alpha/wx-1-old.jsonl");
        let p_recent = dir.path().join("alpha/wx-2-new.jsonl");
        touch(&p_old, "old");
        touch(&p_recent, "new");
        set_mtime(&p_old, old);
        set_mtime(&p_recent, recent);

        let report = sweep_retention_at(dir.path(), 14, now);
        assert_eq!(report.deleted.len(), 1, "{report:?}");
        assert!(!p_old.exists(), "old file should be deleted");
        assert!(p_recent.exists(), "recent file must survive");
        assert_eq!(report.skipped_recent, 1);
    }

    #[test]
    fn zero_disables_sweep() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let very_old = now - Duration::from_secs(365 * 86_400);
        let p = dir.path().join("a/wx-1.jsonl");
        touch(&p, "ancient");
        set_mtime(&p, very_old);

        let report = sweep_retention_at(dir.path(), 0, now);
        assert_eq!(report.deleted.len(), 0);
        assert!(p.exists(), "retention_days=0 must not delete");
    }

    #[test]
    fn missing_root_is_no_op() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("does/not/exist");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let report = sweep_retention_at(&missing, 14, now);
        assert!(report.deleted.is_empty());
        assert!(report.failed.is_empty());
    }

    #[test]
    fn descends_into_nested_spec_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let old = now - Duration::from_secs(60 * 86_400);
        for label in ["alpha", "beta", "gamma"] {
            let p = dir.path().join(format!("{label}/wx-{label}.jsonl"));
            touch(&p, "x");
            set_mtime(&p, old);
        }
        let report = sweep_retention_at(dir.path(), 14, now);
        assert_eq!(report.deleted.len(), 3, "{report:?}");
    }
}

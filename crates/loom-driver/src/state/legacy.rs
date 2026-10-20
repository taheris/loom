//! One-shot import of retired local stores; no imported record grants current admission.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, params};
use serde_json::Value;

use super::{CacheError, RecordKind};

pub(super) struct Import {
    files: Vec<PathBuf>,
    directories: Vec<PathBuf>,
}

impl Import {
    pub(super) fn finish(self) {
        for path in self.files {
            if let Err(error) = std::fs::remove_file(&path) {
                tracing::warn!(path = %path.display(), ?error, "legacy state was imported but its retired file could not be removed");
            }
        }
        for path in self.directories.into_iter().rev() {
            match std::fs::remove_dir(&path) {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                    ) => {}
                Err(error) => {
                    tracing::warn!(path = %path.display(), ?error, "retired state directory could not be removed");
                }
            }
        }
    }
}

pub(super) fn pending(database: &Path) -> Result<bool, CacheError> {
    let Some(workspace) = database
        .parent()
        .filter(|parent| parent.file_name().is_some_and(|name| name == ".loom"))
        .and_then(Path::parent)
    else {
        return Ok(false);
    };
    if workspace.join(".loom/cache.db").try_exists()? {
        return Ok(true);
    }
    for name in [
        ".loom/decision-waits",
        ".loom/decision-waits/dispositions",
        ".loom/decision-waits/migrations",
        ".loom/dependency-waits",
        ".git/loom-verification-continuations",
    ] {
        let directory = directory(workspace, name)?;
        if directory.try_exists()? {
            for entry in std::fs::read_dir(directory)? {
                if entry?
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "json")
                {
                    return Ok(true);
                }
            }
        }
    }
    let proposals = workspace.join(".loom/tune");
    if proposals.try_exists()? {
        for entry in std::fs::read_dir(proposals)? {
            if entry?.path().join("manifest.json").try_exists()? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub(super) fn import(conn: &Connection, database: &Path) -> Result<Import, CacheError> {
    let mut imported = Import {
        files: Vec::new(),
        directories: Vec::new(),
    };
    let Some(workspace) = database
        .parent()
        .filter(|parent| parent.file_name().is_some_and(|name| name == ".loom"))
        .and_then(Path::parent)
    else {
        return Ok(imported);
    };
    let legacy = workspace.join(".loom/cache.db");
    if legacy.try_exists()? {
        let old = Connection::open_with_flags(&legacy, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        copy_notes(&old, conn)?;
        drop(old);
        for suffix in ["", "-wal", "-shm", ".lock"] {
            let mut path = legacy.as_os_str().to_os_string();
            path.push(suffix);
            let path = PathBuf::from(path);
            if path.try_exists()? {
                imported.files.push(path);
            }
        }
    }
    for (directory, kind, field) in [
        (
            ".loom/decision-waits",
            RecordKind::DecisionInventory,
            "record.source",
        ),
        (
            ".loom/dependency-waits",
            RecordKind::DependencyWait,
            "record.source",
        ),
        (
            ".loom/decision-waits/dispositions",
            RecordKind::DecisionDisposition,
            "filename",
        ),
        (
            ".loom/decision-waits/migrations",
            RecordKind::DecisionMigration,
            "before.id",
        ),
        (
            ".git/loom-verification-continuations",
            RecordKind::Verification,
            "bead",
        ),
    ] {
        let directory = self::directory(workspace, directory)?;
        if !directory.try_exists()? {
            continue;
        }
        imported.directories.push(directory.clone());
        let mut paths = std::fs::read_dir(&directory)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort();
        for path in paths {
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            if !std::fs::symlink_metadata(&path)?.file_type().is_file() {
                return Err(invalid(&path, "legacy record is not a regular file"));
            }
            let value: Value =
                serde_json::from_slice(&std::fs::read(&path)?).map_err(|source| {
                    CacheError::Json {
                        column: "legacy record",
                        source,
                    }
                })?;
            let key = if field == "filename" {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .ok_or_else(|| invalid(&path, "invalid record filename"))?
            } else {
                let mut item = &value;
                for part in field.split('.') {
                    item = item
                        .get(part)
                        .ok_or_else(|| invalid(&path, "record lacks its task identity"))?;
                }
                item.as_str()
                    .ok_or_else(|| invalid(&path, "record task identity is not a string"))?
            };
            if field != "filename" {
                key.parse::<crate::identifier::BeadId>()
                    .map_err(|_| invalid(&path, "invalid task identity"))?;
            }
            conn.execute(
                "INSERT INTO workflow_records(kind, record_key, payload) VALUES (?1, ?2, ?3)
                 ON CONFLICT(kind, record_key) DO UPDATE SET payload = excluded.payload",
                params![
                    kind.as_str(),
                    key,
                    serde_json::to_string(&value).map_err(|source| CacheError::Json {
                        column: "legacy record",
                        source
                    })?
                ],
            )?;
            imported.files.push(path);
        }
    }
    let proposals = workspace.join(".loom/tune");
    if proposals.try_exists()? {
        for entry in std::fs::read_dir(proposals)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path().join("manifest.json");
            if !path.try_exists()? {
                continue;
            }
            if !std::fs::symlink_metadata(&path)?.file_type().is_file() {
                return Err(invalid(&path, "tune metadata is not a regular file"));
            }
            let value: Value =
                serde_json::from_slice(&std::fs::read(&path)?).map_err(|source| {
                    CacheError::Json {
                        column: "legacy tune metadata",
                        source,
                    }
                })?;
            let key = value
                .get("proposal_id")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid(&path, "missing proposal identity"))?;
            if entry.file_name() != key || key.parse::<crate::identifier::BeadId>().is_err() {
                return Err(invalid(&path, "tune envelope identity disagrees"));
            }
            conn.execute("INSERT INTO workflow_records(kind,record_key,payload) VALUES (?1,?2,?3) ON CONFLICT(kind,record_key) DO NOTHING", params![RecordKind::Tune.as_str(),key,value.to_string()])?;
            imported.files.push(path);
            // These obsolete directories were never payload destinations; remove only if empty.
            imported.directories.push(entry.path().join("logs"));
            imported.directories.push(entry.path().join("evidence"));
        }
    }
    // Old receipts and verifier caches cannot establish fresh coverage.
    for name in [".loom/marker.json", ".loom/gate-cache.sqlite"] {
        let path = workspace.join(name);
        if path.try_exists()? && std::fs::symlink_metadata(&path)?.file_type().is_file() {
            imported.files.push(path);
        }
    }
    Ok(imported)
}

pub(super) fn copy_notes(source: &Connection, destination: &Connection) -> Result<(), CacheError> {
    let mut query =
        source.prepare("SELECT spec_label, kind, text, created_at FROM notes ORDER BY id")?;
    let notes = query.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    for note in notes {
        let (label, kind, text, created_at) = note?;
        destination.execute(
            "INSERT OR IGNORE INTO specs(label, spec_path) VALUES (?1, ?2)",
            params![label, format!("specs/{label}.md")],
        )?;
        destination.execute(
            "INSERT INTO notes(spec_label, kind, text, created_at)
             SELECT ?1, ?2, ?3, ?4 WHERE NOT EXISTS (SELECT 1 FROM notes WHERE spec_label = ?1 AND kind = ?2 AND text = ?3 AND created_at = ?4)",
            params![label, kind, text, created_at],
        )?;
    }
    Ok(())
}

fn directory(workspace: &Path, name: &str) -> Result<PathBuf, CacheError> {
    if let Some(relative) = name.strip_prefix(".git/")
        && workspace.join(".git").is_file()
    {
        return Ok(crate::git::GitClient::open(workspace)?
            .administrative_directory()
            .join(relative));
    }
    Ok(workspace.join(name))
}

fn invalid(path: &Path, detail: &str) -> CacheError {
    CacheError::Legacy {
        path: path.to_path_buf(),
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::CacheDb;

    #[test]
    fn migration_commits_before_retiring_sources_and_preserves_notes() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join(".loom");
        std::fs::create_dir_all(runtime.join("dependency-waits")).unwrap();
        let legacy = runtime.join("cache.db");
        let db = CacheDb::open(&legacy).unwrap();
        db.notes_add(
            &"harness".parse().unwrap(),
            "implementation",
            "retained guidance",
            123,
        )
        .unwrap();
        drop(db);
        let record = serde_json::json!({"record":{"source":"lm-wait"},"phase":"Active"});
        std::fs::write(
            runtime.join("dependency-waits/old.json"),
            record.to_string(),
        )
        .unwrap();
        let path = crate::state::path(root.path());
        let manager = crate::lock::LockManager::new(root.path()).unwrap();
        let guard = manager.acquire_workspace().unwrap();
        let db = CacheDb::open_for_init(&path, &guard).unwrap();
        assert_eq!(
            db.record::<Value>(RecordKind::DependencyWait, "lm-wait")
                .unwrap(),
            Some(record)
        );
        assert!(!legacy.exists());
        assert!(!runtime.join("dependency-waits/old.json").exists());
        assert_eq!(
            db.notes_list(Some(&"harness".parse().unwrap()), Some("implementation"))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn retired_lock_and_interrupted_files_do_not_require_reimport() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join(".git/loom-verification-continuations");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(".lock"), "").unwrap();
        std::fs::write(directory.join(".pending-interrupted"), "incomplete").unwrap();
        std::fs::write(
            directory.join("00000000000000000001.json"),
            r#"{"bead":"lm-recover","phase":"verify"}"#,
        )
        .unwrap();
        let manager = crate::lock::LockManager::new(root.path()).unwrap();
        let guard = manager.acquire_workspace().unwrap();
        let path = crate::state::path(root.path());
        drop(CacheDb::open_for_init(&path, &guard).unwrap());
        drop(guard);
        let db = CacheDb::open(&path).unwrap();
        assert!(
            db.record::<Value>(RecordKind::Verification, "lm-recover")
                .unwrap()
                .is_some()
        );
        assert!(directory.join(".pending-interrupted").exists());
    }

    #[test]
    fn invalid_import_keeps_original_files_and_database_for_retry() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join(".loom");
        std::fs::create_dir_all(runtime.join("dependency-waits")).unwrap();
        drop(CacheDb::open(runtime.join("cache.db")).unwrap());
        let record = runtime.join("dependency-waits/broken.json");
        std::fs::write(&record, "incomplete").unwrap();
        let manager = crate::lock::LockManager::new(root.path()).unwrap();
        let guard = manager.acquire_workspace().unwrap();
        assert!(CacheDb::open_for_init(crate::state::path(root.path()), &guard).is_err());
        assert!(record.exists());
        assert!(runtime.join("cache.db").exists());
    }
}

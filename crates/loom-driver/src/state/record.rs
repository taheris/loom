//! Transient workflow records. Decoding never grants workflow or evidence authority.

use rusqlite::{OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};

use super::{CacheDb, CacheError};

/// Closed set of local workflow record owners.
#[derive(Debug, Clone, Copy)]
pub enum RecordKind {
    CargoTarget,
    DecisionInventory,
    DependencyWait,
    DecisionDisposition,
    DecisionMigration,
    Verification,
    PublicationReceipt,
    Tune,
}

impl RecordKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::CargoTarget => "cargo-target",
            Self::DecisionInventory => "decision-inventory",
            Self::DependencyWait => "dependency-wait",
            Self::DecisionDisposition => "decision-disposition",
            Self::DecisionMigration => "decision-migration",
            Self::Verification => "verification",
            Self::PublicationReceipt => "publication-receipt",
            Self::Tune => "tune",
        }
    }
}

impl CacheDb {
    /// Read a local record; its owner must check current canonical inputs before use.
    ///
    /// # Errors
    /// Reports database and decoding failures, never an empty record on corruption.
    pub fn record<T: DeserializeOwned>(
        &self,
        kind: RecordKind,
        key: &str,
    ) -> Result<Option<T>, CacheError> {
        self.with_conn(|conn| {
            let value: Option<String> = conn
                .query_row(
                    "SELECT payload FROM workflow_records WHERE kind = ?1 AND record_key = ?2",
                    params![kind.as_str(), key],
                    |row| row.get(0),
                )
                .optional()?;
            value.map(|value| decode(&value)).transpose()
        })
    }

    /// Atomically replace a local operation snapshot.
    ///
    /// # Errors
    /// Reports encoding or database failures without partially replacing the record.
    pub fn put_record<T: Serialize>(
        &self,
        kind: RecordKind,
        key: &str,
        value: &T,
    ) -> Result<(), CacheError> {
        let value = serde_json::to_string(value).map_err(json_error)?;
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO workflow_records(kind, record_key, payload) VALUES (?1, ?2, ?3)
                 ON CONFLICT(kind, record_key) DO UPDATE SET payload = excluded.payload",
                params![kind.as_str(), key, value],
            )?;
            Ok(())
        })
    }

    /// Read one owner's snapshots in deterministic key order.
    ///
    /// # Errors
    /// Reports corrupt records and database failures.
    pub fn records<T: DeserializeOwned>(
        &self,
        kind: RecordKind,
    ) -> Result<Vec<(String, T)>, CacheError> {
        self.with_conn(|conn| {
            let mut query = conn.prepare(
                "SELECT record_key, payload FROM workflow_records WHERE kind = ?1 ORDER BY record_key",
            )?;
            let rows = query.query_map([kind.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut values = Vec::new();
            for row in rows {
                let (key, value) = row?;
                values.push((key, decode(&value)?));
            }
            Ok(values)
        })
    }

    /// Invalidate a transient record, without changing its canonical task or Git inputs.
    ///
    /// # Errors
    /// Reports database failures.
    pub fn remove_record(&self, kind: RecordKind, key: &str) -> Result<(), CacheError> {
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM workflow_records WHERE kind = ?1 AND record_key = ?2",
                params![kind.as_str(), key],
            )?;
            Ok(())
        })
    }
}

fn decode<T: DeserializeOwned>(value: &str) -> Result<T, CacheError> {
    serde_json::from_str(value).map_err(json_error)
}

const fn json_error(source: serde_json::Error) -> CacheError {
    CacheError::Json {
        column: "workflow_records.payload",
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_records_survive_restart_but_not_explicit_reset() {
        let root = tempfile::tempdir().unwrap();
        let path = super::super::path(root.path());
        {
            let db = CacheDb::open(&path).unwrap();
            db.put_record(RecordKind::Verification, "lm-work", &vec!["candidate"])
                .unwrap();
        }
        {
            let db = CacheDb::open(&path).unwrap();
            assert_eq!(
                db.record::<Vec<String>>(RecordKind::Verification, "lm-work")
                    .unwrap(),
                Some(vec!["candidate".into()])
            );
        }
        let db = CacheDb::recreate(&path).unwrap();
        assert!(
            db.record::<Vec<String>>(RecordKind::Verification, "lm-work")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn reset_refuses_live_connections_instead_of_unlinking_their_database() {
        let root = tempfile::tempdir().unwrap();
        let path = super::super::path(root.path());
        let db = CacheDb::open(&path).unwrap();
        db.put_record(RecordKind::Tune, "lm-tune", &"candidate")
            .unwrap();
        assert!(CacheDb::recreate(&path).is_err());
        assert_eq!(
            db.record::<String>(RecordKind::Tune, "lm-tune").unwrap(),
            Some("candidate".into())
        );
    }
}

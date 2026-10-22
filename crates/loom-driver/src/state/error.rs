use std::io;
use std::path::PathBuf;

use displaydoc::Display;
use thiserror::Error;

#[expect(
    clippy::doc_markdown,
    reason = "displaydoc fields are format placeholders; backticks would change the generated error text"
)]
#[derive(Debug, Display, Error)]
pub enum CacheError {
    /// failed to open SQLite database at {path}
    OpenDb {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },

    /// SQLite operation failed
    Sqlite(#[from] rusqlite::Error),

    /// failed to encode/decode JSON value for column {column}
    Json {
        column: &'static str,
        #[source]
        source: serde_json::Error,
    },

    /// cache-db lock was poisoned
    Poisoned,

    /// no spec found with label {label}
    SpecNotFound { label: String },

    /// unknown cache-db schema_version {version}; expected a value this build of loom can migrate from
    UnknownSchemaVersion { version: String },

    /// cannot import retired state at `{path}`: {detail}
    Legacy {
        path: std::path::PathBuf,
        detail: String,
    },
    /// state database is in use at {path}; stop active Loom commands before resetting it
    Busy {
        path: PathBuf,
        #[source]
        source: std::fs::TryLockError,
    },

    /// checkout Git storage could not be resolved
    Git(#[source] Box<crate::git::GitError>),

    /// io failure
    Io(#[from] io::Error),

    /// cache contained an invalid {kind} identifier `{value}`
    InvalidIdentifier { kind: &'static str, value: String },

    /// cache rebuild could not discover complete spec packages
    Package(#[source] crate::spec::package::Error),

    /// cache rebuild found an invalid spec index: {detail}
    SpecIndexMismatch { detail: String },

    /// multiple spec metadata epics found for `{label}`: {ids}; relabel all but one before re-running rebuild
    DuplicateSpecEpics { label: String, ids: String },

    /// work epic `{id}` is missing its cached range or state; run `loom init --rebuild`
    WorkEpicMissing { id: String },
}

impl From<crate::git::GitError> for CacheError {
    fn from(error: crate::git::GitError) -> Self {
        Self::Git(Box::new(error))
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::CacheError;
    use crate::git::GitError;

    #[test]
    fn git_error_conversion_preserves_typed_source() {
        let error = CacheError::from(GitError::GitCli {
            status: 128,
            stderr: "repository unavailable".into(),
        });
        assert_eq!(
            error.to_string(),
            "checkout Git storage could not be resolved"
        );
        assert!(matches!(
            error
                .source()
                .unwrap()
                .downcast_ref::<Box<GitError>>()
                .map(Box::as_ref),
            Some(GitError::GitCli { status: 128, stderr }) if stderr == "repository unavailable"
        ));
    }
}

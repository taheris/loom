use std::io;
use std::path::PathBuf;

use displaydoc::Display;
use thiserror::Error;

use crate::bd::BdError;

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

    /// io failure
    Io(#[from] io::Error),

    /// bead-metadata write inside productive-completion gate failed
    BdUpdate(#[from] BdError),

    /// cache contained an invalid {kind} identifier `{value}`
    InvalidIdentifier { kind: &'static str, value: String },

    /// cache rebuild found an invalid spec index: {detail}
    SpecIndexMismatch { detail: String },

    /// multiple open epics found for spec `{label}`: {ids}; close all but one before re-running rebuild
    DuplicateSpecMolecules { label: String, ids: String },
}

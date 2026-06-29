use std::io;
use std::path::PathBuf;

use displaydoc::Display;
use thiserror::Error;

/// Failures raised by [`super::list_for_label`] and
/// [`super::deps::collect_deps`].
#[derive(Debug, Display, Error)]
pub enum SpecError {
    /// io failure while reading {path}: {source}
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

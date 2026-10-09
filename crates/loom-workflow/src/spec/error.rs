use std::io;
use std::path::PathBuf;

use displaydoc::Display;
use thiserror::Error;

/// Failures raised by [`super::list_for_label`] and
/// [`super::deps::collect_deps`].
#[derive(Debug, Display, Error)]
pub enum SpecError {
    /// failed to discover spec owners
    Discovery(#[from] loom_driver::spec::package::Error),
    /// failed to parse spec acceptance
    Parse(#[from] loom_gate::annotation::ParseError),
    /// unknown spec label `{label}`
    UnknownLabel {
        label: loom_driver::identifier::SpecLabel,
    },
    /// io failure while reading {path}: {source}
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

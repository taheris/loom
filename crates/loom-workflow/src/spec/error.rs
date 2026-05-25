use std::io;
use std::path::PathBuf;

use displaydoc::Display;
use thiserror::Error;

/// Failures raised by [`super::annotations::parse_spec_annotations`] and
/// [`super::deps::scan_deps`].
#[derive(Debug, Display, Error)]
pub enum SpecError {
    /// io failure while reading {path}
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// no `## Success Criteria` section found in {path}
    NoSuccessCriteria { path: PathBuf },
}

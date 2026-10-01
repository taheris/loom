//! Private spawn configuration owned by the surrounding scratch session.

use std::io::Write;
use std::path::{Path, PathBuf};

use rustix::fs::Mode;

use super::directory::Directory;
use crate::agent::{ProtocolError, SpawnConfig};

const FILE_NAME: &str = "spawn-config.json";

/// Removes a newly created config on failed or cancelled startup.
/// After successful startup, [`Self::retain`] leaves cleanup to `ScratchSession`.
pub struct Config {
    directory: Directory,
    path: PathBuf,
    pending: bool,
}

impl Config {
    /// Create a mode-0600 config exclusively in an existing mode-0700 scratch directory.
    /// Symlink components, unsafe ancestors, and existing files are rejected.
    ///
    /// # Errors
    /// Returns serialization or filesystem errors without including credential values.
    pub fn write(scratch_dir: &Path, config: &SpawnConfig) -> Result<Self, ProtocolError> {
        let json = serde_json::to_vec(config)?;
        let directory = Directory::open(scratch_dir)?;
        directory.require_private()?;
        let mut file = directory.create_file(Path::new(FILE_NAME), Mode::RUSR | Mode::WUSR)?;
        let pending = Self {
            directory,
            path: scratch_dir.join(FILE_NAME),
            pending: true,
        };
        file.write_all(&json)?;
        Ok(pending)
    }

    /// Host-visible path handed to the launcher.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Keep the file readable by launcher and runner until scratch teardown.
    pub fn retain(mut self) {
        self.pending = false;
    }
}

impl Drop for Config {
    fn drop(&mut self) {
        if self.pending
            && let Err(error) = self.directory.remove_file(Path::new(FILE_NAME))
        {
            tracing::warn!(path = %self.path.display(), ?error, "spawn config cleanup failed");
        }
    }
}

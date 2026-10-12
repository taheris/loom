//! Immutable repository membership and contents for current-context resolution.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use displaydoc::Display;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A nonempty repository-relative path without traversal or glob syntax.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "PathBuf")]
pub struct ResourcePath(PathBuf);

impl TryFrom<PathBuf> for ResourcePath {
    type Error = Error;

    fn try_from(path: PathBuf) -> Result<Self, Error> {
        if path.as_os_str().is_empty()
            || !path
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            || path.to_str().is_none_or(|text| {
                text.chars()
                    .any(|c| c.is_control() || matches!(c, '*' | '?' | '[' | ']'))
            })
        {
            return Err(Error::Path { path });
        }
        Ok(Self(path))
    }
}

impl ResourcePath {
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// Content and membership identity, not a verification result.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct Identity(blake3::Hash);

impl TryFrom<String> for Identity {
    type Error = Error;

    fn try_from(value: String) -> Result<Self, Error> {
        let hash = blake3::Hash::from_hex(&value).map_err(|_| Error::Identity)?;
        if hash.to_hex().as_str() != value {
            return Err(Error::Identity);
        }
        Ok(Self(hash))
    }
}

impl Serialize for Identity {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.to_hex().as_str())
    }
}

/// Snapshot contents cannot be changed after discovery.
#[derive(Debug, Clone)]
pub struct Snapshot {
    root: PathBuf,
    files: BTreeMap<ResourcePath, Vec<u8>>,
    identity: Identity,
}

impl Snapshot {
    /// Capture tracked and nonignored new files; deleted files are absent.
    ///
    /// # Errors
    /// Rejects Git discovery failures, invalid paths, symlinks, and unreadable files.
    pub fn discover(root: &Path) -> Result<Self, Error> {
        let root = root.canonicalize()?;
        let git = loom_driver::git::GitClient::open(&root)?;
        if git.workdir().canonicalize()? != root {
            return Err(Error::Root);
        }
        let mut files = BTreeMap::new();
        for path in git.repository_files_sync()? {
            let path = ResourcePath::try_from(path)?;
            let absolute = root.join(path.as_path());
            let metadata = match absolute.symlink_metadata() {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || !absolute.canonicalize()?.starts_with(&root)
            {
                return Err(Error::Unsupported { path });
            }
            files.insert(path, std::fs::read(absolute)?);
        }
        let mut hash = blake3::Hasher::new();
        for (path, bytes) in &files {
            hash.update(path.as_path().as_os_str().as_encoded_bytes());
            hash.update(&[0]);
            hash.update(&(bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
        Ok(Self {
            root,
            files,
            identity: Identity(hash.finalize()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub const fn identity(&self) -> &Identity {
        &self.identity
    }

    pub const fn files(&self) -> &BTreeMap<ResourcePath, Vec<u8>> {
        &self.files
    }

    /// Read a captured file without consulting a changed working tree.
    ///
    /// # Errors
    /// Missing files and non-UTF-8 source are explicit failures.
    pub fn text(&self, path: &ResourcePath) -> Result<&str, Error> {
        let bytes = self
            .files
            .get(path)
            .ok_or_else(|| Error::Missing { path: path.clone() })?;
        std::str::from_utf8(bytes).map_err(|_| Error::Encoding)
    }

    pub(super) fn stage(&self) -> Result<tempfile::TempDir, Error> {
        let directory = tempfile::tempdir()?;
        for (path, bytes) in &self.files {
            let staged = directory.path().join(path.as_path());
            let parent = staged.parent().ok_or_else(|| Error::Path {
                path: staged.clone(),
            })?;
            std::fs::create_dir_all(parent)?;
            std::fs::write(staged, bytes)?;
        }
        Ok(directory)
    }

    /// Require a fresh snapshot if project discovery observed a changed checkout.
    ///
    /// # Errors
    /// Rejects changed contents or membership and propagates discovery failures.
    pub fn require_current(&self) -> Result<(), Error> {
        if Self::discover(&self.root)?.identity != self.identity {
            return Err(Error::Changed);
        }
        Ok(())
    }
}

#[derive(Debug, Display, Error)]
pub enum Error {
    /// subject discovery requires the repository root, not a subdirectory; retry `loom gate verify` at the Git root
    Root,
    /// repository discovery I/O failed
    Io(#[from] std::io::Error),
    /// Git subject discovery failed; repair repository access and rerun `loom gate verify`
    Git(#[from] loom_driver::git::GitError),
    /// invalid repository resource path `{path}`; use one exact relative path, not a glob
    Path { path: PathBuf },
    /// repository source paths and text must be UTF-8
    Encoding,
    /// unsupported nonregular repository resource `{path:?}`; declare and resolve its origin before verification
    Unsupported { path: ResourcePath },
    /// missing current resource `{path:?}`; repair the project definition and retry `loom gate verify`
    Missing { path: ResourcePath },
    /// repository changed during discovery; rerun `loom gate verify` against the current snapshot
    Changed,
    /// snapshot identities must be canonical 64-character lowercase BLAKE3 hashes
    Identity,
}

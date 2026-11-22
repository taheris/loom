//! Immutable repository membership and contents for current-context resolution.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read as _;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock};

use displaydoc::Display;
use rustix::io::Errno;
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
    files: Arc<BTreeMap<ResourcePath, Vec<u8>>>,
    executable: Arc<BTreeSet<ResourcePath>>,
    identity: Identity,
    pub(super) contracts: Arc<OnceLock<Vec<ResourcePath>>>,
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
        let directory = open_checkout(&root).map_err(std::io::Error::from)?;
        let mut files = BTreeMap::new();
        let mut executable = BTreeSet::new();
        for path in git.repository_files_sync()? {
            let path = ResourcePath::try_from(path)?;
            let mut file = match open_captured_file(&directory, &path) {
                Ok(file) => file,
                Err(Errno::NOENT) => continue,
                Err(Errno::LOOP | Errno::NOTDIR) => return Err(Error::Unsupported { path }),
                Err(error) => return Err(std::io::Error::from(error).into()),
            };
            let metadata = file.metadata()?;
            if !metadata.is_file() {
                return Err(Error::Unsupported { path });
            }
            #[cfg(unix)]
            if metadata.permissions().mode() & 0o111 != 0 {
                executable.insert(path.clone());
            }
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            files.insert(path, bytes);
        }
        let mut hash = blake3::Hasher::new();
        for (path, bytes) in &files {
            hash.update(path.as_path().as_os_str().as_encoded_bytes());
            hash.update(&[0, u8::from(executable.contains(path))]);
            hash.update(&(bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
        Ok(Self {
            root,
            files: Arc::new(files),
            executable: Arc::new(executable),
            identity: Identity(hash.finalize()),
            contracts: Arc::new(OnceLock::new()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub const fn identity(&self) -> &Identity {
        &self.identity
    }

    pub fn files(&self) -> &BTreeMap<ResourcePath, Vec<u8>> {
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

    pub fn is_executable(&self, path: &ResourcePath) -> bool {
        self.executable.contains(path)
    }

    pub(crate) fn stage(&self) -> Result<tempfile::TempDir, Error> {
        self.stage_paths(&self.files.keys().cloned().collect())
    }

    /// Materialize only the requested captured files, preserving captured executable modes.
    ///
    /// # Errors
    /// Missing captured resources and staging I/O failures are explicit errors.
    pub(crate) fn stage_paths(
        &self,
        paths: &BTreeSet<ResourcePath>,
    ) -> Result<tempfile::TempDir, Error> {
        for path in paths {
            if !self.files.contains_key(path) {
                return Err(Error::Missing { path: path.clone() });
            }
        }
        let directory = staging_directory(&std::env::temp_dir())?;
        for path in paths {
            let bytes = &self.files[path];
            let staged = directory.path().join(path.as_path());
            let parent = staged.parent().ok_or_else(|| Error::Path {
                path: staged.clone(),
            })?;
            std::fs::create_dir_all(parent)?;
            std::fs::write(&staged, bytes)?;
            #[cfg(unix)]
            std::fs::set_permissions(
                staged,
                std::fs::Permissions::from_mode(if self.executable.contains(path) {
                    0o755
                } else {
                    0o644
                }),
            )?;
        }
        Ok(directory)
    }

    /// Require a fresh snapshot if project discovery observed a changed checkout.
    ///
    /// # Errors
    /// Rejects changed contents, executable modes or membership and propagates discovery failures.
    pub fn require_current(&self) -> Result<(), Error> {
        if Self::discover(&self.root)?.identity != self.identity {
            return Err(Error::Changed);
        }
        Ok(())
    }
}

const NO_LINK_FLAGS: rustix::fs::OFlags = rustix::fs::OFlags::RDONLY
    .union(rustix::fs::OFlags::NOFOLLOW)
    .union(rustix::fs::OFlags::CLOEXEC);
const DIRECTORY_FLAGS: rustix::fs::OFlags = NO_LINK_FLAGS.union(rustix::fs::OFlags::DIRECTORY);

fn open_checkout(root: &Path) -> rustix::io::Result<rustix::fd::OwnedFd> {
    use rustix::fs::{Mode, open, openat};

    // `root` is an absolute canonical path. Do not follow a link substituted
    // for a parent of that path between canonicalization and capture either.
    let mut directory = open("/", DIRECTORY_FLAGS, Mode::empty())?;
    for part in root
        .strip_prefix("/")
        .map_err(|_| rustix::io::Errno::INVAL)?
    {
        directory = openat(&directory, part, DIRECTORY_FLAGS, Mode::empty())?;
    }
    Ok(directory)
}

// ResourcePath has only normal components. Open each component without
// following links and read from the resulting descriptor, not a second lookup.
fn open_captured_file(root: &rustix::fd::OwnedFd, path: &ResourcePath) -> rustix::io::Result<File> {
    use rustix::fs::{Mode, OFlags, openat};

    let mut current = None;
    let mut components = path.as_path().iter().peekable();
    while let Some(part) = components.next() {
        let parent = current.as_ref().unwrap_or(root);
        if components.peek().is_none() {
            return openat(
                parent,
                part,
                NO_LINK_FLAGS | OFlags::NONBLOCK,
                Mode::empty(),
            )
            .map(File::from);
        }
        current = Some(openat(parent, part, DIRECTORY_FLAGS, Mode::empty())?);
    }
    Err(rustix::io::Errno::INVAL)
}

fn staging_directory(parent: &Path) -> std::io::Result<tempfile::TempDir> {
    tempfile::tempdir_in(parent.canonicalize()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn staged_directory_resolves_symlinked_temporary_parent() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("actual");
        std::fs::create_dir(&parent).unwrap();
        let alias = directory.path().join("alias");
        std::os::unix::fs::symlink(&parent, &alias).unwrap();
        let staged = staging_directory(&alias).unwrap();
        assert_eq!(staged.path(), staged.path().canonicalize().unwrap());
        assert!(staged.path().starts_with(parent.canonicalize().unwrap()));
        let staged_path = staged.path().to_path_buf();
        drop(staged);
        assert!(!staged_path.exists());
    }

    fn path(value: &str) -> ResourcePath {
        ResourcePath::try_from(PathBuf::from(value)).unwrap()
    }

    fn fixture() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        loom_driver::git::init_test_repo(directory.path()).unwrap();
        std::fs::create_dir(directory.path().join("selected")).unwrap();
        std::fs::write(
            directory.path().join("selected/run.sh"),
            "#!/bin/sh\necho captured\n",
        )
        .unwrap();
        std::fs::write(directory.path().join("unrelated.txt"), "not staged\n").unwrap();
        directory
    }

    #[test]
    fn exact_partition_stages_captured_bytes_without_unrelated_checkout_inputs() {
        let directory = fixture();
        let snapshot = Snapshot::discover(directory.path()).unwrap();
        std::fs::write(directory.path().join("selected/run.sh"), "changed\n").unwrap();
        let partition = snapshot
            .stage_paths(&BTreeSet::from([path("selected/run.sh")]))
            .unwrap();
        assert_eq!(
            std::fs::read(partition.path().join("selected/run.sh")).unwrap(),
            b"#!/bin/sh\necho captured\n"
        );
        assert!(!partition.path().join("unrelated.txt").exists());
        assert!(matches!(snapshot.require_current(), Err(Error::Changed)));
    }

    #[test]
    fn contract_membership_is_shared_only_within_one_immutable_snapshot() {
        let directory = fixture();
        std::fs::create_dir(directory.path().join("specs")).unwrap();
        std::fs::write(directory.path().join("specs/first.md"), "# First\n").unwrap();
        let snapshot = Snapshot::discover(directory.path()).unwrap();
        let first = super::super::contracts(&snapshot).unwrap();
        assert_eq!(first, [path("specs/first.md")]);
        let cloned = snapshot.clone();
        assert!(Arc::ptr_eq(&snapshot.contracts, &cloned.contracts));
        assert_eq!(cloned.contracts.get(), Some(&first));

        std::fs::write(directory.path().join("specs/second.md"), "# Second\n").unwrap();
        assert_eq!(super::super::contracts(&cloned).unwrap(), first);
        assert!(matches!(cloned.require_current(), Err(Error::Changed)));
        let changed = Snapshot::discover(directory.path()).unwrap();
        assert!(changed.contracts.get().is_none());
        assert_eq!(
            super::super::contracts(&changed)
                .unwrap()
                .into_iter()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([path("specs/first.md"), path("specs/second.md")])
        );
        assert!(!Arc::ptr_eq(&snapshot.contracts, &changed.contracts));
    }

    #[test]
    fn malformed_contract_membership_never_initializes_a_successful_discovery() {
        let directory = fixture();
        std::fs::create_dir_all(directory.path().join("specs/first")).unwrap();
        std::fs::write(directory.path().join("specs/first/spec.md"), "# First\n").unwrap();
        let snapshot = Snapshot::discover(directory.path()).unwrap();
        assert!(super::super::contracts(&snapshot).is_err());
        assert!(snapshot.contracts.get().is_none());
        let cloned = snapshot.clone();
        assert!(super::super::contracts(&cloned).is_err());
        assert!(snapshot.contracts.get().is_none());
    }

    #[test]
    fn a_partition_cannot_materialize_a_path_absent_from_the_snapshot() {
        let directory = fixture();
        let snapshot = Snapshot::discover(directory.path()).unwrap();
        assert!(matches!(
            snapshot.stage_paths(&BTreeSet::from([path("absent")])),
            Err(Error::Missing { path: missing }) if missing == path("absent")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn partitions_use_captured_executable_modes_not_later_checkout_permissions() {
        let directory = fixture();
        let executable = directory.path().join("selected/run.sh");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let snapshot = Snapshot::discover(directory.path()).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(snapshot.is_executable(&path("selected/run.sh")));
        assert!(!snapshot.is_executable(&path("unrelated.txt")));
        let partition = snapshot
            .stage_paths(&BTreeSet::from([path("selected/run.sh")]))
            .unwrap();
        assert_eq!(
            std::fs::metadata(partition.path().join("selected/run.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert!(matches!(snapshot.require_current(), Err(Error::Changed)));
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

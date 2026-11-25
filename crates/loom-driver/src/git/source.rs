//! Read regular source files from immutable Git objects, never a live checkout.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use loom_protocol::oid::GitOid;

use super::GitError;

/// A complete, checksum-verified source tree for one exact commit, not verification evidence.
/// It cannot be deserialized or constructed from an unchecked revision string.
#[derive(Debug)]
pub struct CommitSource {
    commit: GitOid,
    tree: GitOid,
    files: BTreeMap<PathBuf, CommitFile>,
}

impl CommitSource {
    pub const fn commit(&self) -> &GitOid {
        &self.commit
    }

    pub const fn tree(&self) -> &GitOid {
        &self.tree
    }

    pub const fn files(&self) -> &BTreeMap<PathBuf, CommitFile> {
        &self.files
    }
}

/// Exact Git blob contents and executable mode. Directories are not sources.
#[derive(Debug)]
pub struct CommitFile {
    blob: GitOid,
    bytes: Vec<u8>,
    executable: bool,
}

impl CommitFile {
    pub const fn blob(&self) -> &GitOid {
        &self.blob
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub const fn is_executable(&self) -> bool {
        self.executable
    }
}

fn object(repo: &gix::Repository, expected: gix::ObjectId) -> Result<gix::Object<'_>, GitError> {
    let object = repo
        .find_object(expected)
        .map_err(|error| GitError::Gix(error.to_string()))?;
    if let Err(error) =
        gix::objs::Data::new(&object.data, object.kind, expected.kind()).verify_checksum(&expected)
    {
        return Err(GitError::SourceObject {
            oid: GitOid::new(&expected.to_string())?,
            detail: error.to_string(),
        });
    }
    Ok(object)
}

fn component(parent: &Path, bytes: &[u8]) -> Result<PathBuf, GitError> {
    let name = std::str::from_utf8(bytes).map_err(|_| GitError::SourceEntry {
        path: parent.to_owned(),
    })?;
    let mut parts = Path::new(name).components();
    let valid_component = matches!(parts.next(), Some(Component::Normal(part)) if part == name)
        && parts.next().is_none()
        && !name.eq_ignore_ascii_case(".git");
    if !valid_component {
        return Err(GitError::SourceEntry {
            path: parent.join(name),
        });
    }
    Ok(parent.join(name))
}

pub(super) fn capture(repo: &gix::Repository, commit: &GitOid) -> Result<CommitSource, GitError> {
    let id = gix::ObjectId::from_hex(commit.as_str().as_bytes())
        .map_err(|error| GitError::Gix(error.to_string()))?;
    let id = object(repo, id)?
        .try_into_commit()
        .map_err(|error| GitError::Gix(error.to_string()))?
        .tree_id()
        .map_err(|error| GitError::Gix(error.to_string()))?
        .detach();
    let tree = GitOid::new(&id.to_string())?;
    let mut files = BTreeMap::new();
    let mut pending = vec![(PathBuf::new(), id)];
    while let Some((parent, id)) = pending.pop() {
        let tree = object(repo, id)?
            .try_into_tree()
            .map_err(|error| GitError::Gix(error.to_string()))?;
        let mut names = BTreeSet::new();
        for entry in tree.iter() {
            let entry = entry.map_err(|error| GitError::Gix(error.to_string()))?;
            let path = component(&parent, entry.filename().as_ref())?;
            if !names.insert(path.clone()) {
                return Err(GitError::SourceEntry { path });
            }
            let id = entry.object_id();
            let mode = entry.mode().value();
            match mode {
                0o040_000 => pending.push((path, id)),
                0o100_644 | 0o100_755 => {
                    let blob = GitOid::new(&id.to_string())?;
                    let mut object = object(repo, id)?
                        .try_into_blob()
                        .map_err(|error| GitError::Gix(error.to_string()))?;
                    files.insert(
                        path,
                        CommitFile {
                            blob,
                            bytes: std::mem::take(&mut object.data),
                            executable: mode == 0o100_755,
                        },
                    );
                }
                _ => return Err(GitError::SourceEntry { path }),
            }
        }
    }
    Ok(CommitSource {
        commit: commit.clone(),
        tree,
        files,
    })
}

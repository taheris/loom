//! A resolved filter is not gate authorization: only the evidence types certify execution.
use std::path::{Path, PathBuf};

use displaydoc::Display;
use loom_driver::git::{GitClient, GitError};
use thiserror::Error;

/// One explicit selection supplied by an operator or workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Files(Vec<PathBuf>),
    Diff(String),
    Tree,
    Target(String),
}

/// A finite set (including empty), whole tree, or exact target, resolved once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    selection: Selection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Selection {
    Files {
        paths: Vec<PathBuf>,
        origin: FileOrigin,
    },
    Tree,
    Target(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FileOrigin {
    Explicit,
    Diff(String),
}

impl Request {
    /// Resolve Git ranges and normalize relative paths without mutating CLI flags.
    ///
    /// # Errors
    /// An invalid Git range is an error, never an empty or whole-tree fallback.
    pub async fn resolve(self, workspace: &Path) -> Result<Resolved, ResolveError> {
        let selection = match self {
            Self::Tree => Selection::Tree,
            Self::Target(target) => Selection::Target(target),
            Self::Files(paths) => Selection::Files {
                paths: absolute_paths(workspace, paths),
                origin: FileOrigin::Explicit,
            },
            Self::Diff(range) => {
                let paths = async {
                    GitClient::open(workspace)?
                        .changed_files_in_range(&range, None)
                        .await
                }
                .await
                .map_err(|source| ResolveError::Diff {
                    range: range.clone(),
                    source,
                })?;
                Selection::Files {
                    paths: absolute_paths(workspace, paths),
                    origin: FileOrigin::Diff(range),
                }
            }
        };
        Ok(Resolved { selection })
    }
}

impl Resolved {
    /// Select annotations without interpreting an empty finite set as unscoped.
    pub fn select(
        &self,
        annotations: &[crate::Annotation],
        resolver: &mut crate::InputResolver,
    ) -> Vec<crate::Annotation> {
        match &self.selection {
            Selection::Tree => annotations.to_vec(),
            Selection::Target(target) => annotations
                .iter()
                .filter(|annotation| &annotation.target == target)
                .cloned()
                .collect(),
            Selection::Files { paths, .. } if paths.is_empty() => Vec::new(),
            Selection::Files { paths, .. } => crate::filter_by_files(annotations, paths, resolver),
        }
    }

    /// `Some([])` means run no file-scoped targets, not the whole tree.
    pub fn files(&self) -> Option<&[PathBuf]> {
        match &self.selection {
            Selection::Files { paths, .. } => Some(paths),
            _ => None,
        }
    }
    pub fn diff(&self) -> Option<&str> {
        match &self.selection {
            Selection::Files {
                origin: FileOrigin::Diff(range),
                ..
            } => Some(range),
            _ => None,
        }
    }
    pub fn target(&self) -> Option<&str> {
        match &self.selection {
            Selection::Target(target) => Some(target),
            _ => None,
        }
    }
    pub const fn is_tree(&self) -> bool {
        matches!(self.selection, Selection::Tree)
    }
    pub const fn is_explicit_files(&self) -> bool {
        matches!(
            self.selection,
            Selection::Files {
                origin: FileOrigin::Explicit,
                ..
            }
        )
    }
}

fn absolute_paths(workspace: &Path, paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths
        .into_iter()
        .map(|path| {
            if path.is_relative() {
                workspace.join(path)
            } else {
                path
            }
        })
        .collect()
}

#[derive(Debug, Display, Error)]
pub enum ResolveError {
    /// loom gate: --diff {range} could not be resolved to a file set
    Diff {
        range: String,
        #[source]
        source: GitError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolved_scope_distinguishes_empty_tree_target_and_file_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let empty = Request::Files(vec![]).resolve(dir.path()).await.unwrap();
        assert_eq!(empty.files(), Some([].as_slice()));
        assert!(!empty.is_tree());
        assert!(empty.is_explicit_files());
        let tree = Request::Tree.resolve(dir.path()).await.unwrap();
        assert!(tree.is_tree());
        assert!(tree.files().is_none());
        let target = Request::Target("exact --target".into())
            .resolve(dir.path())
            .await
            .unwrap();
        assert_eq!(target.target(), Some("exact --target"));
        assert!(target.files().is_none());
        let files = Request::Files(vec!["src/lib.rs".into()])
            .resolve(dir.path())
            .await
            .unwrap();
        assert_eq!(
            files.files(),
            Some([dir.path().join("src/lib.rs")].as_slice())
        );
        assert!(files.is_explicit_files());
        assert!(files.diff().is_none());
    }
}

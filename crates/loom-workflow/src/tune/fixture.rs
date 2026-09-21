use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use loom_driver::git::initialize_snapshot;

use super::{TuneError, budget::Budget};

#[derive(Clone)]
struct File {
    bytes: Vec<u8>,
    permissions: fs::Permissions,
}

/// Immutable file inputs shared by both sides, never Git metadata or live state.
#[derive(Clone)]
pub(super) struct Snapshot {
    files: BTreeMap<PathBuf, File>,
}

impl Snapshot {
    pub(super) fn capture(root: &Path, tracked: &BTreeSet<PathBuf>) -> Result<Self, TuneError> {
        let root = fs::canonicalize(root).map_err(|source| TuneError::ReadFile {
            path: root.to_owned(),
            source,
        })?;
        let mut files = BTreeMap::new();
        for relative in tracked {
            require_relative(relative)?;
            if reserved(relative) {
                continue;
            }
            let path = root.join(relative);
            let canonical = fs::canonicalize(&path).map_err(|source| TuneError::ReadFile {
                path: path.clone(),
                source,
            })?;
            let canonical_relative = canonical
                .strip_prefix(&root)
                .map_err(|_| invalid(&path, "file escapes snapshot root"))?;
            if reserved(canonical_relative) || !tracked.contains(canonical_relative) {
                return Err(invalid(
                    &path,
                    "file resolves to untracked or private state",
                ));
            }
            let metadata = fs::metadata(&canonical).map_err(|source| TuneError::ReadFile {
                path: path.clone(),
                source,
            })?;
            if !metadata.is_file() {
                return Err(invalid(
                    &path,
                    "replay inputs must be regular tracked files",
                ));
            }
            let bytes =
                fs::read(&canonical).map_err(|source| TuneError::ReadFile { path, source })?;
            files.insert(
                relative.clone(),
                File {
                    bytes,
                    permissions: metadata.permissions(),
                },
            );
        }
        Ok(Self { files })
    }

    pub(super) fn text(&self, path: &Path) -> Result<String, TuneError> {
        let file = self
            .files
            .get(path)
            .ok_or_else(|| invalid(path, "file was not captured in the frozen inputs"))?;
        String::from_utf8(file.bytes.clone()).map_err(|_| invalid(path, "text input is not UTF-8"))
    }

    pub(super) fn fixture(&self, path: &Path, request: &str) -> Result<(Self, String), TuneError> {
        let state_path = path.join("state.toml");
        if self.files.contains_key(&state_path) {
            let state: toml::Table = toml::from_str(&self.text(&state_path)?)?;
            if !state.is_empty() {
                return Err(invalid(
                    &state_path,
                    "Beads/inbox/tune state setup is unavailable; replay cannot use operator state",
                ));
            }
        }
        let root = path.join("repo");
        let mut files = BTreeMap::new();
        for (source, file) in &self.files {
            let Ok(relative) = source.strip_prefix(&root) else {
                continue;
            };
            require_relative(relative)?;
            if reserved(relative) {
                return Err(invalid(
                    source,
                    "fixture repo cannot supply Git metadata or live Loom/Beads state",
                ));
            }
            files.insert(relative.to_owned(), file.clone());
        }
        if files.is_empty() {
            return Err(invalid(&root, "fixture repo/ contains no tracked files"));
        }
        let input_path = path.join("input.md");
        let mut input = request.to_owned();
        if self.files.contains_key(&input_path) {
            input.push_str("\n\n");
            input.push_str(&self.text(&input_path)?);
        }
        Ok((Self { files }, input))
    }

    pub(super) async fn checkout(&self, budget: &Budget<'_>) -> Result<Checkout, TuneError> {
        budget.remaining()?;
        let directory = tempfile::Builder::new()
            .prefix("loom-tune-replay-")
            .tempdir()
            .map_err(TuneError::ReplayWorkspace)?;
        let checkout = Checkout { directory };
        for (relative, file) in &self.files {
            budget.remaining()?;
            let path = checkout.path().join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|source| TuneError::CreateDir {
                    path: parent.to_owned(),
                    source,
                })?;
            }
            fs::write(&path, &file.bytes)
                .and_then(|()| fs::set_permissions(&path, file.permissions.clone()))
                .map_err(|source| TuneError::WriteFile { path, source })?;
        }
        budget
            .run(async {
                initialize_snapshot(checkout.path(), budget.clock()).await?;
                Ok(())
            })
            .await?;
        Ok(checkout)
    }
}

/// Owns only one disposable replay directory, including failure/cancellation cleanup.
pub(super) struct Checkout {
    directory: tempfile::TempDir,
}

impl Checkout {
    pub(super) fn path(&self) -> &Path {
        self.directory.path()
    }

    pub(super) fn cleanup(&self) -> Result<(), TuneError> {
        fs::remove_dir_all(self.path()).map_err(TuneError::ReplayWorkspace)
    }
}

impl Drop for Checkout {
    fn drop(&mut self) {
        if let Err(source) = fs::remove_dir_all(self.path())
            && source.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(?source, path = %self.path().display(), "failed to clean up replay checkout");
        }
    }
}

fn require_relative(path: &Path) -> Result<(), TuneError> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid(
            path,
            "snapshot paths must be normalized and relative",
        ));
    }
    Ok(())
}

fn reserved(path: &Path) -> bool {
    [".git", ".loom", ".beads", ".wrix"]
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

fn invalid(path: &Path, reason: &str) -> TuneError {
    TuneError::ReplayFixture {
        path: path.to_owned(),
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::clock::{MockClock, SystemClock};
    use loom_driver::git::{GitClient, read_origin_url};
    use loom_tune::config::ChecksConfig;

    fn source() -> (tempfile::TempDir, Snapshot) {
        let source = tempfile::tempdir().unwrap();
        fs::create_dir_all(source.path().join("case/repo/src")).unwrap();
        fs::write(source.path().join("case/repo/src/lib.rs"), "original").unwrap();
        fs::write(source.path().join("case/repo/data.bin"), [0, 255, 0]).unwrap();
        fs::write(
            source.path().join("case/input.md"),
            "Additional fixture task",
        )
        .unwrap();
        let tracked = [
            "case/repo/src/lib.rs",
            "case/repo/data.bin",
            "case/input.md",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        let snapshot = Snapshot::capture(source.path(), &tracked).unwrap();
        (source, snapshot)
    }

    #[tokio::test]
    async fn replay_checkouts_are_independent_frozen_git_fixtures() {
        let (source, snapshot) = source();
        let (fixture, input) = snapshot
            .fixture(Path::new("case"), "Fix the parser")
            .unwrap();
        assert_eq!(input, "Fix the parser\n\nAdditional fixture task");
        assert!(
            !input.contains("original"),
            "repository files are not flattened into the prompt"
        );
        fs::write(source.path().join("case/repo/src/lib.rs"), "operator edit").unwrap();
        let clock = SystemClock::new();
        let budget = Budget::new(&clock, &ChecksConfig::default());
        let current = fixture.checkout(&budget).await.unwrap();
        let current_head = GitClient::open(current.path())
            .unwrap()
            .head_commit_sha()
            .await
            .unwrap();
        assert!(read_origin_url(current.path()).unwrap().is_none());
        fs::write(current.path().join("src/lib.rs"), "current mutation").unwrap();
        fs::write(current.path().join("current-only"), "not shared").unwrap();
        let candidate = fixture.checkout(&budget).await.unwrap();
        assert_eq!(
            GitClient::open(candidate.path())
                .unwrap()
                .head_commit_sha()
                .await
                .unwrap(),
            current_head
        );
        assert_eq!(
            fs::read_to_string(candidate.path().join("src/lib.rs")).unwrap(),
            "original"
        );
        assert_eq!(
            fs::read(candidate.path().join("data.bin")).unwrap(),
            [0, 255, 0]
        );
        assert!(!candidate.path().join("current-only").exists());
        fs::write(candidate.path().join("src/lib.rs"), "candidate mutation").unwrap();
        assert_eq!(
            fs::read_to_string(current.path().join("src/lib.rs")).unwrap(),
            "current mutation"
        );
        assert_eq!(
            fs::read_to_string(source.path().join("case/repo/src/lib.rs")).unwrap(),
            "operator edit"
        );
        let paths = [current.path().to_owned(), candidate.path().to_owned()];
        drop((current, candidate));
        assert!(paths.iter().all(|path| !path.exists()));
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_removes_owned_replay_directory() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_owned();
        let clock = MockClock::new();
        let config = ChecksConfig {
            max_wall_time_secs: 1,
            ..ChecksConfig::default()
        };
        let budget = Budget::new(&clock, &config);
        let result = budget
            .run(async move {
                let checkout = Checkout { directory };
                fs::write(checkout.path().join("mutated"), "partial replay").unwrap();
                std::future::pending::<Result<(), TuneError>>().await
            })
            .await;
        assert!(matches!(result, Err(TuneError::WallTimeExceeded { .. })));
        assert!(!path.exists());
    }

    #[test]
    fn unsupported_fixture_state_is_not_silently_flattened_or_ignored() {
        let (source, _) = source();
        fs::write(source.path().join("case/state.toml"), "bead = 'lm-live'").unwrap();
        let tracked = ["case/repo/src/lib.rs", "case/state.toml"]
            .into_iter()
            .map(PathBuf::from)
            .collect();
        let snapshot = Snapshot::capture(source.path(), &tracked).unwrap();
        assert!(matches!(
            snapshot.fixture(Path::new("case"), "resolve"),
            Err(TuneError::ReplayFixture { .. })
        ));
    }

    #[test]
    fn fixture_cannot_import_git_or_live_beads_metadata() {
        for reserved in [
            ".git/config",
            ".beads/config.yaml",
            ".loom/cache.db",
            ".wrix/dolt.sock",
        ] {
            let (source, _) = source();
            let path = PathBuf::from("case/repo").join(reserved);
            fs::create_dir_all(source.path().join(&path).parent().unwrap()).unwrap();
            fs::write(source.path().join(&path), "private").unwrap();
            let snapshot = Snapshot::capture(source.path(), &BTreeSet::from([path])).unwrap();
            assert!(
                snapshot.fixture(Path::new("case"), "resolve").is_err(),
                "{reserved}"
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn snapshot_rejects_symlink_escape_and_untracked_referents() {
        let (source, _) = source();
        let external = tempfile::NamedTempFile::new().unwrap();
        let relative = PathBuf::from("case/repo/leak");
        std::os::unix::fs::symlink(external.path(), source.path().join(&relative)).unwrap();
        assert!(Snapshot::capture(source.path(), &BTreeSet::from([relative.clone()])).is_err());
        fs::remove_file(source.path().join(&relative)).unwrap();
        std::os::unix::fs::symlink("src/lib.rs", source.path().join(&relative)).unwrap();
        assert!(Snapshot::capture(source.path(), &BTreeSet::from([relative])).is_err());
    }
}

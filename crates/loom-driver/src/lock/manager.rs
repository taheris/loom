use std::ffi::OsString;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rustix::fs::{FlockOperation, flock};

use crate::clock::{Clock, SystemClock};
use crate::identifier::BeadId;

use super::error::{LockError, PhaseLock};

const WORKSPACE_LOCK_STEM: &str = "workspace";
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

pub struct LockManager {
    workspace: PathBuf,
    locks_dir: PathBuf,
}

pub struct LockGuard {
    file: File,
    workspace: Option<PathBuf>,
    workspace_lock: Option<File>,
}

impl LockGuard {
    pub(crate) fn workspace(&self) -> Option<&Path> {
        self.workspace.as_deref()
    }
}

impl std::fmt::Debug for LockGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockGuard").finish_non_exhaustive()
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if let Err(error) = flock(&self.file, FlockOperation::Unlock) {
            tracing::warn!(?error, "failed to unlock loom lock");
        }
        drop(self.workspace_lock.take());
    }
}

impl LockManager {
    ///
    /// # Errors
    ///
    /// Returns an error when lock state cannot be read, updated, or validated.
    pub fn new(workspace: impl AsRef<Path>) -> Result<Self, LockError> {
        let state_home = resolve_xdg_state_home()?;
        Self::with_state_home(workspace, state_home)
    }

    ///
    /// # Errors
    ///
    /// Returns an error when lock state cannot be read, updated, or validated.
    pub fn with_state_home(
        workspace: impl AsRef<Path>,
        state_home: impl AsRef<Path>,
    ) -> Result<Self, LockError> {
        let basename = workspace_basename(workspace.as_ref())?;
        let locks_dir = state_home.as_ref().join("loom/locks").join(&basename);
        fs::create_dir_all(&locks_dir).map_err(|source| LockError::CreateDir {
            path: locks_dir.clone(),
            source,
        })?;
        let workspace = workspace.as_ref().canonicalize().map_err(|source| {
            LockError::CanonicalizeWorkspace {
                path: workspace.as_ref().to_path_buf(),
                source,
            }
        })?;
        Ok(Self {
            workspace,
            locks_dir,
        })
    }

    /// Serialize a local reconciliation independently of scheduler work-root locks.
    ///
    /// # Errors
    /// Reports lock acquisition and timeout failures.
    pub async fn acquire_record_async(
        &self,
        kind: crate::state::RecordKind,
        key: &BeadId,
        clock: &dyn Clock,
    ) -> Result<LockGuard, LockError> {
        let workspace = self.shared_workspace_lock()?;
        let path = self
            .locks_dir
            .join(format!("record-{}-{key}.lock", kind.as_str()));
        let mut guard = acquire_with_timeout(&path, DEFAULT_LOCK_TIMEOUT, clock, || {
            LockError::WorkRootBusy {
                root: key.to_string(),
            }
        })
        .await?;
        guard.workspace_lock = Some(workspace);
        Ok(guard)
    }

    pub fn locks_dir(&self) -> &Path {
        &self.locks_dir
    }

    ///
    /// # Errors
    ///
    /// Returns an error when lock state cannot be read, updated, or validated.
    pub fn acquire_planning(&self) -> Result<LockGuard, LockError> {
        self.acquire_phase(PhaseLock::Planning)
    }

    ///
    /// # Errors
    ///
    /// Returns an error when lock state cannot be read, updated, or validated.
    pub fn acquire_todo(&self) -> Result<LockGuard, LockError> {
        self.acquire_phase(PhaseLock::Todo)
    }

    ///
    /// # Errors
    ///
    /// Returns an error when lock state cannot be read, updated, or validated.
    pub fn acquire_phase(&self, phase: PhaseLock) -> Result<LockGuard, LockError> {
        self.acquire_phase_with_timeout(phase, DEFAULT_LOCK_TIMEOUT)
    }

    ///
    /// # Errors
    ///
    /// Returns an error when lock state cannot be read, updated, or validated.
    pub fn acquire_phase_with_timeout(
        &self,
        phase: PhaseLock,
        timeout: Duration,
    ) -> Result<LockGuard, LockError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(LockError::RuntimeBuild)?;
        runtime.block_on(self.acquire_phase_with_timeout_async(phase, &SystemClock::new(), timeout))
    }

    ///
    /// # Errors
    ///
    /// Returns an error when lock state cannot be read, updated, or validated.
    pub async fn acquire_phase_async(
        &self,
        phase: PhaseLock,
        clock: &dyn Clock,
    ) -> Result<LockGuard, LockError> {
        self.acquire_phase_with_timeout_async(phase, clock, DEFAULT_LOCK_TIMEOUT)
            .await
    }

    ///
    /// # Errors
    ///
    /// Returns an error when lock state cannot be read, updated, or validated.
    pub async fn acquire_phase_with_timeout_async(
        &self,
        phase: PhaseLock,
        clock: &dyn Clock,
        timeout: Duration,
    ) -> Result<LockGuard, LockError> {
        let workspace = self.shared_workspace_lock()?;
        let path = self.phase_lock_path(phase);
        let mut guard =
            acquire_with_timeout(&path, timeout, clock, || LockError::PhaseBusy { phase }).await?;
        guard.workspace_lock = Some(workspace);
        Ok(guard)
    }

    ///
    /// # Errors
    ///
    /// Returns an error when lock state cannot be read, updated, or validated.
    pub fn acquire_work_root(&self, root: &BeadId) -> Result<LockGuard, LockError> {
        self.acquire_work_root_with_timeout(root, DEFAULT_LOCK_TIMEOUT)
    }

    ///
    /// # Errors
    ///
    /// Returns an error when lock state cannot be read, updated, or validated.
    pub fn acquire_work_root_with_timeout(
        &self,
        root: &BeadId,
        timeout: Duration,
    ) -> Result<LockGuard, LockError> {
        let workspace = self.shared_workspace_lock()?;
        let path = self.work_root_lock_path(root);
        if let Some(mut guard) = try_lock_file(open_lock_file(&path)?)? {
            guard.workspace_lock = Some(workspace);
            return Ok(guard);
        }
        drop(workspace);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(LockError::RuntimeBuild)?;
        runtime.block_on(self.acquire_work_root_with_timeout_async(
            root,
            &SystemClock::new(),
            timeout,
        ))
    }

    ///
    /// # Errors
    ///
    /// Returns an error when lock state cannot be read, updated, or validated.
    pub async fn acquire_work_root_async(
        &self,
        root: &BeadId,
        clock: &dyn Clock,
    ) -> Result<LockGuard, LockError> {
        self.acquire_work_root_with_timeout_async(root, clock, DEFAULT_LOCK_TIMEOUT)
            .await
    }

    ///
    /// # Errors
    ///
    /// Returns an error when lock state cannot be read, updated, or validated.
    pub async fn acquire_work_root_with_timeout_async(
        &self,
        root: &BeadId,
        clock: &dyn Clock,
        timeout: Duration,
    ) -> Result<LockGuard, LockError> {
        let workspace = self.shared_workspace_lock()?;
        let path = self.work_root_lock_path(root);
        let mut guard = acquire_with_timeout(&path, timeout, clock, || LockError::WorkRootBusy {
            root: root.to_string(),
        })
        .await?;
        guard.workspace_lock = Some(workspace);
        Ok(guard)
    }

    ///
    /// # Errors
    ///
    /// Returns an error when lock state cannot be read, updated, or validated.
    pub fn acquire_workspace(&self) -> Result<LockGuard, LockError> {
        let path = self.locks_dir.join(format!("{WORKSPACE_LOCK_STEM}.lock"));
        let file = open_lock_file(&path)?;
        let Some(mut guard) = try_lock_file(file)? else {
            return Err(LockError::WorkspaceBusy {
                root: self
                    .find_held_mutating_lock()?
                    .unwrap_or_else(|| WORKSPACE_LOCK_STEM.into()),
            });
        };
        if let Some(root) = self.find_held_mutating_lock()? {
            return Err(LockError::WorkspaceBusy { root });
        }
        guard.workspace = Some(self.workspace.clone());
        Ok(guard)
    }

    fn shared_workspace_lock(&self) -> Result<File, LockError> {
        let path = self.locks_dir.join(format!("{WORKSPACE_LOCK_STEM}.lock"));
        let file = open_lock_file(&path)?;
        file.try_lock_shared()
            .map_err(|_| LockError::WorkspaceBusy {
                root: WORKSPACE_LOCK_STEM.into(),
            })?;
        Ok(file)
    }

    fn phase_lock_path(&self, phase: PhaseLock) -> PathBuf {
        self.locks_dir.join(format!("{}.lock", phase.file_stem()))
    }

    fn work_root_lock_path(&self, root: &BeadId) -> PathBuf {
        self.locks_dir.join(format!("{}.lock", root.as_str()))
    }

    fn find_held_mutating_lock(&self) -> Result<Option<String>, LockError> {
        let entries = match fs::read_dir(&self.locks_dir) {
            Ok(it) => it,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(LockError::Io(e)),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("lock") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if stem == WORKSPACE_LOCK_STEM {
                continue;
            }
            let file = open_lock_file(&path)?;
            if try_lock_file(file)?.is_none() {
                return Ok(Some(stem.to_string()));
            }
        }
        Ok(None)
    }
}

/// Resolve the existing per-user driver state location.
///
/// # Errors
/// Reports an unset home when no explicit state root is configured.
pub fn resolve_xdg_state_home() -> Result<PathBuf, LockError> {
    if let Some(val) = std::env::var_os("XDG_STATE_HOME")
        && !val.is_empty()
    {
        return Ok(PathBuf::from(val));
    }
    let home = std::env::var_os("HOME").ok_or(LockError::HomeUnset)?;
    Ok(PathBuf::from(home).join(".local/state"))
}

fn workspace_basename(workspace: &Path) -> Result<OsString, LockError> {
    let canonical =
        workspace
            .canonicalize()
            .map_err(|source| LockError::CanonicalizeWorkspace {
                path: workspace.to_path_buf(),
                source,
            })?;
    canonical
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .ok_or_else(|| LockError::WorkspaceNoBasename {
            path: canonical.clone(),
        })
}

async fn acquire_with_timeout<F>(
    path: &Path,
    timeout: Duration,
    clock: &dyn Clock,
    on_busy: F,
) -> Result<LockGuard, LockError>
where
    F: FnOnce() -> LockError,
{
    let deadline = clock.now() + timeout;
    loop {
        let file = open_lock_file(path)?;
        if let Some(guard) = try_lock_file(file)? {
            return Ok(guard);
        }
        if clock.now() >= deadline {
            return Err(on_busy());
        }
        clock.sleep(POLL_INTERVAL).await;
    }
}

fn try_lock_file(file: File) -> Result<Option<LockGuard>, LockError> {
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(Some(LockGuard {
            file,
            workspace: None,
            workspace_lock: None,
        })),
        Err(error) => {
            let io_error = std::io::Error::from(error);
            if io_error.kind() == std::io::ErrorKind::WouldBlock {
                Ok(None)
            } else {
                Err(LockError::Io(io_error))
            }
        }
    }
}

fn open_lock_file(path: &Path) -> Result<File, LockError> {
    File::options()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|source| LockError::OpenFile {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::MockClock;
    use anyhow::Result;

    #[test]
    fn with_state_home_creates_locks_directory_outside_workspace() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let state_home = tempfile::tempdir()?;
        let mgr = LockManager::with_state_home(workspace.path(), state_home.path())?;

        assert!(mgr.locks_dir().is_dir());
        let basename = workspace
            .path()
            .canonicalize()?
            .file_name()
            .map(std::ffi::OsStr::to_os_string)
            .ok_or_else(|| anyhow::anyhow!("workspace tempdir has no basename"))?;
        let expected = state_home.path().join("loom/locks").join(&basename);
        assert_eq!(mgr.locks_dir(), expected.as_path());
        assert!(!mgr.locks_dir().starts_with(workspace.path()));
        Ok(())
    }

    #[test]
    fn drop_releases_so_reacquire_succeeds() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let state_home = tempfile::tempdir()?;
        let mgr = LockManager::with_state_home(workspace.path(), state_home.path())?;
        let root = BeadId::new("lm-alpha")?;
        {
            let _guard = mgr.acquire_work_root(&root)?;
        }
        let _guard = mgr.acquire_work_root(&root)?;
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn acquire_work_root_async_times_out_via_mock_clock() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let state_home = tempfile::tempdir()?;
        let mgr = LockManager::with_state_home(workspace.path(), state_home.path())?;
        let root = BeadId::new("lm-contended")?;
        let clock = MockClock::new();
        let _holder = mgr.acquire_work_root_async(&root, &clock).await?;

        let result = mgr
            .acquire_work_root_with_timeout_async(&root, &clock, Duration::from_millis(250))
            .await;
        match result {
            Err(LockError::WorkRootBusy { root: ref r }) if r == "lm-contended" => Ok(()),
            other => Err(anyhow::anyhow!(
                "expected WorkRootBusy(lm-contended), got {other:?}"
            )),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn acquire_work_root_async_succeeds_after_holder_drops() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let state_home = tempfile::tempdir()?;
        let mgr = LockManager::with_state_home(workspace.path(), state_home.path())?;
        let root = BeadId::new("lm-handoff")?;
        let clock = MockClock::new();
        let holder = mgr.acquire_work_root_async(&root, &clock).await?;
        drop(holder);

        let _guard = mgr
            .acquire_work_root_with_timeout_async(&root, &clock, Duration::from_secs(1))
            .await?;
        Ok(())
    }
}

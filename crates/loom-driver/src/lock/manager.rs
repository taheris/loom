use std::ffi::OsString;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::Duration;

use fd_lock::RwLock;

use crate::clock::{Clock, SystemClock};
use crate::identifier::BeadId;

use super::error::{LockError, PhaseLock};

const WORKSPACE_LOCK_STEM: &str = "workspace";
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

pub struct LockManager {
    locks_dir: PathBuf,
}

pub struct LockGuard {
    _lock: Box<RwLock<File>>,
}

impl std::fmt::Debug for LockGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockGuard").finish_non_exhaustive()
    }
}

impl LockManager {
    pub fn new(workspace: impl AsRef<Path>) -> Result<Self, LockError> {
        let state_home = resolve_xdg_state_home()?;
        Self::with_state_home(workspace, state_home)
    }

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
        Ok(Self { locks_dir })
    }

    pub fn locks_dir(&self) -> &Path {
        &self.locks_dir
    }

    pub fn acquire_planning(&self) -> Result<LockGuard, LockError> {
        self.acquire_phase(PhaseLock::Planning)
    }

    pub fn acquire_todo(&self) -> Result<LockGuard, LockError> {
        self.acquire_phase(PhaseLock::Todo)
    }

    pub fn acquire_phase(&self, phase: PhaseLock) -> Result<LockGuard, LockError> {
        self.acquire_phase_with_timeout(phase, DEFAULT_LOCK_TIMEOUT)
    }

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

    pub async fn acquire_phase_async(
        &self,
        phase: PhaseLock,
        clock: &dyn Clock,
    ) -> Result<LockGuard, LockError> {
        self.acquire_phase_with_timeout_async(phase, clock, DEFAULT_LOCK_TIMEOUT)
            .await
    }

    pub async fn acquire_phase_with_timeout_async(
        &self,
        phase: PhaseLock,
        clock: &dyn Clock,
        timeout: Duration,
    ) -> Result<LockGuard, LockError> {
        let path = self.phase_lock_path(phase);
        acquire_with_timeout(&path, timeout, clock, || LockError::PhaseBusy { phase }).await
    }

    pub fn acquire_work_root(&self, root: &BeadId) -> Result<LockGuard, LockError> {
        self.acquire_work_root_with_timeout(root, DEFAULT_LOCK_TIMEOUT)
    }

    pub fn acquire_work_root_with_timeout(
        &self,
        root: &BeadId,
        timeout: Duration,
    ) -> Result<LockGuard, LockError> {
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

    pub async fn acquire_work_root_async(
        &self,
        root: &BeadId,
        clock: &dyn Clock,
    ) -> Result<LockGuard, LockError> {
        self.acquire_work_root_with_timeout_async(root, clock, DEFAULT_LOCK_TIMEOUT)
            .await
    }

    pub async fn acquire_work_root_with_timeout_async(
        &self,
        root: &BeadId,
        clock: &dyn Clock,
        timeout: Duration,
    ) -> Result<LockGuard, LockError> {
        let path = self.work_root_lock_path(root);
        acquire_with_timeout(&path, timeout, clock, || LockError::WorkRootBusy {
            root: root.to_string(),
        })
        .await
    }

    pub fn acquire_workspace(&self) -> Result<LockGuard, LockError> {
        if let Some(root) = self.find_held_mutating_lock()? {
            return Err(LockError::WorkspaceBusy { root });
        }
        let path = self.locks_dir.join(format!("{WORKSPACE_LOCK_STEM}.lock"));
        let file = open_lock_file(&path)?;
        let mut lock = Box::new(RwLock::new(file));
        if try_lock_and_forget(&mut lock) {
            Ok(LockGuard { _lock: lock })
        } else {
            Err(LockError::WorkspaceBusy {
                root: WORKSPACE_LOCK_STEM.to_string(),
            })
        }
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
            let mut probe = RwLock::new(file);
            if probe.try_write().is_err() {
                return Ok(Some(stem.to_string()));
            }
        }
        Ok(None)
    }
}

fn resolve_xdg_state_home() -> Result<PathBuf, LockError> {
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
        .map(|n| n.to_os_string())
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
    let file = open_lock_file(path)?;
    let mut lock = Box::new(RwLock::new(file));
    let deadline = clock.now() + timeout;
    loop {
        if try_lock_and_forget(&mut lock) {
            return Ok(LockGuard { _lock: lock });
        }
        if clock.now() >= deadline {
            return Err(on_busy());
        }
        clock.sleep(POLL_INTERVAL).await;
    }
}

fn try_lock_and_forget(lock: &mut RwLock<File>) -> bool {
    if let Ok(guard) = lock.try_write() {
        std::mem::forget(guard);
        true
    } else {
        false
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
            .map(|n| n.to_os_string())
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

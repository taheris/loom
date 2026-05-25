use std::ffi::OsString;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::Duration;

use fd_lock::RwLock;

use crate::clock::{Clock, SystemClock};
use crate::identifier::SpecLabel;

use super::error::LockError;

/// Reserved spec label that maps to `workspace.lock`. Spec-scoped commands
/// reject this label so `acquire_spec("workspace")` cannot collide with
/// `acquire_workspace`.
pub const RESERVED_WORKSPACE_LABEL: &str = "workspace";

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const DEFAULT_SPEC_TIMEOUT: Duration = Duration::from_secs(5);

/// Resolves lock-file paths under
/// `$XDG_STATE_HOME/loom/locks/<workspace-basename>/` and hands out RAII
/// guards. Cheap to clone-by-construction (only stores the resolved
/// directory).
pub struct LockManager {
    locks_dir: PathBuf,
}

/// Holds an exclusive `flock(2)` until dropped. The kernel also releases the
/// lock if the process exits or crashes — closing the file descriptor (which
/// `Drop` does) is what releases it, and process exit closes every fd.
pub struct LockGuard {
    // The boxed `RwLock<File>` owns the file descriptor. Dropping it closes
    // the fd, which releases the kernel's flock. The acquired
    // `RwLockWriteGuard` is `mem::forget`-ed during acquisition so its `Drop`
    // — which would call `flock(LOCK_UN)` immediately — does not run.
    _lock: Box<RwLock<File>>,
}

impl std::fmt::Debug for LockGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockGuard").finish_non_exhaustive()
    }
}

impl LockManager {
    /// Build a manager rooted at
    /// `$XDG_STATE_HOME/loom/locks/<workspace-basename>/` (default
    /// `~/.local/state/loom/locks/<basename>/`), creating the directory if
    /// it doesn't yet exist. Locks live outside the workspace bind-mount so
    /// a bead container cannot `rm` them out from under the host driver.
    pub fn new(workspace: impl AsRef<Path>) -> Result<Self, LockError> {
        let state_home = resolve_xdg_state_home()?;
        Self::with_state_home(workspace, state_home)
    }

    /// Build a manager with an explicit state-home root. Production callers
    /// use [`Self::new`]; tests pass an isolated tempdir to avoid touching
    /// the real `~/.local/state`.
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

    /// Directory that holds every lock file. Test-only diagnostic accessor.
    pub fn locks_dir(&self) -> &Path {
        &self.locks_dir
    }

    /// Acquire `<label>.lock` exclusively, waiting up to 5 seconds before
    /// erroring with [`LockError::SpecBusy`]. Releases on guard drop.
    ///
    /// Sync wrapper around [`Self::acquire_spec_async`]; spins up a small
    /// `current_thread` tokio runtime under the hood so callers in sync
    /// contexts (the binary, `loom plan`, `loom use`) need not thread async
    /// through their stack just to take a lock.
    pub fn acquire_spec(&self, label: &SpecLabel) -> Result<LockGuard, LockError> {
        self.acquire_spec_with_timeout(label, DEFAULT_SPEC_TIMEOUT)
    }

    /// Same as [`Self::acquire_spec`] but with a caller-provided timeout. Used
    /// by tests to keep the contention path fast.
    pub fn acquire_spec_with_timeout(
        &self,
        label: &SpecLabel,
        timeout: Duration,
    ) -> Result<LockGuard, LockError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(LockError::RuntimeBuild)?;
        runtime.block_on(self.acquire_spec_with_timeout_async(label, &SystemClock::new(), timeout))
    }

    /// Async variant of [`Self::acquire_spec`]. Tests pass [`crate::clock::MockClock`]
    /// under `#[tokio::test(start_paused = true)]` so the polling loop
    /// advances deterministic time instead of sleeping wall-clock.
    pub async fn acquire_spec_async(
        &self,
        label: &SpecLabel,
        clock: &dyn Clock,
    ) -> Result<LockGuard, LockError> {
        self.acquire_spec_with_timeout_async(label, clock, DEFAULT_SPEC_TIMEOUT)
            .await
    }

    /// Async variant of [`Self::acquire_spec_with_timeout`].
    pub async fn acquire_spec_with_timeout_async(
        &self,
        label: &SpecLabel,
        clock: &dyn Clock,
        timeout: Duration,
    ) -> Result<LockGuard, LockError> {
        if label.as_str() == RESERVED_WORKSPACE_LABEL {
            return Err(LockError::ReservedLabel);
        }
        let path = self.spec_lock_path(label);
        acquire_with_timeout(&path, timeout, clock, || LockError::SpecBusy {
            label: label.to_string(),
        })
        .await
    }

    /// Acquire `workspace.lock` exclusively. Errors immediately with
    /// [`LockError::WorkspaceBusy`] if any per-spec lock is currently held —
    /// `loom init`/`loom init --rebuild` rebuilds the state DB, so it cannot
    /// run while another spec command is mutating state.
    pub fn acquire_workspace(&self) -> Result<LockGuard, LockError> {
        if let Some(busy_label) = self.find_held_spec_lock()? {
            return Err(LockError::WorkspaceBusy { label: busy_label });
        }
        let path = self
            .locks_dir
            .join(format!("{RESERVED_WORKSPACE_LABEL}.lock"));
        let file = open_lock_file(&path)?;
        let mut lock = Box::new(RwLock::new(file));
        if try_lock_and_forget(&mut lock) {
            Ok(LockGuard { _lock: lock })
        } else {
            Err(LockError::WorkspaceBusy {
                label: RESERVED_WORKSPACE_LABEL.to_string(),
            })
        }
    }

    fn spec_lock_path(&self, label: &SpecLabel) -> PathBuf {
        self.locks_dir.join(format!("{}.lock", label.as_str()))
    }

    /// Probe every `*.lock` file (except `workspace.lock`) for a current
    /// holder. Returns the label of the first held lock, or `None`.
    fn find_held_spec_lock(&self) -> Result<Option<String>, LockError> {
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
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s,
                None => continue,
            };
            if stem == RESERVED_WORKSPACE_LABEL {
                continue;
            }
            let file = open_lock_file(&path)?;
            let mut probe = RwLock::new(file);
            if probe.try_write().is_err() {
                return Ok(Some(stem.to_string()));
            }
            // probe drops here, releasing the lock the kernel granted.
        }
        Ok(None)
    }
}

/// Per XDG Base Directory: honor `XDG_STATE_HOME` when set and non-empty,
/// otherwise fall back to `$HOME/.local/state`.
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

/// Try to take an exclusive `flock(2)` on `lock`. On success, `mem::forget`
/// the guard so its `Drop` does not immediately release the lock — the
/// kernel will release on file-descriptor close instead. Returns `true` on
/// success. The split-out function is what lets the borrow on `lock` end
/// before we move it into [`LockGuard`] in the caller.
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
    fn acquire_spec_rejects_reserved_label() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let state_home = tempfile::tempdir()?;
        let mgr = LockManager::with_state_home(workspace.path(), state_home.path())?;
        let result = mgr.acquire_spec(&SpecLabel::new(RESERVED_WORKSPACE_LABEL));
        match result {
            Err(LockError::ReservedLabel) => Ok(()),
            other => Err(anyhow::anyhow!("expected ReservedLabel, got {other:?}")),
        }
    }

    #[test]
    fn drop_releases_so_reacquire_succeeds() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let state_home = tempfile::tempdir()?;
        let mgr = LockManager::with_state_home(workspace.path(), state_home.path())?;
        let label = SpecLabel::new("alpha");
        {
            let _g = mgr.acquire_spec(&label)?;
        }
        let _g2 = mgr.acquire_spec(&label)?;
        Ok(())
    }

    /// Async polling loop: holding the spec lock causes the second async
    /// acquire to error after the timeout passes — measured deterministically
    /// via [`MockClock`] under `start_paused = true`. No real wall-clock
    /// wait is performed.
    #[tokio::test(start_paused = true)]
    async fn acquire_spec_async_times_out_via_mock_clock() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let state_home = tempfile::tempdir()?;
        let mgr = LockManager::with_state_home(workspace.path(), state_home.path())?;
        let label = SpecLabel::new("contended");
        let clock = MockClock::new();
        // Hold the lock via the async path so we don't nest a `block_on` in
        // the existing tokio runtime.
        let _holder = mgr.acquire_spec_async(&label, &clock).await?;

        let result = mgr
            .acquire_spec_with_timeout_async(&label, &clock, Duration::from_millis(250))
            .await;
        match result {
            Err(LockError::SpecBusy { label: ref l }) if l == "contended" => Ok(()),
            other => Err(anyhow::anyhow!(
                "expected SpecBusy(contended), got {other:?}"
            )),
        }
    }

    /// Async path observes lock release without polling wall-clock: drop the
    /// holder then await — the next `try_lock` succeeds before the deadline.
    #[tokio::test(start_paused = true)]
    async fn acquire_spec_async_succeeds_after_holder_drops() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let state_home = tempfile::tempdir()?;
        let mgr = LockManager::with_state_home(workspace.path(), state_home.path())?;
        let label = SpecLabel::new("handoff");
        let clock = MockClock::new();
        let holder = mgr.acquire_spec_async(&label, &clock).await?;
        drop(holder);

        let _g = mgr
            .acquire_spec_with_timeout_async(&label, &clock, Duration::from_secs(1))
            .await?;
        Ok(())
    }
}

//! Integration tests for `loom_driver::lock::LockManager`.
//!
//! The crash-release test re-execs the test binary as a child to take and
//! abandon a lock: `flock(2)` release on process death is a kernel-level
//! guarantee tied to fd close on exit. Asserting it requires a real, reaped
//! subprocess. Tests construct managers via
//! `LockManager::with_state_home(workspace, state_home)` so the lock
//! directory lives under an isolated tempdir rather than the developer's
//! real `~/.local/state`.

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use loom_driver::identifier::BeadId;
use loom_driver::lock::{LockError, LockManager, PhaseLock};

static FORK_SERIALIZE: Mutex<()> = Mutex::new(());

#[test]
fn phase_and_work_root_locks_create_expected_files() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let state_home = tempfile::tempdir()?;
    let mgr = LockManager::with_state_home(dir.path(), state_home.path())?;
    let root = BeadId::new("lm-root")?;

    let _plan = mgr.acquire_planning()?;
    let _todo = mgr.acquire_todo()?;
    let _work = mgr.acquire_work_root(&root)?;

    for name in ["plan.lock", "todo.lock", "lm-root.lock"] {
        let path = mgr.locks_dir().join(name);
        if !path.is_file() {
            return Err(anyhow!("expected lock file at {}", path.display()));
        }
    }
    if mgr.locks_dir().starts_with(dir.path()) {
        return Err(anyhow!("locks dir must be outside workspace"));
    }
    Ok(())
}

#[test]
fn second_acquire_times_out_with_work_root_busy() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let state_home = tempfile::tempdir()?;
    let mgr = LockManager::with_state_home(dir.path(), state_home.path())?;
    let root = BeadId::new("lm-contended")?;
    let _holder = mgr.acquire_work_root(&root)?;

    let timeout = Duration::from_millis(250);
    let started = Instant::now();
    let result = mgr.acquire_work_root_with_timeout(&root, timeout);
    let waited = started.elapsed();

    match result {
        Err(LockError::WorkRootBusy { root: ref r }) if r == "lm-contended" => {}
        other => {
            return Err(anyhow!(
                "expected WorkRootBusy(lm-contended), got {other:?}"
            ));
        }
    }
    if waited < timeout {
        return Err(anyhow!(
            "second acquire returned early ({waited:?}) — should wait the full timeout"
        ));
    }
    if waited > Duration::from_secs(2) {
        return Err(anyhow!(
            "second acquire exceeded its bounded test deadline: {waited:?}"
        ));
    }
    Ok(())
}

#[test]
fn times_out_with_default_timeout() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let state_home = tempfile::tempdir()?;
    let mgr = LockManager::with_state_home(dir.path(), state_home.path())?;
    let root = BeadId::new("lm-default")?;
    let _holder = mgr.acquire_work_root(&root)?;

    let started = Instant::now();
    let result = mgr.acquire_work_root(&root);
    let waited = started.elapsed();

    match result {
        Err(LockError::WorkRootBusy { .. }) => {}
        other => return Err(anyhow!("expected WorkRootBusy, got {other:?}")),
    }
    if waited < Duration::from_millis(4_500) {
        return Err(anyhow!("default timeout wait too short: {waited:?}"));
    }
    if waited > Duration::from_millis(7_000) {
        return Err(anyhow!("default timeout wait too long: {waited:?}"));
    }
    Ok(())
}

#[test]
fn different_work_root_locks_do_not_block() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let state_home = tempfile::tempdir()?;
    let mgr = LockManager::with_state_home(dir.path(), state_home.path())?;
    let alpha = BeadId::new("lm-alpha")?;
    let beta = BeadId::new("lm-beta")?;

    let _alpha_guard = mgr.acquire_work_root(&alpha)?;

    let started = Instant::now();
    let _beta_guard = mgr.acquire_work_root(&beta)?;
    let waited = started.elapsed();

    if waited > Duration::from_millis(250) {
        return Err(anyhow!(
            "different work-root acquire blocked unexpectedly: {waited:?}"
        ));
    }
    Ok(())
}

#[test]
fn readonly_paths_unaffected_by_work_root_lock() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let state_home = tempfile::tempdir()?;
    let mgr = LockManager::with_state_home(dir.path(), state_home.path())?;
    let root = BeadId::new("lm-active")?;
    let _guard = mgr.acquire_work_root(&root)?;

    let started = Instant::now();
    let mgr2 = LockManager::with_state_home(dir.path(), state_home.path())?;
    let _ignored = mgr2.locks_dir().is_dir();
    let waited = started.elapsed();
    if waited > Duration::from_millis(100) {
        return Err(anyhow!("readonly inspection blocked: {waited:?}"));
    }

    let payload = dir.path().join("README");
    std::fs::write(&payload, "hello")?;
    let body = std::fs::read_to_string(&payload)?;
    if body != "hello" {
        return Err(anyhow!("workspace read returned wrong content: {body:?}"));
    }
    Ok(())
}

#[test]
fn acquire_workspace_errors_when_phase_or_work_root_lock_held() -> Result<()> {
    let _serialize = FORK_SERIALIZE.lock().expect("FORK_SERIALIZE poisoned");
    let dir = tempfile::tempdir()?;
    let state_home = tempfile::tempdir()?;
    let mgr = LockManager::with_state_home(dir.path(), state_home.path())?;

    {
        let _ws = mgr.acquire_workspace()?;
    }

    let plan = mgr.acquire_planning()?;
    match mgr.acquire_workspace() {
        Err(LockError::WorkspaceBusy { root }) if root == "plan" => {}
        other => return Err(anyhow!("expected WorkspaceBusy(plan), got {other:?}")),
    }
    drop(plan);

    let todo = mgr.acquire_todo()?;
    match mgr.acquire_workspace() {
        Err(LockError::WorkspaceBusy { root }) if root == "todo" => {}
        other => return Err(anyhow!("expected WorkspaceBusy(todo), got {other:?}")),
    }
    drop(todo);

    let root = BeadId::new("lm-busy")?;
    let work = mgr.acquire_work_root(&root)?;
    match mgr.acquire_workspace() {
        Err(LockError::WorkspaceBusy { root: ref r }) if r == "lm-busy" => {}
        other => return Err(anyhow!("expected WorkspaceBusy(lm-busy), got {other:?}")),
    }
    drop(work);

    let _ws_again = mgr.acquire_workspace()?;
    Ok(())
}

#[test]
fn acquire_workspace_serializes_workspace_holders() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let state_home = tempfile::tempdir()?;
    let mgr = LockManager::with_state_home(dir.path(), state_home.path())?;

    let _first = mgr.acquire_workspace()?;
    match mgr.acquire_workspace() {
        Err(LockError::WorkspaceBusy { root }) if root == "workspace" => Ok(()),
        other => Err(anyhow!("expected WorkspaceBusy(workspace), got {other:?}")),
    }
}

#[test]
#[ignore]
fn crash_helper_take_lock_then_exit() -> Result<()> {
    let workspace = std::env::var("LOOM_LOCK_TEST_DIR")?;
    let state_home = std::env::var("LOOM_LOCK_TEST_STATE_HOME")?;
    let root = std::env::var("LOOM_LOCK_TEST_ROOT")?;
    let mgr = LockManager::with_state_home(PathBuf::from(&workspace), PathBuf::from(&state_home))?;
    let _guard = mgr.acquire_work_root(&BeadId::new(&root)?)?;
    std::process::exit(0);
}

#[test]
fn crash_releases_work_root_lock() -> Result<()> {
    let _serialize = FORK_SERIALIZE.lock().expect("FORK_SERIALIZE poisoned");
    let dir = tempfile::tempdir()?;
    let state_home = tempfile::tempdir()?;
    let workspace = dir.path().to_path_buf();
    let root = BeadId::new("lm-crash")?;

    let exe = std::env::current_exe()?;
    let mut child = Command::new(&exe)
        .env("LOOM_LOCK_TEST_DIR", &workspace)
        .env("LOOM_LOCK_TEST_STATE_HOME", state_home.path())
        .env("LOOM_LOCK_TEST_ROOT", root.as_str())
        .args([
            "--ignored",
            "--exact",
            "crash_helper_take_lock_then_exit",
            "--nocapture",
        ])
        .spawn()?;
    let helper_deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= helper_deadline {
            child.kill()?;
            let killed = child.wait()?;
            return Err(anyhow!(
                "crash helper exceeded five-second deadline and was reaped: {killed:?}"
            ));
        }
        thread::sleep(Duration::from_millis(10));
    };
    if !status.success() {
        return Err(anyhow!("crash helper exited non-zero: {status:?}"));
    }

    let mgr = LockManager::with_state_home(&workspace, state_home.path())?;
    let started = Instant::now();
    let _guard = mgr.acquire_work_root(&root)?;
    let waited = started.elapsed();
    if waited > Duration::from_millis(250) {
        return Err(anyhow!(
            "post-crash acquire took {waited:?} — expected immediate"
        ));
    }
    Ok(())
}

#[test]
fn second_thread_unblocks_when_holder_drops() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let state_home = tempfile::tempdir()?;
    let workspace = dir.path().to_path_buf();
    let state_home_path = state_home.path().to_path_buf();
    let root = BeadId::new("lm-handoff")?;
    let mgr = LockManager::with_state_home(&workspace, &state_home_path)?;

    let holder = mgr.acquire_work_root(&root)?;

    let (tx, rx) = mpsc::channel::<Result<Duration, String>>();
    let root_clone = root.clone();
    let workspace_clone = workspace.clone();
    let state_home_clone = state_home_path.clone();
    let waiter = thread::spawn(move || {
        let mgr2 = match LockManager::with_state_home(&workspace_clone, &state_home_clone) {
            Ok(m) => m,
            Err(e) => {
                let _ = tx.send(Err(format!("manager: {e}")));
                return;
            }
        };
        let started = Instant::now();
        match mgr2.acquire_work_root_with_timeout(&root_clone, Duration::from_secs(3)) {
            Ok(_guard) => {
                let _ = tx.send(Ok(started.elapsed()));
            }
            Err(e) => {
                let _ = tx.send(Err(format!("acquire: {e}")));
            }
        }
    });

    thread::sleep(Duration::from_millis(150));
    drop(holder);

    let elapsed = match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(d)) => d,
        Ok(Err(msg)) => return Err(anyhow!("waiter failed: {msg}")),
        Err(e) => return Err(anyhow!("waiter timed out: {e}")),
    };
    waiter
        .join()
        .map_err(|_| anyhow!("waiter thread panicked"))?;

    if elapsed > Duration::from_millis(800) {
        return Err(anyhow!("handoff took too long: {elapsed:?}"));
    }
    Ok(())
}

#[test]
fn locks_outside_workspace() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let state_home = tempfile::tempdir()?;
    let workspace = dir.path();
    let mgr = LockManager::with_state_home(workspace, state_home.path())?;

    let canonical_workspace = workspace.canonicalize()?;
    let canonical_locks = mgr.locks_dir().canonicalize()?;
    if canonical_locks.starts_with(&canonical_workspace) {
        return Err(anyhow!(
            "locks dir {} lives inside workspace {}",
            canonical_locks.display(),
            canonical_workspace.display(),
        ));
    }

    let basename = canonical_workspace
        .file_name()
        .ok_or_else(|| anyhow!("workspace has no basename"))?;
    let expected = state_home.path().join("loom/locks").join(basename);
    if mgr.locks_dir().canonicalize()? != expected.canonicalize()? {
        return Err(anyhow!(
            "expected locks_dir {}, got {}",
            expected.display(),
            mgr.locks_dir().display(),
        ));
    }

    let _plan = mgr.acquire_phase(PhaseLock::Planning)?;
    drop(_plan);
    let _work = mgr.acquire_work_root(&BeadId::new("lm-alpha")?)?;
    drop(_work);
    let _ws = mgr.acquire_workspace()?;
    drop(_ws);

    let mut intruders = Vec::new();
    walk_collect_locks(workspace, &mut intruders)?;
    if !intruders.is_empty() {
        return Err(anyhow!(
            "found lock files inside workspace bind-mount: {intruders:?}"
        ));
    }
    Ok(())
}

#[test]
fn container_cannot_rm_host_lock() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let state_home = tempfile::tempdir()?;
    let workspace = dir.path().to_path_buf();
    let mgr = LockManager::with_state_home(&workspace, state_home.path())?;
    let root = BeadId::new("lm-contended")?;

    let _holder = mgr.acquire_work_root(&root)?;

    for entry in std::fs::read_dir(&workspace)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }

    let lock_path = mgr.locks_dir().join("lm-contended.lock");
    if !lock_path.is_file() {
        return Err(anyhow!(
            "host lock {} disappeared after wiping workspace",
            lock_path.display(),
        ));
    }
    let result = mgr.acquire_work_root_with_timeout(&root, Duration::from_millis(100));
    match result {
        Err(LockError::WorkRootBusy { root: ref r }) if r == "lm-contended" => Ok(()),
        other => Err(anyhow!(
            "mutual exclusion broken after workspace wipe: {other:?}"
        )),
    }
}

fn walk_collect_locks(root: &std::path::Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            walk_collect_locks(&path, out)?;
        } else if file_type.is_file() && path.extension().and_then(|e| e.to_str()) == Some("lock") {
            out.push(path);
        }
    }
    Ok(())
}

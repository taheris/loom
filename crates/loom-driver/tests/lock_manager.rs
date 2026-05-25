//! Integration tests for `loom_driver::lock::LockManager`.
//!
//! `crash_releases_spec_lock` re-execs the test binary as a child to take
//! and abandon a lock (spec NFR #8): `flock(2)` release on process death
//! is a kernel-level guarantee tied to fd close on exit. Asserting it
//! requires a real, reaped subprocess; an in-process `LineParse +
//! tokio::io::duplex` substitute cannot reach the kernel-side fd table
//! that owns the OFD. The default-timeout test (5 s wall clock) lives at
//! the integration tier so the *real* `acquire_spec` API is exercised
//! end-to-end; the deterministic `MockClock`-driven variant lives inline
//! at `loom-driver/src/lock/manager.rs::tests`.
//!
//! Tests construct managers via `LockManager::with_state_home(workspace,
//! state_home)` so the lock directory lives under an isolated tempdir
//! rather than the developer's real `~/.local/state`. The fork test
//! threads the state-home tempdir through to the child process via the
//! `LOOM_LOCK_TEST_STATE_HOME` env var.

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use loom_driver::identifier::SpecLabel;
use loom_driver::lock::{LockError, LockManager};

/// Serializes tests that fork while another test is mid drop+reacquire of a
/// spec lock — the child inherits the fd and `flock(2)` is per-OFD, so the
/// "released" lock still appears held until execve closes CLOEXEC fds.
static FORK_SERIALIZE: Mutex<()> = Mutex::new(());

#[test]
fn acquire_spec_creates_lock_file() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let state_home = tempfile::tempdir()?;
    let mgr = LockManager::with_state_home(dir.path(), state_home.path())?;
    let label = SpecLabel::new("alpha");

    let _guard = mgr.acquire_spec(&label)?;

    let lock_path = mgr.locks_dir().join("alpha.lock");
    if !lock_path.is_file() {
        return Err(anyhow!("expected lock file at {}", lock_path.display()));
    }
    Ok(())
}

#[test]
fn second_acquire_times_out_with_spec_busy() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let state_home = tempfile::tempdir()?;
    let mgr = LockManager::with_state_home(dir.path(), state_home.path())?;
    let label = SpecLabel::new("contended");

    // First acquisition holds the lock for the whole test.
    let _holder = mgr.acquire_spec(&label)?;

    // Use a sub-second timeout for the test itself; the *default* 5-second
    // wait is exercised by `times_out_with_default_timeout` so this test
    // doesn't add 5s to every cargo run.
    let timeout = Duration::from_millis(250);
    let started = Instant::now();
    let result = mgr.acquire_spec_with_timeout(&label, timeout);
    let waited = started.elapsed();

    match result {
        Err(LockError::SpecBusy { label: ref l }) if l == "contended" => {}
        other => return Err(anyhow!("expected SpecBusy(contended), got {other:?}")),
    }
    if waited < timeout {
        return Err(anyhow!(
            "second acquire returned early ({waited:?}) — should wait the full timeout"
        ));
    }
    Ok(())
}

#[test]
fn times_out_with_default_timeout() -> Result<()> {
    // Smoke-check the default 5s wait advertised in the spec without
    // dragging every test run by 5s. We use the explicit default by relying
    // on `acquire_spec` (no override) but only assert directionally.
    let dir = tempfile::tempdir()?;
    let state_home = tempfile::tempdir()?;
    let mgr = LockManager::with_state_home(dir.path(), state_home.path())?;
    let label = SpecLabel::new("default-timeout");
    let _holder = mgr.acquire_spec(&label)?;

    let started = Instant::now();
    let result = mgr.acquire_spec(&label);
    let waited = started.elapsed();

    match result {
        Err(LockError::SpecBusy { .. }) => {}
        other => return Err(anyhow!("expected SpecBusy, got {other:?}")),
    }
    // The default is 5s; allow some scheduler slop on either side.
    if waited < Duration::from_millis(4_500) {
        return Err(anyhow!("default timeout wait too short: {waited:?}"));
    }
    if waited > Duration::from_millis(7_000) {
        return Err(anyhow!("default timeout wait too long: {waited:?}"));
    }
    Ok(())
}

#[test]
fn cross_spec_locks_do_not_block() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let state_home = tempfile::tempdir()?;
    let mgr = LockManager::with_state_home(dir.path(), state_home.path())?;
    let alpha = SpecLabel::new("alpha");
    let beta = SpecLabel::new("beta");

    let _alpha_guard = mgr.acquire_spec(&alpha)?;

    let started = Instant::now();
    let _beta_guard = mgr.acquire_spec(&beta)?;
    let waited = started.elapsed();

    if waited > Duration::from_millis(250) {
        return Err(anyhow!(
            "cross-spec acquire blocked unexpectedly: {waited:?}"
        ));
    }
    Ok(())
}

#[test]
fn readonly_paths_unaffected_by_spec_lock() -> Result<()> {
    // Read-only commands (status, logs, spec) acquire no lock. We cannot
    // test the CLI from the lock module directly, but we verify the property
    // at this layer: holding a spec lock does not impede unrelated workspace
    // file I/O or a separate `LockManager::with_state_home` for inspection.
    let dir = tempfile::tempdir()?;
    let state_home = tempfile::tempdir()?;
    let mgr = LockManager::with_state_home(dir.path(), state_home.path())?;
    let label = SpecLabel::new("active-run");
    let _guard = mgr.acquire_spec(&label)?;

    // Re-open the manager (a read-only command would do the same to inspect
    // the locks dir) — must not block or error.
    let started = Instant::now();
    let mgr2 = LockManager::with_state_home(dir.path(), state_home.path())?;
    let _ignored = mgr2.locks_dir().is_dir();
    let waited = started.elapsed();
    if waited > Duration::from_millis(100) {
        return Err(anyhow!("readonly inspection blocked: {waited:?}"));
    }

    // Reading any file in the workspace must work even while the spec lock
    // is held — flock(2) on the lock file does not propagate to siblings.
    let payload = dir.path().join("README");
    std::fs::write(&payload, "hello")?;
    let body = std::fs::read_to_string(&payload)?;
    if body != "hello" {
        return Err(anyhow!("workspace read returned wrong content: {body:?}"));
    }
    Ok(())
}

#[test]
fn acquire_workspace_errors_when_spec_lock_held() -> Result<()> {
    let _serialize = FORK_SERIALIZE.lock().expect("FORK_SERIALIZE poisoned");
    let dir = tempfile::tempdir()?;
    let state_home = tempfile::tempdir()?;
    let mgr = LockManager::with_state_home(dir.path(), state_home.path())?;

    // With no spec lock held, workspace acquires cleanly.
    {
        let _ws = mgr.acquire_workspace()?;
    }

    let label = SpecLabel::new("busy-spec");
    let _spec = mgr.acquire_spec(&label)?;
    match mgr.acquire_workspace() {
        Err(LockError::WorkspaceBusy { label: ref l }) if l == "busy-spec" => {}
        other => return Err(anyhow!("expected WorkspaceBusy(busy-spec), got {other:?}")),
    }

    drop(_spec);
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
        Err(LockError::WorkspaceBusy { label: ref l }) if l == "workspace" => Ok(()),
        other => Err(anyhow!("expected WorkspaceBusy(workspace), got {other:?}")),
    }
}

/// Helper test — invoked as a child process by `crash_releases_spec_lock`.
/// `#[ignore]` so plain `cargo test` does not run it as part of the suite.
/// The parent passes `LOOM_LOCK_TEST_DIR` and `LOOM_LOCK_TEST_STATE_HOME`
/// to thread the workspace + isolated state-home into the child.
#[test]
#[ignore]
fn crash_helper_take_lock_then_exit() -> Result<()> {
    let workspace = std::env::var("LOOM_LOCK_TEST_DIR")?;
    let state_home = std::env::var("LOOM_LOCK_TEST_STATE_HOME")?;
    let label_str = std::env::var("LOOM_LOCK_TEST_LABEL")?;
    let mgr = LockManager::with_state_home(PathBuf::from(&workspace), PathBuf::from(&state_home))?;
    let _guard = mgr.acquire_spec(&SpecLabel::new(label_str))?;
    // Exit without unwinding: kernel closes the open fd, releasing flock.
    // This is the same effect as a SIGKILL'd process — proves the spec
    // claim that crashed processes leave no stale locks.
    std::process::exit(0);
}

#[test]
fn crash_releases_spec_lock() -> Result<()> {
    let _serialize = FORK_SERIALIZE.lock().expect("FORK_SERIALIZE poisoned");
    let dir = tempfile::tempdir()?;
    let state_home = tempfile::tempdir()?;
    let workspace = dir.path().to_path_buf();
    let label = "crash-test";

    let exe = std::env::current_exe()?;
    let status = Command::new(&exe)
        .env("LOOM_LOCK_TEST_DIR", &workspace)
        .env("LOOM_LOCK_TEST_STATE_HOME", state_home.path())
        .env("LOOM_LOCK_TEST_LABEL", label)
        .args([
            "--ignored",
            "--exact",
            "crash_helper_take_lock_then_exit",
            "--nocapture",
        ])
        .status()?;
    if !status.success() {
        return Err(anyhow!("crash helper exited non-zero: {status:?}"));
    }

    // Child process has fully exited; the kernel released its flock when
    // the fd closed at exit. A fresh acquire must succeed immediately.
    let mgr = LockManager::with_state_home(&workspace, state_home.path())?;
    let started = Instant::now();
    let _guard = mgr.acquire_spec(&SpecLabel::new(label))?;
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
    // Sanity: serialization works across threads, and once the first holder
    // releases the lock, the waiter completes.
    let dir = tempfile::tempdir()?;
    let state_home = tempfile::tempdir()?;
    let workspace = dir.path().to_path_buf();
    let state_home_path = state_home.path().to_path_buf();
    let label = SpecLabel::new("handoff");
    let mgr = LockManager::with_state_home(&workspace, &state_home_path)?;

    let holder = mgr.acquire_spec(&label)?;

    let (tx, rx) = mpsc::channel::<Result<Duration, String>>();
    let label_clone = label.clone();
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
        match mgr2.acquire_spec_with_timeout(&label_clone, Duration::from_secs(3)) {
            Ok(_g) => {
                let _ = tx.send(Ok(started.elapsed()));
            }
            Err(e) => {
                let _ = tx.send(Err(format!("acquire: {e}")));
            }
        }
    });

    // Give the waiter time to begin polling.
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

/// Spec acceptance: lock files live under
/// `$XDG_STATE_HOME/loom/locks/<workspace-basename>/`, NOT inside the
/// workspace bind-mount. After acquiring every lock kind (per-spec and
/// workspace), the workspace tree must contain zero `*.lock` files.
#[test]
fn locks_outside_workspace() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let state_home = tempfile::tempdir()?;
    let workspace = dir.path();
    let mgr = LockManager::with_state_home(workspace, state_home.path())?;

    // Resolve the locks dir and assert it is outside the workspace tree
    // (the bead container has the workspace bind-mounted, not state_home).
    let canonical_workspace = workspace.canonicalize()?;
    let canonical_locks = mgr.locks_dir().canonicalize()?;
    if canonical_locks.starts_with(&canonical_workspace) {
        return Err(anyhow!(
            "locks dir {} lives inside workspace {} (spec violation)",
            canonical_locks.display(),
            canonical_workspace.display(),
        ));
    }

    // Resolved layout matches the spec: <state_home>/loom/locks/<basename>.
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

    // Acquire one of each lock kind so files actually exist on disk.
    let _spec = mgr.acquire_spec(&SpecLabel::new("alpha"))?;
    drop(_spec);
    let _ws = mgr.acquire_workspace()?;
    drop(_ws);

    // Walk the workspace and assert no `*.lock` files were created inside it.
    let mut intruders = Vec::new();
    walk_collect_locks(workspace, &mut intruders)?;
    if !intruders.is_empty() {
        return Err(anyhow!(
            "found lock files inside workspace bind-mount: {intruders:?}"
        ));
    }
    Ok(())
}

/// Spec acceptance: removing any file inside the workspace bind-mount
/// cannot break mutual exclusion, because the host-side lock lives outside
/// the bind-mount. Simulates the container by `rm -rf`-ing the workspace
/// while the lock is held; the second acquirer must still see the lock as
/// busy.
#[test]
fn container_cannot_rm_host_lock() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let state_home = tempfile::tempdir()?;
    let workspace = dir.path().to_path_buf();
    let mgr = LockManager::with_state_home(&workspace, state_home.path())?;
    let label = SpecLabel::new("contended");

    let _holder = mgr.acquire_spec(&label)?;

    // Simulate the bead container nuking everything it can reach inside
    // the workspace bind-mount. The host lock is in state_home, so
    // mutual exclusion must survive.
    for entry in std::fs::read_dir(&workspace)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }

    // The locks_dir lives outside the workspace, so the file must still
    // exist and a second acquirer must see the lock as busy.
    let lock_path = mgr.locks_dir().join("contended.lock");
    if !lock_path.is_file() {
        return Err(anyhow!(
            "host lock {} disappeared after wiping workspace",
            lock_path.display(),
        ));
    }
    let result = mgr.acquire_spec_with_timeout(&label, Duration::from_millis(100));
    match result {
        Err(LockError::SpecBusy { label: ref l }) if l == "contended" => Ok(()),
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

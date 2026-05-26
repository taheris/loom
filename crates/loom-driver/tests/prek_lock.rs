//! Integration tests for `lib/prek/lock.sh` and the
//! `lib/prek/hooks/{pre-commit,pre-push}` shims.
//!
//! Tests source the shell scripts into bash subshells against a tempdir
//! workspace and assert against on-disk state. Worker-context tests set
//! `core.hooksPath` to point at the real shims under `lib/prek/hooks/`,
//! drop a stub `prek` onto `PATH`, and verify that `git commit` /
//! `git push` from a linked worktree fire the configured shims.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use tempfile::TempDir;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("CARGO_MANIFEST_DIR has parent (crates/)")
        .parent()
        .expect("crates/ has parent (workspace root)")
        .to_path_buf()
}

fn lock_sh() -> PathBuf {
    workspace_root().join("lib/prek/lock.sh")
}

fn hooks_dir() -> PathBuf {
    workspace_root().join("lib/prek/hooks")
}

fn git_in(repo: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .with_context(|| format!("spawn git {args:?}"))?;
    if !status.success() {
        bail!("git {args:?} in {} exited with {status}", repo.display());
    }
    Ok(())
}

fn init_repo(dir: &Path) -> Result<()> {
    git_in(dir, &["init", "-q", "-b", "main"])?;
    git_in(dir, &["config", "user.email", "test@example.com"])?;
    git_in(dir, &["config", "user.name", "Prek Tester"])?;
    git_in(dir, &["config", "commit.gpgsign", "false"])?;
    fs::write(dir.join("README.md"), "init\n")?;
    git_in(dir, &["add", "README.md"])?;
    git_in(dir, &["commit", "-q", "-m", "initial"])?;
    Ok(())
}

fn workspace_basename(dir: &Path) -> Result<String> {
    let canon = dir.canonicalize()?;
    let name = canon
        .file_name()
        .ok_or_else(|| anyhow!("path has no basename: {}", canon.display()))?;
    Ok(name.to_string_lossy().into_owned())
}

fn make_executable(path: &Path, body: &str) -> Result<()> {
    fs::write(path, body)?;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
}

fn path_with(prefix: &Path) -> String {
    let mut parts = vec![prefix.display().to_string()];
    if let Ok(existing) = std::env::var("PATH") {
        parts.push(existing);
    }
    parts.join(":")
}

fn git_with_path(repo: &Path, path: &str, state_home: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("PATH", path)
        .env("XDG_STATE_HOME", state_home)
        .env("HOME", state_home)
        .status()?;
    if !status.success() {
        bail!("git {args:?} in {} failed: {status}", repo.display());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Lock-script tests (SC 176, 179, 183, 218).
// ---------------------------------------------------------------------------

#[test]
fn dead_pid_lock_is_reclaimed() -> Result<()> {
    let workspace = TempDir::new()?;
    let state_home = TempDir::new()?;
    let signals = TempDir::new()?;
    init_repo(workspace.path())?;

    // The dead-PID branch only fires when flock(9) is held by *someone else*
    // while the PID stamped in the lock file is already dead — the exact
    // subprocess-discipline-violation scenario the recovery covers. We
    // simulate it by acquiring the lock from a bash holder that forks a
    // `sleep` child inheriting fd 9, then the holder exits. After exit, the
    // file still contains the holder's (dead) PID, and the orphan `sleep`
    // keeps the flock alive.
    let orphan_pid_file = signals.path().join("orphan-pid");
    let holder_script = format!(
        "set -euo pipefail
. \"{lock_sh}\"
_prek_acquire_lock
( exec sleep 60 ) &
printf '%s\\n' \"$!\" > \"{orphan}\"
",
        lock_sh = lock_sh().display(),
        orphan = orphan_pid_file.display(),
    );

    let holder_status = Command::new("bash")
        .arg("-c")
        .arg(&holder_script)
        .current_dir(workspace.path())
        .env("XDG_STATE_HOME", state_home.path())
        .env("HOME", state_home.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("spawn lock holder")?;
    if !holder_status.success() {
        bail!("holder exited non-zero: {holder_status}");
    }

    let orphan_pid: i32 = fs::read_to_string(&orphan_pid_file)?.trim().parse()?;

    // The acquirer should observe flock-busy + stamped-PID-dead, log the
    // reclaim, and obtain the lock on a fresh inode.
    let acquire_script = format!(
        "set -euo pipefail\n. \"{}\"\n_prek_acquire_lock\necho acquired\n",
        lock_sh().display(),
    );
    let started = Instant::now();
    let out = Command::new("bash")
        .arg("-c")
        .arg(&acquire_script)
        .current_dir(workspace.path())
        .env("XDG_STATE_HOME", state_home.path())
        .env("HOME", state_home.path())
        .output()
        .context("spawn bash for dead-pid acquire")?;
    let waited = started.elapsed();

    // Always clean up the orphan, even on failure paths below.
    let _ = Command::new("kill")
        .arg("-9")
        .arg(orphan_pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        bail!(
            "lock acquire failed: status={} stdout={stdout:?} stderr={stderr:?}",
            out.status,
        );
    }
    if !stdout.contains("acquired") {
        bail!("expected `acquired` in stdout; got {stdout:?}");
    }
    if !stderr.contains("reclaiming lock from dead PID") {
        bail!("expected dead-PID reclaim notice on stderr; got {stderr:?}");
    }
    if waited > Duration::from_secs(5) {
        bail!("dead-PID reclamation took too long: {waited:?}");
    }
    Ok(())
}

#[test]
fn concurrent_acquisitions_serialize() -> Result<()> {
    let workspace = TempDir::new()?;
    let state_home = TempDir::new()?;
    let signals = TempDir::new()?;
    init_repo(workspace.path())?;

    let first_acq = signals.path().join("first-acquired");
    let first_rel = signals.path().join("first-releasing");
    let second_acq = signals.path().join("second-acquired");

    let hold_script = format!(
        "set -euo pipefail\n. \"{lock_sh}\"\n_prek_acquire_lock\ntouch \"{first_acq}\"\nsleep 2\ntouch \"{first_rel}\"\n",
        lock_sh = lock_sh().display(),
        first_acq = first_acq.display(),
        first_rel = first_rel.display(),
    );
    let wait_script = format!(
        "set -euo pipefail\n. \"{lock_sh}\"\n_prek_acquire_lock\ntouch \"{second_acq}\"\n",
        lock_sh = lock_sh().display(),
        second_acq = second_acq.display(),
    );

    let mut first = Command::new("bash")
        .arg("-c")
        .arg(&hold_script)
        .current_dir(workspace.path())
        .env("XDG_STATE_HOME", state_home.path())
        .env("HOME", state_home.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn first acquirer")?;

    let deadline = Instant::now() + Duration::from_secs(5);
    while !first_acq.exists() {
        if Instant::now() > deadline {
            let _ = first.kill();
            bail!("first acquirer never reached the acquired state");
        }
        thread::sleep(Duration::from_millis(50));
    }

    let mut second = Command::new("bash")
        .arg("-c")
        .arg(&wait_script)
        .current_dir(workspace.path())
        .env("XDG_STATE_HOME", state_home.path())
        .env("HOME", state_home.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn second acquirer")?;

    thread::sleep(Duration::from_millis(500));
    if second_acq.exists() {
        let _ = first.kill();
        let _ = second.kill();
        bail!("second acquirer obtained lock while first was still holding");
    }

    let first_status = first.wait()?;
    if !first_status.success() {
        let _ = second.kill();
        bail!("first acquirer exited non-zero: {first_status}");
    }
    if !first_rel.exists() {
        bail!("first acquirer never reached the release sentinel");
    }

    let second_status = second.wait()?;
    if !second_status.success() {
        bail!("second acquirer exited non-zero: {second_status}");
    }
    if !second_acq.exists() {
        bail!("second acquirer never acquired after first released");
    }
    Ok(())
}

#[test]
fn linked_worktrees_share_lock() -> Result<()> {
    let workspace = TempDir::new()?;
    let state_home = TempDir::new()?;
    init_repo(workspace.path())?;

    let wt_path = workspace.path().join(".wrapix/worktree/feat/lm-test.0");
    fs::create_dir_all(wt_path.parent().expect("worktree parent"))?;
    git_in(
        workspace.path(),
        &[
            "worktree",
            "add",
            "-q",
            &wt_path.display().to_string(),
            "-b",
            "loom/feat/lm-test.0",
        ],
    )?;

    let acquire_script = format!(
        "set -euo pipefail\n. \"{}\"\n_prek_acquire_lock\n",
        lock_sh().display(),
    );

    // Acquire from the main checkout.
    let out = Command::new("bash")
        .arg("-c")
        .arg(&acquire_script)
        .current_dir(workspace.path())
        .env("XDG_STATE_HOME", state_home.path())
        .env("HOME", state_home.path())
        .output()?;
    if !out.status.success() {
        bail!(
            "main acquire failed: {}",
            String::from_utf8_lossy(&out.stderr),
        );
    }

    let expected_basename = workspace_basename(workspace.path())?;
    let expected_lock = state_home
        .path()
        .join("loom/prek")
        .join(&expected_basename)
        .join("prek.lock");
    if !expected_lock.is_file() {
        bail!(
            "expected lock at {} after main acquire; missing",
            expected_lock.display(),
        );
    }

    // Acquire from the linked worktree; the same lock file must be touched.
    let out2 = Command::new("bash")
        .arg("-c")
        .arg(&acquire_script)
        .current_dir(&wt_path)
        .env("XDG_STATE_HOME", state_home.path())
        .env("HOME", state_home.path())
        .output()?;
    if !out2.status.success() {
        bail!(
            "worktree acquire failed: {}",
            String::from_utf8_lossy(&out2.stderr),
        );
    }

    let prek_dir = state_home.path().join("loom/prek");
    let mut entries: Vec<String> = fs::read_dir(&prek_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    if entries != vec![expected_basename.clone()] {
        bail!(
            "expected only one workspace dir under {}; got {entries:?}",
            prek_dir.display(),
        );
    }

    // The worktree-side acquire should have re-stamped the same lock file
    // with its own PID (not created a separate file).
    let lock_pid_after = fs::read_to_string(&expected_lock)?;
    if lock_pid_after.trim().is_empty() {
        bail!("lock file empty after worktree acquire — not re-stamped");
    }
    Ok(())
}

#[test]
fn stamp_shared_across_worktrees() -> Result<()> {
    let workspace = TempDir::new()?;
    let state_home = TempDir::new()?;
    init_repo(workspace.path())?;

    let wt_path = workspace.path().join(".wrapix/worktree/feat/lm-test.0");
    fs::create_dir_all(wt_path.parent().expect("worktree parent"))?;
    git_in(
        workspace.path(),
        &[
            "worktree",
            "add",
            "-q",
            &wt_path.display().to_string(),
            "-b",
            "loom/feat/lm-test.0",
        ],
    )?;

    // Probe the stamp-path derivation from each context. Both must resolve
    // to the same `<workspace-basename>` subdir under `$XDG_STATE_HOME`.
    let probe = "set -euo pipefail
state_home=\"${XDG_STATE_HOME:-$HOME/.local/state}\"
basename=\"$(basename \"$(git worktree list --porcelain | awk '/^worktree / {print $2; exit}')\")\"
printf '%s\\n' \"$state_home/loom/prek/$basename/push-verified\"
";

    let main_out = Command::new("bash")
        .arg("-c")
        .arg(probe)
        .current_dir(workspace.path())
        .env("XDG_STATE_HOME", state_home.path())
        .env("HOME", state_home.path())
        .output()?;
    let main_stamp = String::from_utf8_lossy(&main_out.stdout).trim().to_string();

    let wt_out = Command::new("bash")
        .arg("-c")
        .arg(probe)
        .current_dir(&wt_path)
        .env("XDG_STATE_HOME", state_home.path())
        .env("HOME", state_home.path())
        .output()?;
    let wt_stamp = String::from_utf8_lossy(&wt_out.stdout).trim().to_string();

    if main_stamp != wt_stamp {
        bail!("stamp path diverges across worktrees: main={main_stamp} wt={wt_stamp}");
    }

    // End-to-end: a stamp written via the main path must be observable via
    // the worktree-derived path.
    fs::create_dir_all(Path::new(&main_stamp).parent().expect("stamp parent"))?;
    fs::write(&main_stamp, "deadbeef\n")?;
    let observed = fs::read_to_string(&wt_stamp)?;
    if observed.trim() != "deadbeef" {
        bail!("worktree did not observe main-written stamp: {observed:?}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Worker-context tests (SC 232, 237, 240).
// ---------------------------------------------------------------------------

struct WorkerSetup {
    workspace: TempDir,
    state_home: TempDir,
    stub_bin: TempDir,
    wt_path: PathBuf,
    prek_marker: PathBuf,
}

/// Builds a temp repo with `core.hooksPath` aimed at the real
/// `lib/prek/hooks/`, a stub `prek` on `PATH` that appends each invocation
/// to a marker file, and a linked worker worktree under
/// `.wrapix/worktree/<label>/<bead-id>/`. Returned tempdirs hold ownership
/// of the on-disk state for the test's duration.
fn setup_worker_repo(prek_body: &str) -> Result<WorkerSetup> {
    let workspace = TempDir::new()?;
    let state_home = TempDir::new()?;
    let stub_bin = TempDir::new()?;
    init_repo(workspace.path())?;

    git_in(
        workspace.path(),
        &[
            "config",
            "--local",
            "core.hooksPath",
            &hooks_dir().display().to_string(),
        ],
    )?;

    let prek_marker = stub_bin.path().join("prek-invocations");
    let prek_stub = stub_bin.path().join("prek");
    let body = prek_body.replace("{MARKER}", &prek_marker.display().to_string());
    make_executable(&prek_stub, &body)?;

    let wt_path = workspace.path().join(".wrapix/worktree/feat/lm-test.0");
    fs::create_dir_all(wt_path.parent().expect("worktree parent"))?;
    git_in(
        workspace.path(),
        &[
            "worktree",
            "add",
            "-q",
            &wt_path.display().to_string(),
            "-b",
            "loom/feat/lm-test.0",
        ],
    )?;

    Ok(WorkerSetup {
        workspace,
        state_home,
        stub_bin,
        wt_path,
        prek_marker,
    })
}

const PREK_RECORD_AND_EXIT_OK: &str = r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "{MARKER}"
exit 0
"#;

#[test]
fn worker_worktree_commit_fires_pre_commit_hook() -> Result<()> {
    let setup = setup_worker_repo(PREK_RECORD_AND_EXIT_OK)?;

    fs::write(setup.wt_path.join("change.txt"), "hello\n")?;
    let new_path = path_with(setup.stub_bin.path());

    let st = Command::new("git")
        .args([
            "-C",
            &setup.wt_path.display().to_string(),
            "add",
            "change.txt",
        ])
        .env("PATH", &new_path)
        .env("XDG_STATE_HOME", setup.state_home.path())
        .env("HOME", setup.state_home.path())
        .status()?;
    if !st.success() {
        bail!("git add failed: {st}");
    }

    let out = Command::new("git")
        .args([
            "-C",
            &setup.wt_path.display().to_string(),
            "commit",
            "-q",
            "-m",
            "worktree commit",
        ])
        .env("PATH", &new_path)
        .env("XDG_STATE_HOME", setup.state_home.path())
        .env("HOME", setup.state_home.path())
        .output()?;
    if !out.status.success() {
        bail!(
            "git commit from worktree failed: status={} stdout={:?} stderr={:?}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }

    if !setup.prek_marker.is_file() {
        bail!("pre-commit shim did not invoke the stub prek from the linked worktree");
    }
    let recorded = fs::read_to_string(&setup.prek_marker)?;
    if !recorded.contains("--hook-type=pre-commit") {
        bail!("stub prek did not receive --hook-type=pre-commit: {recorded:?}");
    }
    if !recorded.contains("--no-progress") {
        bail!("stub prek did not receive --no-progress: {recorded:?}");
    }
    Ok(())
}

#[test]
fn worker_worktree_push_fires_pre_push_hook() -> Result<()> {
    let prek_body = r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "{MARKER}"
# Real prek reads .pre-commit-config.yaml and runs `nix flake check` at
# pre-push. Mirror that so the stub `nix` on PATH records its invocation
# and the test can assert the chain end-to-end.
hook_type=""
for arg in "$@"; do
    case "$arg" in
        --hook-type=*) hook_type="${arg#--hook-type=}" ;;
    esac
done
if [[ "$hook_type" == "pre-push" ]]; then
    nix flake check
fi
exit 0
"#;
    let setup = setup_worker_repo(prek_body)?;

    let nix_marker = setup.stub_bin.path().join("nix-invocations");
    let nix_stub = setup.stub_bin.path().join("nix");
    make_executable(
        &nix_stub,
        &format!(
            "#!/usr/bin/env bash\nset -euo pipefail\nprintf '%s\\n' \"$*\" >> \"{}\"\nexit 0\n",
            nix_marker.display(),
        ),
    )?;

    // Bare remote so `git push` has a destination.
    let remote = TempDir::new()?;
    let st = Command::new("git")
        .args(["init", "--bare", "-q", "-b", "main"])
        .arg(remote.path())
        .status()?;
    if !st.success() {
        bail!("bare remote init failed: {st}");
    }
    git_in(
        setup.workspace.path(),
        &[
            "remote",
            "add",
            "origin",
            &remote.path().display().to_string(),
        ],
    )?;

    // Stage and commit a change in the worktree, then push from the worktree.
    fs::write(setup.wt_path.join("change.txt"), "hello\n")?;
    let new_path = path_with(setup.stub_bin.path());
    let st = Command::new("git")
        .args([
            "-C",
            &setup.wt_path.display().to_string(),
            "add",
            "change.txt",
        ])
        .env("PATH", &new_path)
        .env("XDG_STATE_HOME", setup.state_home.path())
        .env("HOME", setup.state_home.path())
        .status()?;
    if !st.success() {
        bail!("git add failed: {st}");
    }
    let st = Command::new("git")
        .args([
            "-C",
            &setup.wt_path.display().to_string(),
            "commit",
            "-q",
            "-m",
            "worktree commit",
        ])
        .env("PATH", &new_path)
        .env("XDG_STATE_HOME", setup.state_home.path())
        .env("HOME", setup.state_home.path())
        .status()?;
    if !st.success() {
        bail!("git commit failed: {st}");
    }

    let out = Command::new("git")
        .args([
            "-C",
            &setup.wt_path.display().to_string(),
            "push",
            "-q",
            "origin",
            "HEAD:refs/heads/from-worktree",
        ])
        .env("PATH", &new_path)
        .env("XDG_STATE_HOME", setup.state_home.path())
        .env("HOME", setup.state_home.path())
        .output()?;
    if !out.status.success() {
        bail!(
            "git push from worktree failed: status={} stderr={:?}",
            out.status,
            String::from_utf8_lossy(&out.stderr),
        );
    }

    if !setup.prek_marker.is_file() {
        bail!("pre-push shim did not invoke the stub prek from the linked worktree");
    }
    let prek_log = fs::read_to_string(&setup.prek_marker)?;
    if !prek_log.contains("--hook-type=pre-push") {
        bail!("stub prek did not receive --hook-type=pre-push: {prek_log:?}");
    }
    if !prek_log.contains("--no-progress") {
        bail!("stub prek did not receive --no-progress: {prek_log:?}");
    }

    if !nix_marker.is_file() {
        bail!("pre-push shim did not run `nix flake check` (no stub-nix invocation)");
    }
    let nix_log = fs::read_to_string(&nix_marker)?;
    if !nix_log.contains("flake check") {
        bail!("stub nix was invoked but not with `flake check`: {nix_log:?}");
    }
    Ok(())
}

#[test]
fn pre_push_catches_workspace_style_violation() -> Result<()> {
    // The real chain is pre-push shim -> prek hook-impl -> nix flake check
    // -> cargo test --test style git_client_encapsulation. The contract this
    // test pins is the *propagation*: when the workspace-scope style lint
    // reports a violation, the shim exits non-zero so the push aborts. A
    // stub prek inspects tracked .rs files for the same
    // `Command::new("git")`-outside-allowed-prefix pattern that style.rs
    // enforces, standing in for the heavy real toolchain invocation.
    let prek_body = r##"#!/usr/bin/env bash
set -euo pipefail
hook_type=""
for arg in "$@"; do
    case "$arg" in
        --hook-type=*) hook_type="${arg#--hook-type=}" ;;
    esac
done
if [[ "${1:-}" != "hook-impl" || "$hook_type" != "pre-push" ]]; then
    exit 0
fi
violations=0
while IFS= read -r f; do
    case "$f" in
        crates/loom-driver/src/git/*) continue ;;
    esac
    if grep -F 'Command::new("git")' "$f" >/dev/null 2>&1; then
        echo "style: git_client_encapsulation violation in $f" >&2
        violations=$((violations + 1))
    fi
done < <(git ls-files '*.rs')
if (( violations > 0 )); then
    exit 1
fi
exit 0
"##;
    let setup = setup_worker_repo(prek_body)?;

    let remote = TempDir::new()?;
    let st = Command::new("git")
        .args(["init", "--bare", "-q", "-b", "main"])
        .arg(remote.path())
        .status()?;
    if !st.success() {
        bail!("bare remote init failed: {st}");
    }
    git_in(
        setup.workspace.path(),
        &[
            "remote",
            "add",
            "origin",
            &remote.path().display().to_string(),
        ],
    )?;

    let new_path = path_with(setup.stub_bin.path());

    // Baseline: clean tree pushes successfully. Setup commits must route
    // through the stub-prek PATH so the pre-commit shim (which fires on
    // any commit once `core.hooksPath` is set) finds our stub instead of
    // the real prek that would abort on the missing `.pre-commit-config.yaml`.
    fs::create_dir_all(setup.workspace.path().join("crates/loom-driver/src/git"))?;
    fs::write(
        setup
            .workspace
            .path()
            .join("crates/loom-driver/src/git/ok.rs"),
        "// allowed prefix may use Command::new(\"git\") freely\n",
    )?;
    git_with_path(
        setup.workspace.path(),
        &new_path,
        setup.state_home.path(),
        &["add", "crates/loom-driver/src/git/ok.rs"],
    )?;
    git_with_path(
        setup.workspace.path(),
        &new_path,
        setup.state_home.path(),
        &["commit", "-q", "-m", "ok"],
    )?;

    let st = Command::new("git")
        .args([
            "-C",
            &setup.workspace.path().display().to_string(),
            "push",
            "-q",
            "origin",
            "HEAD:refs/heads/baseline",
        ])
        .env("PATH", &new_path)
        .env("XDG_STATE_HOME", setup.state_home.path())
        .env("HOME", setup.state_home.path())
        .status()?;
    if !st.success() {
        bail!("baseline push (no violation) failed: {st}");
    }

    // Introduce a git_client_encapsulation violation outside the allowed prefix.
    fs::create_dir_all(setup.workspace.path().join("crates/loom/src"))?;
    fs::write(
        setup.workspace.path().join("crates/loom/src/violation.rs"),
        "use std::process::Command;\nfn bad() { let _ = Command::new(\"git\"); }\n",
    )?;
    git_with_path(
        setup.workspace.path(),
        &new_path,
        setup.state_home.path(),
        &["add", "crates/loom/src/violation.rs"],
    )?;
    git_with_path(
        setup.workspace.path(),
        &new_path,
        setup.state_home.path(),
        &["commit", "-q", "-m", "introduce violation"],
    )?;

    let out = Command::new("git")
        .args([
            "-C",
            &setup.workspace.path().display().to_string(),
            "push",
            "-q",
            "origin",
            "HEAD:refs/heads/with-violation",
        ])
        .env("PATH", &new_path)
        .env("XDG_STATE_HOME", setup.state_home.path())
        .env("HOME", setup.state_home.path())
        .output()?;
    if out.status.success() {
        bail!("push with style violation unexpectedly succeeded");
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stderr.contains("git_client_encapsulation") {
        bail!("expected git_client_encapsulation violation in stderr; got {stderr:?}",);
    }
    Ok(())
}

//! End-to-end smoke test for `loom run --once`.
//!
//! Pins that `loom run --once` against a fake bd returns a meaningful
//! exit code (not unrecognized subcommand).
//!
//! The test installs a stub `bd` on PATH that prints `[]` for every `ready` /
//! `list` query (emulating an empty molecule), seeds a state DB so spec
//! resolution succeeds, and invokes the compiled `loom` binary. The expected
//! path through `run_loop`:
//!
//! 1. `next_ready_bead` → `bd ready --label spec:<X>` → empty slice → `None`,
//! 2. `RunMode::Once` exits cleanly without invoking `loom review`,
//! 3. binary returns exit code 0.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

/// Initialize a real git repo at `path` with one initial commit so
/// `loom run`'s per-bead worktree dispatch succeeds. The smoke tests
/// invoke `loom run` against a workspace tempdir; without this, the
/// binary's [`GitClient::open`] call fails before reaching the ready-queue
/// check this test is asserting against.
fn init_workspace_repo(path: &Path) {
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .status()
            .expect("spawn git");
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["init", "-q", "-b", "main"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test"]);
    run(&["config", "commit.gpgsign", "false"]);
    std::fs::write(path.join("README.md"), "initial\n").expect("write README");
    run(&["add", "README.md"]);
    run(&["commit", "-q", "-m", "initial"]);
}

/// Write a stub `bd` shell script to `dir/bin/bd` that returns `[]` for any
/// JSON-shaped subcommand and `0` for everything else. Returns the bin
/// directory caller should prepend to PATH.
fn install_bd_stub(dir: &Path) -> std::path::PathBuf {
    let bin_dir = dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let bd = bin_dir.join("bd");
    std::fs::write(
        &bd,
        "#!/bin/sh\n\
         # The driver's `ready` / `list` calls all carry --json; the rest of\n\
         # the bd surface (close, update) gets a silent zero.\n\
         for arg in \"$@\"; do\n\
           if [ \"$arg\" = \"--json\" ]; then\n\
             printf '%s' '[]'\n\
             exit 0\n\
           fi\n\
         done\n\
         exit 0\n",
    )
    .unwrap();
    let mut perm = std::fs::metadata(&bd).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&bd, perm).unwrap();
    bin_dir
}

/// Write a stub `bd` that appends each invocation's full argv to
/// `argv_log` (NUL-separated argv per line) and returns `[]` for any
/// JSON-shaped subcommand. Used to inspect the exact flags `loom`'s bd
/// client emits.
fn install_bd_argv_logger(dir: &Path, argv_log: &Path) -> std::path::PathBuf {
    let bin_dir = dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let bd = bin_dir.join("bd");
    let script = format!(
        "#!/bin/sh\n\
         {{ for a in \"$@\"; do printf '%s\\t' \"$a\"; done; printf '\\n'; }} >> {log}\n\
         for arg in \"$@\"; do\n\
           if [ \"$arg\" = \"--json\" ]; then\n\
             printf '%s' '[]'\n\
             exit 0\n\
           fi\n\
         done\n\
         exit 0\n",
        log = argv_log.display(),
    );
    std::fs::write(&bd, script).unwrap();
    let mut perm = std::fs::metadata(&bd).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&bd, perm).unwrap();
    bin_dir
}

#[test]
fn loom_run_once_against_empty_bd_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    init_workspace_repo(workspace);
    std::fs::create_dir_all(workspace.join(".wrapix/loom")).unwrap();
    std::fs::create_dir_all(workspace.join("specs")).unwrap();

    // Seed state DB + active spec so resolve_spec_label returns Some(label)
    // without the caller having to pass -s.
    let db = loom_driver::state::StateDb::open(workspace.join(".wrapix/loom/state.db")).unwrap();
    db.set_current_spec(&loom_driver::identifier::SpecLabel::new("harness"))
        .unwrap();
    drop(db);

    let bin_dir = install_bd_stub(workspace);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = vec![bin_dir];
    path_entries.extend(std::env::split_paths(&path));
    let new_path = std::env::join_paths(path_entries).unwrap();

    // The CLI requires LOOM_PROFILES_MANIFEST for spawn-bound subcommands.
    // Even on the empty-queue fast-path the manifest is read before spec
    // resolution, so the smoke test must point at a real file.
    let manifest_path = workspace.join("profile-images.json");
    std::fs::write(&manifest_path, "{}").unwrap();

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .arg("run")
        .arg("--once")
        .env("PATH", new_path)
        .env("LOOM_PROFILES_MANIFEST", &manifest_path)
        // The exec_review path is gated behind RunMode::Continuous; on the
        // empty-queue path we still set this so the binary can locate itself
        // if the loop ever changes shape.
        .env("LOOM_BIN", loom_bin)
        .env("XDG_STATE_HOME", workspace.join(".loom-test-state"))
        // Bypass the nested-loom guard so cargo test inside a loom container
        // still reaches the run dispatch path under test.
        .env_remove("LOOM_INSIDE")
        .output()
        .expect("spawn loom");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom run --once must exit zero on empty queue. stdout={stdout} stderr={stderr}",
    );
    assert!(
        stdout.contains("loom run:"),
        "expected the run summary line. stdout={stdout}",
    );
    assert!(
        stdout.contains("molecule_complete=true"),
        "empty queue must mark the molecule complete. stdout={stdout}",
    );
    assert!(
        stdout.contains("execed_review=false"),
        "--once must NOT exec review. stdout={stdout}",
    );
}

/// FR1: the `--parallel N` path of `loom run` must call
/// `bd ready` with `--exclude-label=loom:clarify --exclude-label=loom:blocked`,
/// matching the sequential path's behavior. Without these flags, beads
/// parked for human resolution would be re-dispatched on every loop. The
/// `BdClient` unit test pins the wire encoding once labels are passed in;
/// this test pins the call site in `run_parallel_run` itself.
#[test]
fn loom_run_parallel_passes_exclude_label_to_bd_ready() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    init_workspace_repo(workspace);
    std::fs::create_dir_all(workspace.join(".wrapix/loom")).unwrap();
    std::fs::create_dir_all(workspace.join("specs")).unwrap();

    let db = loom_driver::state::StateDb::open(workspace.join(".wrapix/loom/state.db")).unwrap();
    db.set_current_spec(&loom_driver::identifier::SpecLabel::new("harness"))
        .unwrap();
    drop(db);

    let argv_log = workspace.join("bd-argv.log");
    let bin_dir = install_bd_argv_logger(workspace, &argv_log);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = vec![bin_dir];
    path_entries.extend(std::env::split_paths(&path));
    let new_path = std::env::join_paths(path_entries).unwrap();

    let manifest_path = workspace.join("profile-images.json");
    std::fs::write(&manifest_path, "{}").unwrap();

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .arg("run")
        .arg("--parallel")
        .arg("2")
        .env("PATH", new_path)
        .env("LOOM_PROFILES_MANIFEST", &manifest_path)
        .env("LOOM_BIN", loom_bin)
        .env("XDG_STATE_HOME", workspace.join(".loom-test-state"))
        .env_remove("LOOM_INSIDE")
        .output()
        .expect("spawn loom");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom run --parallel 2 must exit zero against an empty bd queue. \
         stdout={stdout} stderr={stderr}",
    );

    let log = std::fs::read_to_string(&argv_log)
        .unwrap_or_else(|_| panic!("bd-argv log {} must exist", argv_log.display()));
    let ready_line = log
        .lines()
        .find(|line| {
            let mut fields = line.split('\t');
            fields.next() == Some("ready")
        })
        .unwrap_or_else(|| panic!("no `bd ready` call recorded in log:\n{log}"));
    let argv: Vec<&str> = ready_line.split('\t').collect();
    assert!(
        argv.contains(&"--exclude-label=loom:clarify"),
        "parallel `bd ready` must exclude loom:clarify; argv={argv:?}",
    );
    assert!(
        argv.contains(&"--exclude-label=loom:blocked"),
        "parallel `bd ready` must exclude loom:blocked; argv={argv:?}",
    );
}

#[test]
fn loom_run_recognizes_subcommand() {
    // Regression guard: `loom run --help` must exit cleanly. A binary
    // that does not expose `run` prints "unrecognized subcommand: run".
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .arg("run")
        .arg("--help")
        .output()
        .expect("spawn loom");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom run --help must exit zero. stdout={stdout} stderr={stderr}",
    );
    assert!(
        stdout.contains("--once") && stdout.contains("--parallel"),
        "loom run --help must list the spec'd flags. stdout={stdout}",
    );
}

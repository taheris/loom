//! End-to-end verdict-gate tests covering `LOOM_BLOCKED`, `LOOM_CLARIFY`,
//! and `LOOM_COMPLETE` marker handling, plus the invariant that the
//! driver itself never invokes `bd close` (closure is the agent's
//! responsibility per `specs/harness.md` § Verdict gate).
//!
//! Drives `loom loop <bead-id>` against a Rust mock agent that emits the
//! marker through the pi-mono protocol, with `bd-shim` standing in for
//! the live beads socket. A prior bug collapsed every clean-exit
//! session to `AgentOutcome::Success → bd close`, ignoring markers; the
//! unit tests on `phase_verdict::decide` passed throughout because they
//! never exercised `loom loop`'s actual marker-routing wiring.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Initialize a real git repo at `path` plus the loom-owned integration
/// workspace at `.loom/integration/` so `loom loop`'s per-bead
/// worktree dispatch (via `GitClient::create_worktree`) and the
/// post-merge push gate both succeed.
fn init_workspace_repo(path: &Path) {
    loom_driver::git::init_test_repo_with_integration(path)
        .expect("init test repo with loom integration");
}

fn seed_bead(state_dir: &Path, id: &str, title: &str, description: &str, labels: &[&str]) {
    let bead_dir = state_dir.join(id);
    std::fs::create_dir_all(&bead_dir).expect("mkdir bead dir");
    std::fs::write(bead_dir.join("title"), title).expect("write title");
    std::fs::write(bead_dir.join("description"), description).expect("write description");
    std::fs::write(bead_dir.join("status"), "open").expect("write status");
    std::fs::write(bead_dir.join("priority"), "2").expect("write priority");
    std::fs::write(bead_dir.join("issue_type"), "task").expect("write issue_type");
    let body = labels.join("\n");
    std::fs::write(bead_dir.join("labels"), body).expect("write labels");
}

fn install_bd_shim(dir: &Path) -> PathBuf {
    let bin_dir = dir.join("bd-bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir bd-bin");
    let bd_path = bin_dir.join("bd");
    let source = PathBuf::from(env!("CARGO_BIN_EXE_bd-shim"));
    match std::os::unix::fs::symlink(&source, &bd_path) {
        Ok(_) => {}
        Err(_) => {
            std::fs::copy(&source, &bd_path).expect("copy bd-shim");
            let mut perm = std::fs::metadata(&bd_path).expect("stat bd").permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&bd_path, perm).expect("chmod bd");
        }
    }
    bin_dir
}

/// Write a profile manifest pointing at an empty tar; `loom loop`
/// resolves it via `LOOM_PROFILES_MANIFEST` even on the empty-queue
/// fast path. The image is never instantiated — the mock agent
/// replaces wrix end-to-end — so the source tar can be empty.
fn write_minimal_manifest(dir: &Path) -> PathBuf {
    let source = dir.join("base.tar");
    std::fs::write(&source, "").expect("write base.tar");
    let manifest = dir.join("profile-images.json");
    let body = format!(
        r#"{{"base": {{"pi": {{"ref":"localhost/wrix-base-pi:test","source":{source:?}, "source_kind": "nix-descriptor"}}, "claude": {{"ref":"localhost/wrix-base-claude:test","source":{source:?}, "source_kind": "nix-descriptor"}}, "direct": {{"ref":"localhost/wrix-base-direct:test","source":{source:?}, "source_kind": "nix-descriptor"}}}}}}"#,
        source = source.display().to_string(),
    );
    std::fs::write(&manifest, body).expect("write manifest");
    manifest
}

fn run_loom_loop_bead(
    workspace: &Path,
    bin_dir: &Path,
    state_dir: &Path,
    manifest: &Path,
    agent_mode: &str,
    bead_id: &str,
) -> std::process::Output {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let mut entries: Vec<PathBuf> = vec![bin_dir.to_path_buf()];
    entries.extend(std::env::split_paths(&path_var));
    let new_path = std::env::join_paths(entries).expect("join PATH");

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let mock_agent = env!("CARGO_BIN_EXE_mock-loom-agent");

    Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .arg("--agent")
        .arg("pi")
        .arg("loop")
        .arg("--host-key")
        .arg(bead_id)
        .env("PATH", new_path)
        .env("LOOM_WRIX_BIN", mock_agent)
        .env_remove("LOOM_WRIX_SPAWN_BIN")
        .env("LOOM_TEST_AGENT_MODE", agent_mode)
        .env("LOOM_BIN", loom_bin)
        .env("LOOM_PROFILES_MANIFEST", manifest)
        .env("BD_STATE_DIR", state_dir)
        .env("XDG_STATE_HOME", workspace.join(".loom-test-state"))
        // The nested-loom guard refuses `loom loop` when LOOM_INSIDE=1.
        // The cargo test runner inherits LOOM_INSIDE when this suite is
        // executed inside a loom-managed container, which would block
        // the child `loom loop` invocation before it reached the marker
        // routing under test. Strip it so the test exercises the live
        // dispatch path the spec criterion pins.
        .env_remove("LOOM_INSIDE")
        .output()
        .expect("spawn loom")
}

fn read_invocation_log(state_dir: &Path) -> String {
    std::fs::read_to_string(state_dir.join(".invocations.log")).unwrap_or_default()
}

fn read_field(state_dir: &Path, id: &str, field: &str) -> String {
    std::fs::read_to_string(state_dir.join(id).join(field)).unwrap_or_default()
}

fn read_labels(state_dir: &Path, id: &str) -> Vec<String> {
    read_field(state_dir, id, "labels")
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// A `bd close <id>` invocation from the driver looks like
/// `close <id>` in the shim's quoted argv log. Returns true iff any
/// such line targets `target_id`.
fn driver_closed_bead(log: &str, target_id: &str) -> bool {
    log.lines().any(|line| {
        let mut tokens = line.split_whitespace();
        tokens.next() == Some("close") && tokens.next() == Some(target_id)
    })
}

// -------------------------------------------------------------------
// B5 — `test_gate_loom_blocked_marker`
// -------------------------------------------------------------------

/// Agent emits `LOOM_BLOCKED` with a reason. Verdict gate must:
/// - transition the bead to `status=blocked`,
/// - add the `loom:blocked` label (via `bd update --add-label`),
/// - NOT invoke `bd close` on that bead from the driver process.
///
/// The status transition is the dedup mechanism: `bd ready` natively
/// excludes status=blocked so the run loop won't re-dispatch the bead.
#[test]
fn loom_loop_bead_routes_blocked_marker_to_label_and_status_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    init_workspace_repo(workspace);
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();

    seed_bead(
        &state_dir,
        "lm-blocka",
        "spec missing schema",
        "Need to land the schema section before this bead can proceed.\n",
        &["spec:markertest", "profile:base"],
    );

    let bin_dir = install_bd_shim(workspace);
    let manifest = write_minimal_manifest(workspace);

    let output = run_loom_loop_bead(
        workspace,
        &bin_dir,
        &state_dir,
        &manifest,
        "blocked-marker",
        "lm-blocka",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let log = read_invocation_log(&state_dir);
    assert!(
        output.status.success(),
        "loom loop <bead-id> must exit 0 on LOOM_BLOCKED.\n\
         stdout={stdout}\nstderr={stderr}\nbd-shim log:\n{log}",
    );

    let status = read_field(&state_dir, "lm-blocka", "status");
    assert_eq!(
        status.trim(),
        "blocked",
        "blocked bead must transition to status=blocked so `bd ready` excludes it \
         on the next loop iteration. status={status:?}\nbd-shim log:\n{log}",
    );

    let labels = read_labels(&state_dir, "lm-blocka");
    assert!(
        labels.iter().any(|l| l == "loom:blocked"),
        "blocked bead must carry loom:blocked. labels={labels:?}\nbd-shim log:\n{log}",
    );

    assert!(
        !driver_closed_bead(&log, "lm-blocka"),
        "driver must NOT call `bd close lm-blocka` on LOOM_BLOCKED.\nbd-shim log:\n{log}",
    );
}

/// Agent emits `LOOM_CLARIFY` with a question. Same shape as
/// blocked-marker: `status=blocked`, `loom:clarify` label, no driver-side
/// close. The status transition is the dedup mechanism per the paired
/// label+status contract.
#[test]
fn loom_loop_bead_routes_clarify_marker_to_label_and_status_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    init_workspace_repo(workspace);
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();

    seed_bead(
        &state_dir,
        "lm-clara",
        "deploy key path?",
        "Need to know which deploy-key path to mount before continuing.\n\n\
         ## Options — pick a deploy-key path\n\n\
         ### Option 1 — mount /var/keys\n\
         body.\n\n\
         ### Option 2 — mount /etc/keys\n\
         body.\n",
        &["spec:markertest", "profile:base"],
    );

    let bin_dir = install_bd_shim(workspace);
    let manifest = write_minimal_manifest(workspace);

    let output = run_loom_loop_bead(
        workspace,
        &bin_dir,
        &state_dir,
        &manifest,
        "clarify-marker",
        "lm-clara",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let log = read_invocation_log(&state_dir);
    assert!(
        output.status.success(),
        "loom loop <bead-id> must exit 0 on LOOM_CLARIFY.\n\
         stdout={stdout}\nstderr={stderr}\nbd-shim log:\n{log}",
    );

    let status = read_field(&state_dir, "lm-clara", "status");
    assert_eq!(
        status.trim(),
        "blocked",
        "clarify bead must transition to status=blocked so `bd ready` excludes it \
         on the next loop iteration. status={status:?}\nbd-shim log:\n{log}",
    );

    let labels = read_labels(&state_dir, "lm-clara");
    assert!(
        labels.iter().any(|l| l == "loom:clarify"),
        "clarify bead must carry loom:clarify. labels={labels:?}\nbd-shim log:\n{log}",
    );

    assert!(
        !driver_closed_bead(&log, "lm-clara"),
        "driver must NOT call `bd close lm-clara` on LOOM_CLARIFY.\nbd-shim log:\n{log}",
    );
}

#[test]
fn direct_emit_clarify_without_options_block_falls_back_to_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    init_workspace_repo(workspace);
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();
    seed_bead(
        &state_dir,
        "lm-noopts",
        "clarify without persisted options",
        "The agent forgot to persist the canonical options block.",
        &["spec:agent", "profile:base"],
    );
    let bin_dir = install_bd_shim(workspace);
    let manifest = write_minimal_manifest(workspace);

    let output = run_loom_loop_bead(
        workspace,
        &bin_dir,
        &state_dir,
        &manifest,
        "clarify-marker",
        "lm-noopts",
    );
    let log = read_invocation_log(&state_dir);
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}\nbd-shim log:\n{log}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(read_field(&state_dir, "lm-noopts", "status"), "blocked");
    let labels = read_labels(&state_dir, "lm-noopts");
    assert!(
        labels.iter().any(|label| label == "loom:blocked"),
        "{labels:?}\n{log}"
    );
    assert!(
        !labels.iter().any(|label| label == "loom:clarify"),
        "{labels:?}\n{log}"
    );
    assert!(
        read_field(&state_dir, "lm-noopts", "notes").contains("clarify-without-options"),
        "{log}"
    );
    assert!(!driver_closed_bead(&log, "lm-noopts"), "{log}");
}

// -------------------------------------------------------------------
// B6 — `test_run_does_not_close_bead`
// -------------------------------------------------------------------

/// Sweeps all three marker scenarios in one test and asserts a single
/// invariant across them: the driver never calls `bd close <id>` on
/// the dispatched bead — closure is the agent's responsibility per
/// the verdict-gate decision table.
///
/// For LOOM_COMPLETE specifically: the mock agent does NOT call
/// `bd close` itself (that's outside its mocked surface area). The
/// bead is expected to remain open after the run. The point of the
/// test is the driver's restraint, not the bd-closed observable.
#[test]
fn loom_loop_never_invokes_bd_close_on_dispatched_bead_across_all_markers() {
    for (mode, id) in [
        ("blocked-marker", "lm-noclos"),
        ("clarify-marker", "lm-noclos2"),
        ("complete-marker", "lm-noclos3"),
        ("no-marker", "lm-noclos4"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path();
        init_workspace_repo(workspace);
        let state_dir = workspace.join("bd-state");
        std::fs::create_dir_all(&state_dir).unwrap();

        seed_bead(
            &state_dir,
            id,
            "no-driver-close gate",
            "Driver must not call bd close on this bead.\n",
            &["spec:noclostest", "profile:base"],
        );

        let bin_dir = install_bd_shim(workspace);
        let manifest = write_minimal_manifest(workspace);

        let output = run_loom_loop_bead(workspace, &bin_dir, &state_dir, &manifest, mode, id);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let log = read_invocation_log(&state_dir);

        assert!(
            !driver_closed_bead(&log, id),
            "[{mode}] driver must NOT invoke `bd close {id}` — closure is the \
             agent's job per the verdict-gate decision table.\n\
             stdout={stdout}\nstderr={stderr}\nbd-shim log:\n{log}",
        );
    }
}

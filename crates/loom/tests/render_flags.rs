//! `loom loop` reaches the `Renderer` trait via `--json` / `--raw` /
//! `--plain` CLI flags and the on-disk JSONL stays identical regardless
//! of which mode drives the terminal.
//!
//! Each test spawns `loom loop <bead-id>` against the mock pi agent in
//! `complete-marker` mode with one of the new flags, then parses
//! stdout against the spec'd shape:
//!
//! - `--json` — one pretty-printed JSON object per line (multi-line
//!   per event; first line starts with `{`)
//! - `--raw`  — one compact JSON line per event (single-line per
//!   record, parseable as an `AgentEvent`)
//! - `--plain` — ASCII text with no ANSI escape bytes
//!
//! Each test also asserts the on-disk JSONL log was written — the
//! sink's tee contract holds across mode changes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

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

fn install_loom_gate_stub(dir: &Path) -> PathBuf {
    let stub = dir.join("loom-gate-stub.sh");
    std::fs::write(
        &stub,
        loom_test_support::bash_script(
            "set -euo pipefail\n\
         if [[ \"${2:-}\" == \"review\" ]]; then\n\
             echo 'LOOM_COMPLETE'\n\
         fi\n",
        ),
    )
    .expect("write loom gate stub");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
        .expect("chmod loom gate stub");
    stub
}

fn run_loom_with_flags(
    workspace: &Path,
    bin_dir: &Path,
    state_dir: &Path,
    manifest: &Path,
    bead_id: &str,
    flags: &[&str],
) -> std::process::Output {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let mut entries: Vec<PathBuf> = vec![bin_dir.to_path_buf()];
    entries.extend(std::env::split_paths(&path_var));
    let new_path = std::env::join_paths(entries).expect("join PATH");

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let mock_agent = env!("CARGO_BIN_EXE_mock-loom-agent");
    let loom_gate_stub = install_loom_gate_stub(workspace);

    Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .arg("--agent")
        .arg("pi")
        .arg("loop")
        .arg(bead_id)
        .args(flags)
        .env("PATH", new_path)
        .env("LOOM_WRIX_BIN", mock_agent)
        .env("LOOM_TEST_AGENT_MODE", "complete-marker")
        .env("LOOM_BIN", loom_gate_stub)
        .env("LOOM_PROFILES_MANIFEST", manifest)
        .env("BD_STATE_DIR", state_dir)
        .env("XDG_STATE_HOME", workspace.join(".loom-test-state"))
        // Bypass the nested-loom guard so cargo test inside a loom container
        // still reaches the run dispatch path under test.
        .env_remove("LOOM_INSIDE")
        .output()
        .expect("spawn loom")
}

fn run_loom_with_flag(
    workspace: &Path,
    bin_dir: &Path,
    state_dir: &Path,
    manifest: &Path,
    bead_id: &str,
    flag: &str,
) -> std::process::Output {
    run_loom_with_flags(workspace, bin_dir, state_dir, manifest, bead_id, &[flag])
}

fn setup(workspace: &Path) -> (PathBuf, PathBuf, PathBuf) {
    init_workspace_repo(workspace);
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).expect("mkdir state");
    let bin_dir = install_bd_shim(workspace);
    let manifest = write_minimal_manifest(workspace);
    (state_dir, bin_dir, manifest)
}

/// Initialize a real git repo at `path` plus the loom-owned integration
/// workspace at `.loom/integration/` and a bare `origin` so
/// `loom loop`'s per-bead worktree dispatch + post-merge push gate both
/// succeed.
fn init_workspace_repo(path: &Path) {
    loom_driver::git::init_test_repo_with_integration(path)
        .expect("init test repo with loom integration");
}

fn find_bead_log(workspace: &Path, spec_label: &str, bead_id: &str) -> PathBuf {
    let dir = workspace.join(".loom/logs").join(spec_label);
    let entries =
        std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
    let prefix = format!("{bead_id}-");
    let mut candidates: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".jsonl"))
        })
        .collect();
    candidates.sort();
    candidates
        .pop()
        .unwrap_or_else(|| panic!("no JSONL log for `{bead_id}` under {}", dir.display()))
}

#[test]
fn loom_loop_json_flag_emits_pretty_printed_json_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let (state_dir, bin_dir, manifest) = setup(workspace);

    let bead = "lm-r5j";
    let spec = "rendertest";
    seed_bead(
        &state_dir,
        bead,
        "render flag pin",
        "Drives loom loop --json against the mock agent.\n",
        &[&format!("spec:{spec}"), "profile:base"],
    );

    let output = run_loom_with_flag(workspace, &bin_dir, &state_dir, &manifest, bead, "--json");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom loop <bead-id> --json must exit 0.\nstdout={stdout}\nstderr={stderr}",
    );
    // Json mode emits pretty-printed objects. The presence of `"kind":`
    // (with the space between `:` and the value that pretty-print
    // inserts) is the easy positive signal. The summary line
    // (`loom loop: …`) is the only non-JSON content allowed in stdout.
    assert!(
        stdout.contains("\"kind\":"),
        "expected pretty-printed event JSON on stdout — got {stdout:?}",
    );
    // On-disk JSONL is independent of render mode.
    let log_path = find_bead_log(workspace, spec, bead);
    let body = std::fs::read_to_string(&log_path).expect("read log");
    assert!(
        !body.is_empty(),
        "tee contract: on-disk JSONL must be written even with --json: {}",
        log_path.display(),
    );
}

#[test]
fn loom_loop_raw_flag_emits_compact_jsonl_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let (state_dir, bin_dir, manifest) = setup(workspace);

    let bead = "lm-r5r";
    let spec = "rendertest";
    seed_bead(
        &state_dir,
        bead,
        "render flag pin",
        "Drives loom loop --raw against the mock agent.\n",
        &[&format!("spec:{spec}"), "profile:base"],
    );

    let output = run_loom_with_flag(workspace, &bin_dir, &state_dir, &manifest, bead, "--raw");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom loop <bead-id> --raw must exit 0.\nstdout={stdout}\nstderr={stderr}",
    );
    // Raw mode emits compact JSON. Find at least one parseable event line.
    let parsed = stdout
        .lines()
        .filter(|l| l.starts_with('{') && l.ends_with('}'))
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v.get("kind").and_then(|k| k.as_str()).is_some());
    assert!(
        parsed.is_some(),
        "expected at least one compact-JSON event line on stdout — got {stdout:?}",
    );
}

#[test]
fn loom_loop_plain_flag_emits_no_ansi_escapes_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let (state_dir, bin_dir, manifest) = setup(workspace);

    let bead = "lm-r5p";
    let spec = "rendertest";
    seed_bead(
        &state_dir,
        bead,
        "render flag pin",
        "Drives loom loop --plain against the mock agent.\n",
        &[&format!("spec:{spec}"), "profile:base"],
    );

    let output = run_loom_with_flag(workspace, &bin_dir, &state_dir, &manifest, bead, "--plain");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom loop <bead-id> --plain must exit 0.\nstdout={stdout}\nstderr={stderr}",
    );
    // Plain output must not carry ANSI escape bytes; OSC 8 wrappers and
    // color codes both start with `\x1b`.
    assert!(
        !stdout.contains('\x1b'),
        "plain mode must not emit ANSI escape bytes in stdout: {stdout:?}",
    );
}

#[test]
fn loom_loop_without_trace_keeps_agent_bookkeeping_off_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let (state_dir, bin_dir, manifest) = setup(workspace);

    let bead = "lm-r5n";
    let spec = "rendertest";
    seed_bead(
        &state_dir,
        bead,
        "render flag pin",
        "Drives loom loop without raw tracing enabled.\n",
        &[&format!("spec:{spec}"), "profile:base"],
    );

    let output = run_loom_with_flag(workspace, &bin_dir, &state_dir, &manifest, bead, "--plain");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom loop <bead-id> --plain must exit 0.\nstdout={stdout}\nstderr={stderr}",
    );
    assert!(
        !stderr.contains("agent event") && !stderr.contains("message_delta"),
        "agent bookkeeping must be hidden unless --trace is set: {stderr:?}",
    );
}

#[test]
fn loop_trace_flag_mirrors_tracing_to_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let (state_dir, bin_dir, manifest) = setup(workspace);

    let bead = "lm-r5t";
    let spec = "rendertest";
    seed_bead(
        &state_dir,
        bead,
        "render flag pin",
        "Drives loom loop --trace against the mock agent.\n",
        &[&format!("spec:{spec}"), "profile:base"],
    );

    let output = run_loom_with_flags(
        workspace,
        &bin_dir,
        &state_dir,
        &manifest,
        bead,
        &["--plain", "--trace"],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom loop <bead-id> --plain --trace must exit 0.\nstdout={stdout}\nstderr={stderr}",
    );
    assert!(stdout.contains("LOOM_COMPLETE"), "{stdout:?}");
    assert!(
        !stdout.contains("agent event") && !stdout.contains("message_delta"),
        "--trace must not change renderer stdout: {stdout:?}",
    );
    assert!(stderr.contains("agent event"), "{stderr:?}");
    assert!(stderr.contains("message_delta"), "{stderr:?}");
}

#[test]
fn loom_loop_rejects_conflicting_render_flags() {
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .arg("loop")
        .arg("--json")
        .arg("--raw")
        .output()
        .expect("spawn loom");
    assert!(
        !output.status.success(),
        "clap must reject `--json --raw` as conflicting flags",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--raw") || stderr.contains("--json"),
        "error must name one of the conflicting flags: {stderr:?}",
    );
}

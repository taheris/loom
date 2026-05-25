//! Every `AgentEvent` emitted by `loom run` carries a per-spawn
//! envelope (real `bead_id`, monotonic `seq`, real `ts_ms`).
//!
//! Drives `loom run --once` against the mock pi agent in
//! `complete-marker` mode, locates the per-bead JSONL log, and asserts
//! that every recorded event carries the seeded bead id and that `seq`
//! advances by exactly one per event starting at zero. Guards against
//! regression to `EventEnvelope::placeholder()` (sentinel `wx-pending`,
//! `seq=0` everywhere).

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
        r#"{{"base": {{"ref":"localhost/wrapix-base:test","source":{source:?}}}}}"#,
        source = source.display().to_string(),
    );
    std::fs::write(&manifest, body).expect("write manifest");
    manifest
}

fn run_loom_run_once(
    workspace: &Path,
    bin_dir: &Path,
    state_dir: &Path,
    manifest: &Path,
    agent_mode: &str,
    spec_label: &str,
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
        .arg("run")
        .arg("--once")
        .arg("-s")
        .arg(spec_label)
        .env("PATH", new_path)
        .env("LOOM_WRAPIX_BIN", mock_agent)
        .env("LOOM_TEST_AGENT_MODE", agent_mode)
        .env("LOOM_BIN", loom_bin)
        .env("LOOM_PROFILES_MANIFEST", manifest)
        .env("BD_STATE_DIR", state_dir)
        .env("XDG_STATE_HOME", workspace.join(".loom-test-state"))
        // Bypass the nested-loom guard so cargo test inside a loom container
        // still reaches the run dispatch path under test.
        .env_remove("LOOM_INSIDE")
        .output()
        .expect("spawn loom")
}

fn find_bead_log(workspace: &Path, spec_label: &str, bead_id: &str) -> PathBuf {
    let dir = workspace.join(".wrapix/loom/logs").join(spec_label);
    let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| {
        panic!("read_dir {}: {e}", dir.display());
    });
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
    candidates.pop().unwrap_or_else(|| {
        panic!(
            "no JSONL log for bead `{bead_id}` under {} — directory listing did not include a \
             matching file",
            dir.display(),
        )
    })
}

#[test]
fn loom_run_stamps_real_bead_id_and_monotonic_seq_on_every_event() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let bead = "wx-envt";
    let spec = "envelopetest";
    seed_bead(
        &state_dir,
        bead,
        "envelope stamping",
        "Drive the mock agent and inspect the JSONL envelope.\n",
        &[&format!("spec:{spec}"), "profile:base"],
    );

    let bin_dir = install_bd_shim(workspace);
    let manifest = write_minimal_manifest(workspace);

    let output = run_loom_run_once(
        workspace,
        &bin_dir,
        &state_dir,
        &manifest,
        "complete-marker",
        spec,
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom run --once must exit 0 on complete-marker.\nstdout={stdout}\nstderr={stderr}",
    );

    let log_path = find_bead_log(workspace, spec, bead);
    let body = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", log_path.display()));
    let lines: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
    assert!(
        !lines.is_empty(),
        "log {} has no events — the sink wrote zero records",
        log_path.display(),
    );

    for (i, line) in lines.iter().enumerate() {
        let value: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("line {i} parse failed: {e}\nline={line}"));
        let obj = value
            .as_object()
            .unwrap_or_else(|| panic!("line {i} not an object: {line}"));
        let actual_bead = obj
            .get("bead_id")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("line {i} missing bead_id: {line}"));
        assert_eq!(
            actual_bead, bead,
            "line {i} carries bead_id={actual_bead:?}, expected {bead:?} (sentinel \
             `wx-pending` here means envelope wiring regressed)\nline={line}",
        );
        let seq = obj
            .get("seq")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_else(|| panic!("line {i} missing or non-u64 seq: {line}"));
        assert_eq!(
            seq, i as u64,
            "line {i} carries seq={seq}, expected {i} — events must be stamped with a \
             monotonic per-spawn counter starting at zero\nline={line}",
        );
        let ts = obj
            .get("ts_ms")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_else(|| panic!("line {i} missing or non-i64 ts_ms: {line}"));
        assert!(
            ts > 0,
            "line {i} carries ts_ms={ts}, expected a real wall-clock millisecond value \
             (sentinel `0` here means the clock closure did not run)\nline={line}",
        );
    }
}

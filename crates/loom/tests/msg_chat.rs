//! `loom msg --chat` integration tests.
//!
//! `loom msg --chat` mirrors `loom plan`'s shape: a single `wrapix run
//! <workspace> claude --dangerously-skip-permissions <prompt>` shell-out
//! with **inherited stdio** so claude attaches directly to the user's
//! terminal as a real REPL. There is no pi-mono protocol involved here
//! — the tests use a shell stub that records argv and (per the test
//! mode) forks `bd update` calls or exits non-zero.
//!
//! Five distinct slices, one per `test_msg_chat_*` dispatcher:
//!
//! - `launches_container`     — argv shape: `run <workspace> claude
//!   --dangerously-skip-permissions <prompt>` plus the
//!   `WRAPIX_DEFAULT_IMAGE_REF` / `_SOURCE` env vars the launcher reads.
//! - `writes_notes`           — stub parses the prompt for `### <id>`
//!   headers and forks `bd update <id> --notes "…" --remove-label
//!   loom:clarify` per bead; bd-shim log + bead state reflect it.
//! - `partial_progress`       — stub exits 0 without resolving anything;
//!   remaining clarifies persist.
//! - `rejects_non_complete_exit` — stub exits non-zero; loom msg --chat
//!   surfaces it as a wrapix-exit error.
//! - `scope_filters_to_spec`  — `-s <label>` narrows the prompt; stub
//!   dumps the prompt and only in-scope IDs are present.

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

/// Install a shell stub at `<dir>/wrapix-bin/wrapix-stub` that pretends
/// to be `wrapix run`. The stub:
///
/// 1. Logs every argv element (one per line) to `<dir>/argv.log` so the
///    `launches_container` test can pin the dispatch shape.
/// 2. Logs the `WRAPIX_DEFAULT_IMAGE_REF` / `_SOURCE` env vars to
///    `<dir>/env.log` so the same test can verify the launcher contract.
/// 3. Optionally dumps the prompt (argv[5]) to `$WRAPIX_STUB_PROMPT_DUMP`.
/// 4. Branches on `$WRAPIX_STUB_MODE`:
///    - `resolve-all` — parses the prompt for `### wx-…` lines and
///      forks `bd update <id> --notes "resolved …" --remove-label
///      loom:clarify` per match.
///    - `resolve-none` (default) — exits 0 immediately.
///    - `emit-blocked` — exits 1 so loom msg --chat surfaces failure.
fn install_wrapix_stub(dir: &Path) -> PathBuf {
    let bin_dir = dir.join("wrapix-bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir wrapix-bin");
    let bin = bin_dir.join("wrapix-stub");
    let argv_log = dir.join("argv.log");
    let env_log = dir.join("env.log");
    let script = format!(
        r#"#!/bin/sh
set -eu

argv_log={argv_log:?}
env_log={env_log:?}

for a in "$@"; do
    printf '%s\n' "$a" >> "$argv_log"
done
printf -- '---\n' >> "$argv_log"

printf 'WRAPIX_DEFAULT_IMAGE_REF=%s\n' "${{WRAPIX_DEFAULT_IMAGE_REF:-}}" >> "$env_log"
printf 'WRAPIX_DEFAULT_IMAGE_SOURCE=%s\n' "${{WRAPIX_DEFAULT_IMAGE_SOURCE:-}}" >> "$env_log"

# Argv layout (per loom-workflow/src/msg/chat.rs::build_wrapix_argv):
#   $1 = "run"
#   $2 = <workspace>
#   $3 = "claude"
#   $4 = "--dangerously-skip-permissions"
#   $5 = <prompt body>
prompt="${{5:-}}"

if [ -n "${{WRAPIX_STUB_PROMPT_DUMP:-}}" ]; then
    printf '%s' "$prompt" > "$WRAPIX_STUB_PROMPT_DUMP"
fi

mode="${{WRAPIX_STUB_MODE:-resolve-none}}"
case "$mode" in
    resolve-all)
        # Parse the rendered msg.md prompt for `### <id> — …` lines and
        # update each bead. Same shape a real claude session would emit
        # (one `bd update` per resolved clarify).
        ids=$(printf '%s\n' "$prompt" | awk '/^### wx-/ {{print $2}}')
        for id in $ids; do
            bd update "$id" --notes "resolved via msg --chat (stub $id)" --remove-label loom:clarify
        done
        ;;
    resolve-none)
        :
        ;;
    emit-blocked)
        exit 1
        ;;
    *)
        echo "wrapix-stub: unknown mode $mode" >&2
        exit 2
        ;;
esac
"#,
        argv_log = argv_log.display(),
        env_log = env_log.display(),
    );
    std::fs::write(&bin, script).expect("write stub");
    let mut perm = std::fs::metadata(&bin).expect("stat stub").permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&bin, perm).expect("chmod stub");
    bin
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

struct ChatRun {
    workspace: PathBuf,
    state_dir: PathBuf,
    bd_bin_dir: PathBuf,
    wrapix_stub: PathBuf,
    manifest: PathBuf,
    argv_log: PathBuf,
    _tmp: tempfile::TempDir,
}

fn setup_chat() -> ChatRun {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().to_path_buf();
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).expect("mkdir state");
    let bd_bin_dir = install_bd_shim(&workspace);
    let wrapix_stub = install_wrapix_stub(&workspace);
    let manifest = write_minimal_manifest(&workspace);
    let argv_log = workspace.join("argv.log");
    ChatRun {
        workspace,
        state_dir,
        bd_bin_dir,
        wrapix_stub,
        manifest,
        argv_log,
        _tmp: tmp,
    }
}

fn run_loom_msg_chat(env: &ChatRun, mode: &str, args: &[&str]) -> std::process::Output {
    run_loom_msg_chat_with_extra_env(env, mode, args, &[])
}

fn run_loom_msg_chat_with_extra_env(
    env: &ChatRun,
    mode: &str,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> std::process::Output {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let mut entries: Vec<PathBuf> = vec![env.bd_bin_dir.clone()];
    entries.extend(std::env::split_paths(&path_var));
    let new_path = std::env::join_paths(entries).expect("join PATH");

    let loom_bin = env!("CARGO_BIN_EXE_loom");

    let mut cmd = Command::new(loom_bin);
    cmd.arg("--workspace")
        .arg(&env.workspace)
        .arg("msg")
        .arg("-c")
        .args(args)
        .env("PATH", new_path)
        .env("LOOM_WRAPIX_BIN", &env.wrapix_stub)
        .env("WRAPIX_STUB_MODE", mode)
        .env("LOOM_BIN", loom_bin)
        .env("LOOM_PROFILES_MANIFEST", &env.manifest)
        .env("BD_STATE_DIR", &env.state_dir)
        .env("XDG_STATE_HOME", env.workspace.join(".loom-test-state"))
        // Bypass the nested-loom guard so cargo test inside a loom container
        // still reaches the msg --chat dispatch path under test.
        .env_remove("LOOM_INSIDE");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().expect("spawn loom")
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

#[test]
fn loom_msg_chat_launches_container() {
    let env = setup_chat();
    seed_bead(
        &env.state_dir,
        "wx-c01",
        "container launch pin",
        "## Options — pick one\n\n### Option 1 — A\nbody\n",
        &["loom:clarify", "spec:scope-a"],
    );
    let output = run_loom_msg_chat(&env, "resolve-none", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom msg --chat must exit 0 on a clean session.\nstdout={stdout}\nstderr={stderr}",
    );

    // wrapix-stub's argv.log holds every argument the dispatch passed.
    // The contract is the same as `loom plan`: `wrapix run <workspace>
    // claude --dangerously-skip-permissions <prompt>` — no `--stdio`,
    // no `--spawn-config` (those are the non-interactive surfaces).
    let argv = std::fs::read_to_string(&env.argv_log).expect("argv.log present");
    let lines: Vec<&str> = argv.lines().collect();
    assert!(
        lines.iter().any(|l| *l == "run"),
        "argv must start with `run` subcommand: {argv:?}",
    );
    assert!(
        lines.iter().any(|l| *l == env.workspace.to_string_lossy()),
        "argv must include the workspace path: {argv:?}",
    );
    assert!(
        lines.iter().any(|l| *l == "claude"),
        "argv must select the claude backend: {argv:?}",
    );
    assert!(
        lines.iter().any(|l| *l == "--dangerously-skip-permissions"),
        "argv must pass `--dangerously-skip-permissions`: {argv:?}",
    );
    assert!(
        !lines.iter().any(|l| *l == "--stdio"),
        "msg --chat must NOT use the pi-mono `--stdio` flag: {argv:?}",
    );
    assert!(
        !lines.iter().any(|l| *l == "--spawn-config"),
        "msg --chat must NOT use `--spawn-config`: {argv:?}",
    );

    // The launcher-image env vars match the manifest entry — same
    // contract `loom plan` enforces.
    let env_log = std::fs::read_to_string(env.workspace.join("env.log")).unwrap_or_default();
    assert!(
        env_log.contains("WRAPIX_DEFAULT_IMAGE_REF=localhost/wrapix-base:test"),
        "env.log missing image ref: {env_log}",
    );
    assert!(
        stdout.contains("loom msg --chat"),
        "expected a session-summary line on stdout: {stdout:?}",
    );
}

#[test]
fn loom_msg_chat_writes_notes_and_clears_labels() {
    let env = setup_chat();
    for id in ["wx-w01", "wx-w02", "wx-w03"] {
        seed_bead(
            &env.state_dir,
            id,
            &format!("note-pin {id}"),
            "## Options — choose\n\n### Option 1 — only\nbody\n",
            &["loom:clarify", "spec:notes"],
        );
    }
    let output = run_loom_msg_chat(&env, "resolve-all", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom msg --chat must exit 0 when resolve-all completes.\n\
         stdout={stdout}\nstderr={stderr}",
    );
    let log = read_invocation_log(&env.state_dir);
    for id in ["wx-w01", "wx-w02", "wx-w03"] {
        assert!(
            log.contains(&format!("update {id}")),
            "expected bd update call for {id}: {log}",
        );
        let notes = read_field(&env.state_dir, id, "notes");
        assert!(
            notes.contains("resolved via msg --chat"),
            "bead {id} notes not updated: {notes:?}",
        );
        let labels = read_labels(&env.state_dir, id);
        assert!(
            !labels.iter().any(|l| l == "loom:clarify"),
            "bead {id} should have lost loom:clarify label: {labels:?}",
        );
    }
    assert!(
        stdout.contains("resolved 3"),
        "summary must report 3 resolved beads: {stdout:?}",
    );
}

#[test]
fn loom_msg_chat_partial_progress_leaves_unresolved_clarifies_open() {
    let env = setup_chat();
    seed_bead(
        &env.state_dir,
        "wx-p01",
        "partial",
        "## Options — choose\n\n### Option 1 — only\nbody\n",
        &["loom:clarify", "spec:partial"],
    );
    let output = run_loom_msg_chat(&env, "resolve-none", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "partial-progress session must exit 0 (clean per spec).\n\
         stdout={stdout}\nstderr={stderr}",
    );
    let labels = read_labels(&env.state_dir, "wx-p01");
    assert!(
        labels.iter().any(|l| l == "loom:clarify"),
        "unresolved bead must keep loom:clarify: {labels:?}",
    );
    let notes = read_field(&env.state_dir, "wx-p01", "notes");
    assert!(
        notes.is_empty(),
        "unresolved bead notes should be empty: {notes:?}",
    );
    assert!(
        stdout.contains("remaining 1"),
        "summary must report 1 remaining bead: {stdout:?}",
    );
}

#[test]
fn loom_msg_chat_rejects_non_complete_exit_signal() {
    let env = setup_chat();
    seed_bead(
        &env.state_dir,
        "wx-x01",
        "exit-signal",
        "## Options — choose\n\n### Option 1 — only\nbody\n",
        &["loom:clarify", "spec:exit"],
    );
    let output = run_loom_msg_chat(&env, "emit-blocked", &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "wrapix-stub exit 1 must fail the session: stderr={stderr}",
    );
    assert!(
        stderr.contains("wrapix exited") || stderr.contains("exit status"),
        "error must reference the wrapix exit status: stderr={stderr}",
    );
}

#[test]
fn loom_msg_chat_scope_filters_to_spec() {
    let env = setup_chat();
    seed_bead(
        &env.state_dir,
        "wx-s01",
        "in-scope alpha",
        "## Options — choose\n\n### Option 1 — only\nbody\n",
        &["loom:clarify", "spec:alpha"],
    );
    seed_bead(
        &env.state_dir,
        "wx-s02",
        "out-of-scope beta",
        "## Options — choose\n\n### Option 1 — only\nbody\n",
        &["loom:clarify", "spec:beta"],
    );
    let prompt_dump = env.workspace.join("prompt-dump.txt");
    let output = run_loom_msg_chat_with_extra_env(
        &env,
        "resolve-none",
        &["-s", "alpha"],
        &[("WRAPIX_STUB_PROMPT_DUMP", &prompt_dump.to_string_lossy())],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "scope filter session must exit 0.\nstdout={stdout}\nstderr={stderr}",
    );
    let dumped = std::fs::read_to_string(&prompt_dump)
        .unwrap_or_else(|e| panic!("read prompt dump {}: {e}", prompt_dump.display()));
    assert!(
        dumped.contains("wx-s01"),
        "in-scope bead must appear in prompt: {dumped:.500?}",
    );
    assert!(
        !dumped.contains("wx-s02"),
        "out-of-scope bead must NOT appear in prompt: {dumped:.500?}",
    );
}

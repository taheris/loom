//! `loom msg --chat` integration tests.
//!
//! `loom msg --chat` mirrors `loom plan`'s shape: a single interactive
//! `wrix run <workspace> <agent command> ... <prompt>` shell-out with
//! **inherited stdio** so the configured agent attaches directly to the
//! user's terminal as a real REPL. There is no pi-mono protocol involved here
//! — the tests use a shell stub that records argv and (per the test
//! mode) forks `bd update` calls or exits non-zero.
//!
//! Five distinct slices, one per `test_msg_chat_*` dispatcher:
//!
//! - `launches_container`     — argv shape plus the
//!   `WRIX_DEFAULT_IMAGE_REF` / `_SOURCE` env vars the launcher reads.
//! - `writes_notes`           — stub parses the prompt for `### <id>`
//!   headers and forks `bd update <id> --notes "…" --remove-label
//!   loom:clarify` per bead; bd-shim log + bead state reflect it.
//! - `partial_progress`       — stub exits 0 without resolving anything;
//!   remaining clarifies persist.
//! - `rejects_non_complete_exit` — stub exits non-zero; loom msg --chat
//!   surfaces it as a wrix-exit error.
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

/// Install a shell stub at `<dir>/wrix-bin/wrix-stub` that pretends
/// to be `wrix run`. The stub:
///
/// 1. Logs every argv element (one per line) to `<dir>/argv.log` so the
///    `launches_container` test can pin the dispatch shape.
/// 2. Logs the `WRIX_DEFAULT_IMAGE_REF` / `_SOURCE` and `WRIX_AGENT`
///    env vars to `<dir>/env.log` so the same test can verify the launcher
///    contract.
/// 3. Optionally dumps the prompt (argv[5]) to `$WRIX_STUB_PROMPT_DUMP`.
/// 4. Branches on `$WRIX_STUB_MODE`:
///    - `resolve-all` — parses the prompt for `### lm-…` lines and
///      forks `bd update <id> --notes "resolved …" --remove-label
///      loom:clarify` per match.
///    - `resolve-none` (default) — exits 0 immediately.
///    - `emit-blocked` — exits 1 so loom msg --chat surfaces failure.
fn install_wrix_stub(dir: &Path) -> PathBuf {
    let bin_dir = dir.join("wrix-bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir wrix-bin");
    let bin = bin_dir.join("wrix-stub");
    let argv_log = dir.join("argv.log");
    let env_log = dir.join("env.log");
    let script = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

argv_log={argv_log:?}
env_log={env_log:?}

for a in "$@"; do
    printf '%s\n' "$a" >> "$argv_log"
done
printf -- '---\n' >> "$argv_log"

printf 'WRIX_DEFAULT_IMAGE_REF=%s\n' "${{WRIX_DEFAULT_IMAGE_REF:-}}" >> "$env_log"
printf 'WRIX_DEFAULT_IMAGE_SOURCE=%s\n' "${{WRIX_DEFAULT_IMAGE_SOURCE:-}}" >> "$env_log"
printf 'WRIX_AGENT=%s\n' "${{WRIX_AGENT:-}}" >> "$env_log"

# Argv layout (per loom-workflow/src/msg/chat.rs::build_wrix_argv):
#   $1 = "run"
#   $2 = <workspace>
#   Claude: $3 = "claude", $4 = "--dangerously-skip-permissions", $5 = <prompt body>
#   Pi:     $3 = "pi",     $4 = <prompt body>
# Profile selection rides the WRIX_DEFAULT_IMAGE_* env vars, NOT argv —
# `wrix run` has no --profile parser; any extra tokens between the
# workspace and agent command would be forwarded into the container as a
# command and exit 127.
if [[ "${{3:-}}" == "pi" ]]; then
    prompt="${{4:-}}"
else
    prompt="${{5:-}}"
fi

if [[ -n "${{WRIX_STUB_PROMPT_DUMP:-}}" ]]; then
    printf '%s' "$prompt" > "$WRIX_STUB_PROMPT_DUMP"
fi

mode="${{WRIX_STUB_MODE:-resolve-none}}"
case "$mode" in
    resolve-all)
        # Parse the rendered msg.md prompt for `### <id> — …` lines and
        # update each bead. Same shape a real claude session would emit
        # (one `bd update` per resolved clarify).
        while IFS= read -r id; do
            bd update "$id" --notes "resolved via msg --chat (stub $id)" --remove-label loom:clarify
        done < <(printf '%s\n' "$prompt" | awk '/^### lm-/ {{print $2}}')
        ;;
    notes-only)
        # The persistence-boundary contract per specs/harness.md: agent
        # only writes the resolution note; the driver runs the unblock
        # transition (--status=open + label removal) after the session.
        while IFS= read -r id; do
            bd update "$id" --notes "resolved via msg --chat (notes-only stub $id)"
        done < <(printf '%s\n' "$prompt" | awk '/^### lm-/ {{print $2}}')
        ;;
    bd-close)
        # Adversarial: the agent infers `bd close` from the worker-phase
        # `LOOM_COMPLETE` framing and closes the bead. The driver must
        # still issue the canonical unblock so the bead re-opens for
        # the next implementing session.
        while IFS= read -r id; do
            bd close "$id"
        done < <(printf '%s\n' "$prompt" | awk '/^### lm-/ {{print $2}}')
        ;;
    resolve-none)
        :
        ;;
    emit-blocked)
        exit 1
        ;;
    *)
        echo "wrix-stub: unknown mode $mode" >&2
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
        r#"{{"base": {{"pi": {{"ref":"localhost/wrix-base-pi:test","source":{source:?}}}, "claude": {{"ref":"localhost/wrix-base-claude:test","source":{source:?}}}, "direct": {{"ref":"localhost/wrix-base-direct:test","source":{source:?}}}}}}}"#,
        source = source.display().to_string(),
    );
    std::fs::write(&manifest, body).expect("write manifest");
    manifest
}

struct ChatRun {
    workspace: PathBuf,
    state_dir: PathBuf,
    bd_bin_dir: PathBuf,
    wrix_stub: PathBuf,
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
    let wrix_stub = install_wrix_stub(&workspace);
    let manifest = write_minimal_manifest(&workspace);
    let argv_log = workspace.join("argv.log");
    ChatRun {
        workspace,
        state_dir,
        bd_bin_dir,
        wrix_stub,
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
        .env("LOOM_WRIX_BIN", &env.wrix_stub)
        .env("WRIX_STUB_MODE", mode)
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
        "lm-c01",
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

    // wrix-stub's argv.log holds every argument the dispatch passed.
    // The contract is the same as `loom plan`: interactive `wrix run`
    // with no `--stdio` and no `--spawn-config` (those are the
    // non-interactive surfaces).
    let argv = std::fs::read_to_string(&env.argv_log).expect("argv.log present");
    let lines: Vec<&str> = argv.lines().collect();
    assert!(
        lines.contains(&"run"),
        "argv must start with `run` subcommand: {argv:?}",
    );
    assert!(
        lines.iter().any(|l| *l == env.workspace.to_string_lossy()),
        "argv must include the workspace path: {argv:?}",
    );
    assert!(
        lines.contains(&"claude"),
        "argv must select the claude backend: {argv:?}",
    );
    assert!(
        lines.contains(&"--dangerously-skip-permissions"),
        "argv must pass `--dangerously-skip-permissions`: {argv:?}",
    );
    assert!(
        !lines.contains(&"--stdio"),
        "msg --chat must NOT use the pi-mono `--stdio` flag: {argv:?}",
    );
    assert!(
        !lines.contains(&"--spawn-config"),
        "msg --chat must NOT use `--spawn-config`: {argv:?}",
    );

    // The launcher-image env vars match the manifest entry — same
    // contract `loom plan` enforces.
    let env_log = std::fs::read_to_string(env.workspace.join("env.log")).unwrap_or_default();
    assert!(
        env_log.contains("WRIX_DEFAULT_IMAGE_REF=localhost/wrix-base-claude:test"),
        "env.log missing image ref: {env_log}",
    );
    assert!(
        stdout.contains("loom msg --chat"),
        "expected a session-summary line on stdout: {stdout:?}",
    );
}

#[test]
fn loom_msg_chat_phase_agent_pi_selects_pi_command() {
    let env = setup_chat();
    std::fs::write(
        env.workspace.join("loom.toml"),
        "[phase.default]\nagent.backend = \"pi\"\n",
    )
    .expect("write loom.toml");
    seed_bead(
        &env.state_dir,
        "lm-pi01",
        "pi command pin",
        "## Options — pick one\n\n### Option 1 — A\nbody\n",
        &["loom:clarify", "spec:scope-a"],
    );

    let output = run_loom_msg_chat(&env, "resolve-none", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom msg --chat must exit 0 on a clean pi session.\nstdout={stdout}\nstderr={stderr}",
    );

    let argv = std::fs::read_to_string(&env.argv_log).expect("argv.log present");
    let lines: Vec<&str> = argv.lines().collect();
    assert!(lines.contains(&"pi"), "expected pi argv: {argv}");
    assert!(
        !lines.contains(&"claude"),
        "pi-backed msg chat must not call claude: {argv}",
    );
    assert!(
        !lines.contains(&"--dangerously-skip-permissions"),
        "pi-backed msg chat must not receive claude-only flags: {argv}",
    );
}

/// The resolved profile (from `LoomConfig::agent_for(Phase::Msg)` or
/// the CLI override) flows to `wrix run` via the
/// `WRIX_DEFAULT_IMAGE_REF` / `WRIX_DEFAULT_IMAGE_SOURCE` env vars
/// — not via argv. `wrix run` has no `--profile` parser; any
/// extra tokens between the workspace and the agent command would be
/// forwarded into the container as a command and exit 127.
#[test]
fn msg_chat_passes_resolved_profile_runtime_to_wrix_run() {
    let env = setup_chat();
    std::fs::write(
        env.workspace.join("loom.toml"),
        "[phase.msg]\nprofile = \"base\"\nagent.backend = \"pi\"\n",
    )
    .expect("write loom.toml");
    seed_bead(
        &env.state_dir,
        "lm-pf01",
        "profile pin",
        "## Options — choose\n\n### Option 1 — only\nbody\n",
        &["loom:clarify", "spec:profile"],
    );
    let output = run_loom_msg_chat(&env, "resolve-none", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom msg --chat must exit 0 on a clean session.\nstdout={stdout}\nstderr={stderr}",
    );

    let argv = std::fs::read_to_string(&env.argv_log).expect("argv.log present");
    let lines: Vec<&str> = argv.lines().collect();
    assert!(lines.contains(&"pi"), "expected pi argv: {argv}");
    assert!(
        !lines.contains(&"claude"),
        "pi-backed msg chat must not call claude: {argv}",
    );
    assert!(
        !lines.contains(&"--dangerously-skip-permissions"),
        "pi-backed msg chat must not receive claude flags: {argv}",
    );
    assert!(
        !lines.contains(&"--profile"),
        "wrix run has no --profile parser; the flag must not appear in argv. \
         argv.log:\n{argv}",
    );

    let env_log = std::fs::read_to_string(env.workspace.join("env.log")).expect("env.log present");
    assert!(
        env_log.contains("WRIX_DEFAULT_IMAGE_REF=localhost/wrix-base-pi:test"),
        "phase msg profile/runtime must select the matching image ref \
         via env var. env.log:\n{env_log}",
    );
    assert!(
        env_log.contains("WRIX_DEFAULT_IMAGE_SOURCE=") && env_log.contains("base.tar"),
        "phase msg profile/runtime must select the matching image source \
         via env var. env.log:\n{env_log}",
    );
    assert!(
        env_log.contains("WRIX_AGENT=pi"),
        "phase msg runtime must set backend-derived WRIX_AGENT. env.log:\n{env_log}",
    );
}

#[test]
fn msg_chat_rejects_direct_backend_before_wrix_run() {
    let env = setup_chat();
    std::fs::write(
        env.workspace.join("loom.toml"),
        "[phase.msg]\nagent.backend = \"direct\"\n",
    )
    .expect("write loom.toml");
    seed_bead(
        &env.state_dir,
        "lm-dir01",
        "direct pin",
        "## Options — choose\n\n### Option 1 — only\nbody\n",
        &["loom:clarify", "spec:direct"],
    );

    let output = run_loom_msg_chat(&env, "resolve-none", &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "direct-backed msg chat must fail before spawning wrix",
    );
    assert!(
        stderr.contains("direct backend cannot run interactive `loom msg --chat`"),
        "stderr must name the unsupported interactive direct backend: {stderr}",
    );
    assert!(
        !env.argv_log.exists(),
        "wrix stub must not be invoked when direct is selected",
    );
}

#[test]
fn loom_msg_chat_writes_notes_and_clears_labels() {
    let env = setup_chat();
    for id in ["lm-w01", "lm-w02", "lm-w03"] {
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
    for id in ["lm-w01", "lm-w02", "lm-w03"] {
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
        "lm-p01",
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
    let labels = read_labels(&env.state_dir, "lm-p01");
    assert!(
        labels.iter().any(|l| l == "loom:clarify"),
        "unresolved bead must keep loom:clarify: {labels:?}",
    );
    let notes = read_field(&env.state_dir, "lm-p01", "notes");
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
        "lm-x01",
        "exit-signal",
        "## Options — choose\n\n### Option 1 — only\nbody\n",
        &["loom:clarify", "spec:exit"],
    );
    let output = run_loom_msg_chat(&env, "emit-blocked", &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "wrix-stub exit 1 must fail the session: stderr={stderr}",
    );
    assert!(
        stderr.contains("wrix exited") || stderr.contains("exit status"),
        "error must reference the wrix exit status: stderr={stderr}",
    );
}

#[test]
fn loom_msg_chat_scope_filters_to_spec() {
    let env = setup_chat();
    seed_bead(
        &env.state_dir,
        "lm-s01",
        "in-scope alpha",
        "## Options — choose\n\n### Option 1 — only\nbody\n",
        &["loom:clarify", "spec:alpha"],
    );
    seed_bead(
        &env.state_dir,
        "lm-s02",
        "out-of-scope beta",
        "## Options — choose\n\n### Option 1 — only\nbody\n",
        &["loom:clarify", "spec:beta"],
    );
    let prompt_dump = env.workspace.join("prompt-dump.txt");
    let output = run_loom_msg_chat_with_extra_env(
        &env,
        "resolve-none",
        &["-s", "alpha"],
        &[("WRIX_STUB_PROMPT_DUMP", &prompt_dump.to_string_lossy())],
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
        dumped.contains("lm-s01"),
        "in-scope bead must appear in prompt: {dumped:.500?}",
    );
    assert!(
        !dumped.contains("lm-s02"),
        "out-of-scope bead must NOT appear in prompt: {dumped:.500?}",
    );
}

/// Persistence-boundary contract per `specs/templates.md` Implementation
/// Note 5: interactive sessions (msg, plan_*) own their own bd state.
/// The driver does NOT reconcile bd state after the session — no
/// `--status=open`, no `--remove-label`. Whatever bd state the chat
/// agent + human established at session end IS the canonical state.
///
/// In this scenario the stub writes only the resolution note and does
/// NOT clear the `loom:clarify` label. The bead therefore stays
/// labelled; the driver does not auto-unblock it. The human catches
/// any mis-application in the next `loom msg` session.
#[test]
fn loom_msg_chat_driver_does_not_auto_unblock_after_notes_only_write() {
    let env = setup_chat();
    seed_bead(
        &env.state_dir,
        "lm-n01",
        "notes-only flow",
        "## Options — choose\n\n### Option 1 — only\nbody\n",
        &["loom:clarify", "spec:notes-only"],
    );
    let output = run_loom_msg_chat(&env, "notes-only", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "notes-only chat must exit 0.\nstdout={stdout}\nstderr={stderr}",
    );

    let notes = read_field(&env.state_dir, "lm-n01", "notes");
    assert!(
        notes.contains("notes-only stub"),
        "agent's resolution note must persist: {notes:?}",
    );

    // Driver does not touch the label set — the agent wrote notes only,
    // so the clarify label stays in place.
    let labels = read_labels(&env.state_dir, "lm-n01");
    assert!(
        labels.iter().any(|l| l == "loom:clarify"),
        "driver must NOT auto-remove loom:clarify on a notes-only resolution \
         (the agent owns the label transition): {labels:?}",
    );

    let log = read_invocation_log(&env.state_dir);
    let unblock_calls: Vec<&str> = log
        .lines()
        .filter(|line| {
            line.starts_with("update lm-n01")
                && line.contains("--status")
                && line.contains("open")
                && line.contains("--remove-label")
        })
        .collect();
    assert!(
        unblock_calls.is_empty(),
        "driver must NOT issue an unblock `bd update` after the session; got {} \
         matching lines in:\n{}",
        unblock_calls.len(),
        log,
    );
}

/// Adversarial: when the agent closes the bead, the driver does NOT
/// reverse the close. Per `specs/templates.md` Implementation Note 5,
/// the chat agent has full bd-write authority; the previous "driver
/// reverses agent-applied bd close" behavior is removed.
#[test]
fn loom_msg_chat_driver_does_not_reverse_agent_bd_close() {
    let env = setup_chat();
    seed_bead(
        &env.state_dir,
        "lm-bc01",
        "agent applies bd close",
        "## Options — choose\n\n### Option 1 — only\nbody\n",
        &["loom:clarify", "spec:close-discipline"],
    );
    let output = run_loom_msg_chat(&env, "bd-close", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "session must exit 0 when the agent applies bd close.\n\
         stdout={stdout}\nstderr={stderr}",
    );

    let log = read_invocation_log(&env.state_dir);
    assert!(
        log.lines().any(|line| line.starts_with("close lm-bc01")),
        "test setup smoke check: stub must have recorded the close: {log}",
    );

    // Driver must NOT run any post-session `bd update` against the closed
    // bead — no status reset, no label removal.
    let post_session_updates: Vec<&str> = log
        .lines()
        .filter(|line| line.starts_with("update lm-bc01"))
        .collect();
    assert!(
        post_session_updates.is_empty(),
        "driver must NOT issue any `bd update` after the agent closed the \
         bead; got {} matching lines in:\n{}",
        post_session_updates.len(),
        log,
    );

    let status = read_field(&env.state_dir, "lm-bc01", "status");
    assert_eq!(
        status.trim(),
        "closed",
        "agent's bd close must persist; the driver does not reverse it",
    );
}

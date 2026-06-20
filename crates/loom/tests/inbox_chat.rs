//! `loom inbox chat` integration tests.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn seed_bead(
    state_dir: &Path,
    id: &str,
    title: &str,
    description: &str,
    status: &str,
    labels: &[&str],
) {
    let bead_dir = state_dir.join(id);
    std::fs::create_dir_all(&bead_dir).expect("mkdir bead dir");
    std::fs::write(bead_dir.join("title"), title).expect("write title");
    std::fs::write(bead_dir.join("description"), description).expect("write description");
    std::fs::write(bead_dir.join("status"), status).expect("write status");
    std::fs::write(bead_dir.join("priority"), "2").expect("write priority");
    std::fs::write(bead_dir.join("issue_type"), "task").expect("write issue_type");
    std::fs::write(bead_dir.join("labels"), labels.join("\n")).expect("write labels");
}

fn seed_metadata(state_dir: &Path, id: &str, metadata: serde_json::Value) {
    std::fs::write(
        state_dir.join(id).join("metadata.json"),
        serde_json::to_string(&metadata).expect("metadata json"),
    )
    .expect("write metadata");
}

fn install_bd_shim(dir: &Path) -> PathBuf {
    let bin_dir = dir.join("bd-bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir bd-bin");
    let bd_path = bin_dir.join("bd");
    let source = PathBuf::from(env!("CARGO_BIN_EXE_bd-shim"));
    match std::os::unix::fs::symlink(&source, &bd_path) {
        Ok(()) => {}
        Err(_) => {
            std::fs::copy(&source, &bd_path).expect("copy bd-shim");
            let mut perm = std::fs::metadata(&bd_path).expect("stat bd").permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&bd_path, perm).expect("chmod bd");
        }
    }
    bin_dir
}

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
        while IFS= read -r id; do
            bd update "$id" --notes "resolved via inbox chat (stub $id)" --remove-label loom:clarify --status open
        done < <(printf '%s\n' "$prompt" | awk '/^### [0-9]+\. lm-/ {{print $3}}')
        ;;
    notes-only)
        while IFS= read -r id; do
            bd update "$id" --notes "resolved via inbox chat (notes-only stub $id)"
        done < <(printf '%s\n' "$prompt" | awk '/^### [0-9]+\. lm-/ {{print $3}}')
        ;;
    bd-close)
        while IFS= read -r id; do
            bd close "$id"
        done < <(printf '%s\n' "$prompt" | awk '/^### [0-9]+\. lm-/ {{print $3}}')
        ;;
    resolve-none)
        :
        ;;
    fail)
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

fn write_manifest(dir: &Path) -> PathBuf {
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
    env_log: PathBuf,
    _tmp: tempfile::TempDir,
}

fn setup_chat() -> ChatRun {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().to_path_buf();
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).expect("mkdir state");
    let bd_bin_dir = install_bd_shim(&workspace);
    let wrix_stub = install_wrix_stub(&workspace);
    let manifest = write_manifest(&workspace);
    ChatRun {
        argv_log: workspace.join("argv.log"),
        env_log: workspace.join("env.log"),
        workspace,
        state_dir,
        bd_bin_dir,
        wrix_stub,
        manifest,
        _tmp: tmp,
    }
}

fn run_chat(env: &ChatRun, mode: &str, args: &[&str]) -> std::process::Output {
    run_chat_extra(env, mode, args, &[])
}

fn run_chat_extra(
    env: &ChatRun,
    mode: &str,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> std::process::Output {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let mut entries: Vec<PathBuf> = vec![env.bd_bin_dir.clone()];
    entries.extend(std::env::split_paths(&path_var));
    let new_path = std::env::join_paths(entries).expect("join PATH");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_loom"));
    cmd.arg("--workspace")
        .arg(&env.workspace)
        .arg("inbox")
        .arg("chat")
        .args(args)
        .env("PATH", new_path)
        .env("LOOM_WRIX_BIN", &env.wrix_stub)
        .env("WRIX_STUB_MODE", mode)
        .env("LOOM_PROFILES_MANIFEST", &env.manifest)
        .env("BD_STATE_DIR", &env.state_dir)
        .env("XDG_STATE_HOME", env.workspace.join(".loom-test-state"))
        .env_remove("LOOM_INSIDE");
    for (key, value) in extra_env {
        cmd.env(key, value);
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
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect()
}

#[test]
fn loom_inbox_chat_launches_container() {
    let env = setup_chat();
    seed_bead(
        &env.state_dir,
        "lm-chat01",
        "needs decision",
        "## Options — pick\n\n### Option 1 — A\nbody\n",
        "open",
        &["loom:clarify", "spec:agent"],
    );

    let output = run_chat(&env, "resolve-none", &[]);
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let argv = std::fs::read_to_string(&env.argv_log).expect("argv log");
    let lines: Vec<&str> = argv.lines().collect();
    assert_eq!(lines[0], "run");
    assert_eq!(lines[1], env.workspace.to_string_lossy());
    assert_eq!(lines[2], "claude");
    assert!(lines.contains(&"--dangerously-skip-permissions"), "{argv}");
    assert!(!lines.contains(&"--stdio"), "{argv}");
    assert!(!lines.contains(&"--spawn-config"), "{argv}");
    let env_log = std::fs::read_to_string(&env.env_log).expect("env log");
    assert!(env_log.contains("WRIX_DEFAULT_IMAGE_REF=localhost/wrix-base-claude:test"));
    assert!(env_log.contains("WRIX_AGENT=claude"));
}

#[test]
fn inbox_chat_passes_resolved_profile_runtime_to_wrix_run() {
    let env = setup_chat();
    std::fs::write(
        env.workspace.join("loom.toml"),
        "[phase.inbox]\nprofile = \"base\"\nagent.backend = \"pi\"\n",
    )
    .expect("write config");
    seed_bead(
        &env.state_dir,
        "lm-chat02",
        "needs decision",
        "blocked",
        "open",
        &["loom:blocked", "spec:agent"],
    );

    let output = run_chat(&env, "resolve-none", &[]);
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let argv = std::fs::read_to_string(&env.argv_log).expect("argv log");
    let lines: Vec<&str> = argv.lines().collect();
    assert_eq!(lines[2], "pi");
    assert!(!lines.contains(&"--dangerously-skip-permissions"), "{argv}");
    let env_log = std::fs::read_to_string(&env.env_log).expect("env log");
    assert!(env_log.contains("WRIX_DEFAULT_IMAGE_REF=localhost/wrix-base-pi:test"));
    assert!(env_log.contains("WRIX_AGENT=pi"));
}

#[test]
fn inbox_chat_rejects_direct_backend_before_wrix_run() {
    let env = setup_chat();
    std::fs::write(
        env.workspace.join("loom.toml"),
        "[phase.inbox]\nagent.backend = \"direct\"\n",
    )
    .expect("write config");
    seed_bead(
        &env.state_dir,
        "lm-direct",
        "direct rejected",
        "blocked",
        "open",
        &["loom:blocked"],
    );

    let output = run_chat(&env, "resolve-none", &[]);
    assert!(!output.status.success(), "direct inbox chat must fail");
    assert!(!env.argv_log.exists(), "wrix must not be spawned");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("direct backend cannot run interactive `loom inbox chat`"),
        "{stderr}",
    );
}

#[test]
fn loom_inbox_chat_writes_notes_and_clears_labels() {
    let env = setup_chat();
    seed_bead(
        &env.state_dir,
        "lm-resolve",
        "resolve me",
        "## Options — choose\n\n### Option 1 — A\nbody\n",
        "blocked",
        &["loom:clarify", "spec:agent"],
    );

    let output = run_chat(&env, "resolve-all", &[]);
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let notes = read_field(&env.state_dir, "lm-resolve", "notes");
    assert!(notes.contains("resolved via inbox chat"), "{notes}");
    assert_eq!(read_field(&env.state_dir, "lm-resolve", "status"), "open");
    let labels = read_labels(&env.state_dir, "lm-resolve");
    assert!(
        !labels.iter().any(|label| label == "loom:clarify"),
        "{labels:?}"
    );
}

#[test]
fn loom_inbox_chat_scope_filters_queue() {
    let env = setup_chat();
    seed_bead(
        &env.state_dir,
        "lm-alpha",
        "alpha",
        "alpha body",
        "open",
        &["loom:clarify", "spec:alpha"],
    );
    seed_bead(
        &env.state_dir,
        "lm-beta",
        "beta",
        "beta body",
        "open",
        &["loom:blocked", "spec:beta"],
    );
    seed_bead(
        &env.state_dir,
        "lm-tune",
        "tune",
        "tune body",
        "open",
        &["loom:tune", "spec:alpha"],
    );
    seed_metadata(
        &env.state_dir,
        "lm-tune",
        serde_json::json!({"loom.tune.state":"pending"}),
    );
    let prompt_dump = env.workspace.join("prompt.txt");
    let dump = prompt_dump.to_string_lossy().into_owned();

    let output = run_chat_extra(
        &env,
        "resolve-none",
        &["-s", "alpha", "-k", "tune"],
        &[("WRIX_STUB_PROMPT_DUMP", dump.as_str())],
    );
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let prompt = std::fs::read_to_string(prompt_dump).expect("prompt dump");
    assert!(prompt.contains("lm-tune"), "{prompt}");
    assert!(!prompt.contains("lm-alpha"), "{prompt}");
    assert!(!prompt.contains("lm-beta"), "{prompt}");
}

#[test]
fn inbox_chat_targeting_focuses_single_item() {
    let env = setup_chat();
    seed_bead(
        &env.state_dir,
        "lm-one",
        "one",
        "one body",
        "open",
        &["loom:clarify"],
    );
    seed_bead(
        &env.state_dir,
        "lm-two",
        "two",
        "two body",
        "open",
        &["loom:blocked"],
    );
    let prompt_dump = env.workspace.join("target.txt");
    let dump = prompt_dump.to_string_lossy().into_owned();

    let output = run_chat_extra(
        &env,
        "resolve-none",
        &["-b", "lm-two"],
        &[("WRIX_STUB_PROMPT_DUMP", dump.as_str())],
    );
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let prompt = std::fs::read_to_string(prompt_dump).expect("prompt dump");
    assert!(prompt.contains("lm-two"), "{prompt}");
    assert!(!prompt.contains("lm-one"), "{prompt}");
}

#[test]
fn inbox_chat_driver_does_not_reconcile_bd_state_after_session() {
    let env = setup_chat();
    seed_bead(
        &env.state_dir,
        "lm-notes",
        "notes only",
        "blocked",
        "blocked",
        &["loom:blocked"],
    );

    let output = run_chat(&env, "notes-only", &[]);
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(read_field(&env.state_dir, "lm-notes", "status"), "blocked");
    let labels = read_labels(&env.state_dir, "lm-notes");
    assert!(
        labels.iter().any(|label| label == "loom:blocked"),
        "{labels:?}"
    );
    let log = read_invocation_log(&env.state_dir);
    assert!(
        !log.contains("--remove-label loom:blocked --status open"),
        "{log}"
    );
}

#[test]
fn inbox_chat_driver_does_not_reverse_agent_bd_close() {
    let env = setup_chat();
    seed_bead(
        &env.state_dir,
        "lm-close",
        "close me",
        "blocked",
        "blocked",
        &["loom:blocked"],
    );

    let output = run_chat(&env, "bd-close", &[]);
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(read_field(&env.state_dir, "lm-close", "status"), "closed");
    let log = read_invocation_log(&env.state_dir);
    assert!(!log.contains("bd update lm-close --status open"), "{log}");
}

#[test]
fn loom_inbox_chat_crash_exits_nonzero_without_auto_retry() {
    let env = setup_chat();
    seed_bead(
        &env.state_dir,
        "lm-fail",
        "fail",
        "blocked",
        "open",
        &["loom:blocked"],
    );

    let output = run_chat(&env, "fail", &[]);
    assert!(
        !output.status.success(),
        "failing wrix stub must fail inbox chat"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("wrix exited"), "{stderr}");
    let log = read_invocation_log(&env.state_dir);
    assert_eq!(
        log.matches("list --json").count(),
        1,
        "no retry/reconcile list after failure: {log}"
    );
}

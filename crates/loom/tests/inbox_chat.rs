//! `loom inbox chat` integration tests.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use loom_driver::git::{
    GitClient, bare_origin_path, commit_all_in, init_test_repo_with_integration,
    status_porcelain_sync, sync_head_commit_sha, sync_rev_parse,
};

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
    accept-tune)
        while IFS= read -r id; do
            bd update "$id" --status open --set-metadata loom.tune.state=accepted
        done < <(printf '%s\n' "$prompt" | awk '/^### [0-9]+\. lm-/ && /\[tune\]/ {{print $3}}')
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

marker="${{WRIX_STUB_MARKER:-LOOM_COMPLETE}}"
if [[ -n "$marker" ]]; then
    printf '%s\n' "$marker"
fi
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
        r#"{{"base": {{"pi": {{"ref":"localhost/wrix-base-pi:test","source":{source:?}, "source_kind": "nix-descriptor"}}, "claude": {{"ref":"localhost/wrix-base-claude:test","source":{source:?}, "source_kind": "nix-descriptor"}}, "direct": {{"ref":"localhost/wrix-base-direct:test","source":{source:?}, "source_kind": "nix-descriptor"}}}}}}"#,
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

fn read_metadata(state_dir: &Path, id: &str) -> serde_json::Value {
    let raw = read_field(state_dir, id, "metadata.json");
    serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({}))
}

fn install_apply_loom_stub(dir: &Path) -> PathBuf {
    let bin_dir = dir.join("apply-loom-bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir apply loom bin");
    let bin = bin_dir.join("loom-apply-stub");
    let log = dir.join("apply-loom.log");
    let script = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

log={log:?}
printf '%s\n' "$*" >> "$log"

case "${{1:-}} ${{2:-}}" in
    'gate verify')
        if [[ "${{LOOM_APPLY_STUB_VERIFY:-pass}}" == "fail" ]]; then
            printf 'verify failed\n' >&2
            exit 42
        fi
        printf 'LOOM_COMPLETE\n'
        ;;
    'gate review')
        case "${{LOOM_APPLY_STUB_REVIEW:-pass}}" in
            pass)
                printf 'LOOM_COMPLETE\n'
                ;;
            fail)
                printf 'review failed\n' >&2
                exit 43
                ;;
            concern)
                printf 'LOOM_CONCERN: {{"summary":"review failed"}}\n'
                ;;
            *)
                printf 'unknown review mode %s\n' "${{LOOM_APPLY_STUB_REVIEW:-}}" >&2
                exit 44
                ;;
        esac
        ;;
    *)
        printf 'unexpected loom stub argv: %s\n' "$*" >&2
        exit 2
        ;;
esac
"#,
        log = log.display(),
    );
    std::fs::write(&bin, script).expect("write apply loom stub");
    let mut perm = std::fs::metadata(&bin)
        .expect("stat apply loom stub")
        .permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&bin, perm).expect("chmod apply loom stub");
    bin
}

fn create_tune_proposal(env: &ChatRun, id: &str, edits: &[(&str, &str)]) {
    let git = GitClient::open(&env.workspace).expect("open workspace git");
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let base = runtime
        .block_on(git.head_commit_sha())
        .expect("base commit")
        .to_string();
    let branch = format!("loom/tune/{id}");
    let repo = env.workspace.join(".loom/tune").join(id).join("repo");
    runtime
        .block_on(git.create_tune_checkout(&repo, &base, &branch))
        .expect("create tune checkout");
    for (path, body) in edits {
        let file = repo.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).expect("mkdir proposal edit parent");
        }
        std::fs::write(file, body).expect("write proposal edit");
    }
    commit_all_in(&repo, "tune proposal").expect("commit proposal");
    let head = runtime
        .block_on(
            GitClient::open(&repo)
                .expect("open proposal git")
                .head_commit_sha(),
        )
        .expect("proposal head")
        .to_string();
    std::fs::write(
        env.workspace
            .join(".loom/tune")
            .join(id)
            .join("manifest.json"),
        serde_json::json!({"proposal_id": id}).to_string(),
    )
    .expect("write manifest");
    seed_bead(
        &env.state_dir,
        id,
        "accepted tune",
        "Tune body",
        "open",
        &["loom:tune", "spec:skills"],
    );
    seed_metadata(
        &env.state_dir,
        id,
        serde_json::json!({
            "loom.tune.id": id,
            "loom.tune.state": "pending",
            "loom.tune.base_commit": base,
            "loom.tune.proposal_branch": branch,
            "loom.tune.proposal_head": head,
        }),
    );
}

fn apply_marker(ids: &[&str]) -> String {
    let payload = serde_json::json!({"proposals": ids});
    format!("LOOM_APPLY: {payload}")
}

fn init_apply_repo(env: &ChatRun) {
    init_test_repo_with_integration(&env.workspace).expect("init git repo with integration");
    std::fs::write(
        env.workspace.join(".git/info/exclude"),
        "/bd-state/\n/bd-bin/\n/wrix-bin/\n/apply-loom-bin/\n/profile-images.json\n/base.tar\n/argv.log\n/env.log\n/apply-loom.log\n",
    )
    .expect("write git exclude");
}

fn assert_apply_failed_kind(env: &ChatRun, id: &str, kind: &str) {
    let metadata = read_metadata(&env.state_dir, id);
    assert_eq!(metadata["loom.tune.state"], "apply_failed");
    assert_eq!(metadata["loom.tune.apply_failure"]["kind"], kind);
    assert_eq!(read_field(&env.state_dir, id, "status"), "blocked");
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
fn inbox_apply_marker_triggers_single_driver_handoff() {
    let env = setup_chat();
    init_apply_repo(&env);
    let loom_stub = install_apply_loom_stub(&env.workspace);
    create_tune_proposal(&env, "lm-app1", &[("proposal.txt", "accepted\n")]);
    let marker = apply_marker(&["lm-app1"]);
    let loom_stub_path = loom_stub.to_string_lossy().into_owned();

    let output = run_chat_extra(
        &env,
        "accept-tune",
        &["-p", "lm-app1"],
        &[
            ("WRIX_STUB_MARKER", marker.as_str()),
            ("LOOM_INBOX_APPLY_LOOM_BIN", loom_stub_path.as_str()),
        ],
    );
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let metadata = read_metadata(&env.state_dir, "lm-app1");
    assert_eq!(metadata["loom.tune.state"], "applied");
    assert_eq!(read_field(&env.state_dir, "lm-app1", "status"), "closed");
    let gate_log =
        std::fs::read_to_string(env.workspace.join("apply-loom.log")).expect("apply loom log");
    assert_eq!(
        gate_log.matches("gate verify --diff").count(),
        1,
        "{gate_log}"
    );
    assert_eq!(
        gate_log.matches("gate review --diff").count(),
        1,
        "{gate_log}"
    );
    let integration_head =
        sync_head_commit_sha(&env.workspace.join(".loom/integration")).expect("integration head");
    let origin_head =
        sync_rev_parse(&bare_origin_path(&env.workspace), "main").expect("origin head");
    assert_eq!(integration_head, origin_head);
}

#[test]
fn inbox_apply_batch_is_all_or_nothing() {
    let env = setup_chat();
    init_apply_repo(&env);
    let loom_stub = install_apply_loom_stub(&env.workspace);
    create_tune_proposal(&env, "lm-app2", &[("a.txt", "a\n")]);
    create_tune_proposal(&env, "lm-app3", &[("b.txt", "b\n")]);
    let integration = env.workspace.join(".loom/integration");
    let pre_head = sync_head_commit_sha(&integration).expect("pre integration head");
    let origin_pre = sync_rev_parse(&bare_origin_path(&env.workspace), "main").expect("origin pre");
    let marker = apply_marker(&["lm-app2", "lm-app3"]);
    let loom_stub_path = loom_stub.to_string_lossy().into_owned();

    let output = run_chat_extra(
        &env,
        "accept-tune",
        &[],
        &[
            ("WRIX_STUB_MARKER", marker.as_str()),
            ("LOOM_INBOX_APPLY_LOOM_BIN", loom_stub_path.as_str()),
            ("LOOM_APPLY_STUB_VERIFY", "fail"),
        ],
    );
    assert!(!output.status.success(), "verify failure must fail apply");
    for id in ["lm-app2", "lm-app3"] {
        let metadata = read_metadata(&env.state_dir, id);
        assert_eq!(metadata["loom.tune.state"], "apply_failed");
        assert_eq!(metadata["loom.tune.apply_failure"]["kind"], "verify_failed");
        assert_eq!(read_field(&env.state_dir, id, "status"), "blocked");
        let labels = read_labels(&env.state_dir, id);
        assert!(
            labels.iter().any(|label| label == "loom:blocked"),
            "{labels:?}"
        );
    }
    assert_eq!(
        sync_head_commit_sha(&integration).expect("post integration head"),
        pre_head
    );
    assert_eq!(
        sync_rev_parse(&bare_origin_path(&env.workspace), "main").expect("origin post"),
        origin_pre,
    );
    assert_eq!(
        status_porcelain_sync(&integration).expect("integration status"),
        "",
    );
    let gate_log =
        std::fs::read_to_string(env.workspace.join("apply-loom.log")).expect("apply loom log");
    assert_eq!(
        gate_log.matches("gate verify --diff").count(),
        1,
        "{gate_log}"
    );
    assert_eq!(
        gate_log.matches("gate review --diff").count(),
        0,
        "{gate_log}"
    );
}

#[test]
fn inbox_apply_cherry_pick_conflict_aborts_batch() {
    let env = setup_chat();
    init_apply_repo(&env);
    let loom_stub = install_apply_loom_stub(&env.workspace);
    create_tune_proposal(&env, "lm-app5", &[("README.md", "proposal\n")]);
    let integration = env.workspace.join(".loom/integration");
    std::fs::write(integration.join("README.md"), "integration\n").expect("edit integration");
    commit_all_in(&integration, "integration edit").expect("commit integration edit");
    let pre_head = sync_head_commit_sha(&integration).expect("pre integration head");
    let marker = apply_marker(&["lm-app5"]);
    let loom_stub_path = loom_stub.to_string_lossy().into_owned();

    let output = run_chat_extra(
        &env,
        "accept-tune",
        &[],
        &[
            ("WRIX_STUB_MARKER", marker.as_str()),
            ("LOOM_INBOX_APPLY_LOOM_BIN", loom_stub_path.as_str()),
        ],
    );
    assert!(!output.status.success(), "conflict must fail apply");
    assert_apply_failed_kind(&env, "lm-app5", "cherry_pick_conflict");
    assert_eq!(
        sync_head_commit_sha(&integration).expect("post integration head"),
        pre_head
    );
    assert_eq!(
        status_porcelain_sync(&integration).expect("integration status"),
        "",
    );
}

#[test]
fn inbox_apply_review_failure_aborts_batch() {
    let env = setup_chat();
    init_apply_repo(&env);
    let loom_stub = install_apply_loom_stub(&env.workspace);
    create_tune_proposal(&env, "lm-app6", &[("review.txt", "candidate\n")]);
    let integration = env.workspace.join(".loom/integration");
    let pre_head = sync_head_commit_sha(&integration).expect("pre integration head");
    let marker = apply_marker(&["lm-app6"]);
    let loom_stub_path = loom_stub.to_string_lossy().into_owned();

    let output = run_chat_extra(
        &env,
        "accept-tune",
        &[],
        &[
            ("WRIX_STUB_MARKER", marker.as_str()),
            ("LOOM_INBOX_APPLY_LOOM_BIN", loom_stub_path.as_str()),
            ("LOOM_APPLY_STUB_REVIEW", "concern"),
        ],
    );
    assert!(!output.status.success(), "review concern must fail apply");
    assert_apply_failed_kind(&env, "lm-app6", "review_failed");
    assert_eq!(
        sync_head_commit_sha(&integration).expect("post integration head"),
        pre_head
    );
    assert_eq!(
        status_porcelain_sync(&integration).expect("integration status"),
        "",
    );
}

#[test]
fn inbox_apply_push_failure_aborts_batch() {
    let env = setup_chat();
    init_apply_repo(&env);
    let loom_stub = install_apply_loom_stub(&env.workspace);
    create_tune_proposal(&env, "lm-app7", &[("push.txt", "candidate\n")]);
    let integration = env.workspace.join(".loom/integration");
    let pre_head = sync_head_commit_sha(&integration).expect("pre integration head");
    let origin_pre = sync_rev_parse(&bare_origin_path(&env.workspace), "main").expect("origin pre");
    let remote = Command::new("git")
        .arg("-C")
        .arg(&integration)
        .args(["remote", "set-url", "origin", "/definitely/missing/origin"])
        .status()
        .expect("set invalid origin");
    assert!(remote.success(), "remote set-url failed: {remote}");
    let marker = apply_marker(&["lm-app7"]);
    let loom_stub_path = loom_stub.to_string_lossy().into_owned();

    let output = run_chat_extra(
        &env,
        "accept-tune",
        &[],
        &[
            ("WRIX_STUB_MARKER", marker.as_str()),
            ("LOOM_INBOX_APPLY_LOOM_BIN", loom_stub_path.as_str()),
        ],
    );
    assert!(!output.status.success(), "push failure must fail apply");
    assert_apply_failed_kind(&env, "lm-app7", "push_failed");
    assert_eq!(
        sync_head_commit_sha(&integration).expect("post integration head"),
        pre_head
    );
    assert_eq!(
        sync_rev_parse(&bare_origin_path(&env.workspace), "main").expect("origin post"),
        origin_pre,
    );
    assert_eq!(
        status_porcelain_sync(&integration).expect("integration status"),
        "",
    );
}

#[test]
fn apply_failed_tune_proposals_require_reauthorization() {
    let env = setup_chat();
    init_apply_repo(&env);
    let loom_stub = install_apply_loom_stub(&env.workspace);
    create_tune_proposal(&env, "lm-app4", &[("reauth.txt", "candidate\n")]);
    seed_metadata(
        &env.state_dir,
        "lm-app4",
        serde_json::json!({
            "loom.tune.id": "lm-app4",
            "loom.tune.state": "apply_failed",
            "loom.tune.proposal_branch": "loom/tune/lm-app4",
            "loom.tune.proposal_head": read_metadata(&env.state_dir, "lm-app4")["loom.tune.proposal_head"].clone(),
        }),
    );
    std::fs::write(env.state_dir.join("lm-app4").join("status"), "blocked").expect("set blocked");
    let marker = apply_marker(&["lm-app4"]);
    let loom_stub_path = loom_stub.to_string_lossy().into_owned();

    let output = run_chat_extra(
        &env,
        "resolve-none",
        &["-p", "lm-app4"],
        &[
            ("WRIX_STUB_MARKER", marker.as_str()),
            ("LOOM_INBOX_APPLY_LOOM_BIN", loom_stub_path.as_str()),
        ],
    );
    assert!(
        !output.status.success(),
        "apply_failed requires accepted reauthorization"
    );
    let metadata = read_metadata(&env.state_dir, "lm-app4");
    assert_eq!(metadata["loom.tune.state"], "apply_failed");
    assert!(
        !env.workspace.join("apply-loom.log").exists(),
        "gates must not run"
    );
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

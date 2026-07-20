//! End-to-end smoke tests for `loom loop`.
//!
//! These tests pin CLI-level dispatch surfaces that need the compiled binary
//! plus a stub `bd` on PATH.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

/// Initialize a real git repo at `path` plus the loom-owned integration
/// workspace at `.loom/integration/` so `loom loop`'s per-bead
/// worktree dispatch and the post-merge push gate both succeed.
fn init_workspace_repo(path: &Path) {
    loom_driver::git::init_test_repo_with_integration(path)
        .expect("init test repo with loom integration");
}

/// Write a stub `bd` that appends each invocation's full argv to
/// `argv_log` and exposes one active work epic with an empty ready queue.
/// Used to inspect the exact flags `loom`'s bd client emits.
fn install_bd_argv_logger(dir: &Path, argv_log: &Path) -> std::path::PathBuf {
    let bin_dir = dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let bd = bin_dir.join("bd");
    let active = r#"[{"id":"lm-active","title":"active","status":"open","priority":2,"issue_type":"epic","labels":["loom:active","spec:harness"],"metadata":{}}]"#;
    let script = format!(
        r#"#!/bin/sh
{{ for a in "$@"; do printf '%s\t' "$a"; done; printf '\n'; }} >> {log}
cmd="${{1:-}}"
has_json=0
label_active=0
for arg in "$@"; do
  case "$arg" in
    --json) has_json=1 ;;
    --label=loom:active) label_active=1 ;;
  esac
done
if [ "$cmd" = "list" ] && [ "$has_json" = "1" ] && [ "$label_active" = "1" ]; then
  printf '%s' '{active}'
  exit 0
fi
if [ "$has_json" = "1" ]; then
  printf '%s' '[]'
  exit 0
fi
exit 0
"#,
        log = argv_log.display(),
        active = active,
    );
    std::fs::write(&bd, script).unwrap();
    let mut perm = std::fs::metadata(&bd).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&bd, perm).unwrap();
    bin_dir
}

/// Write a stub `bd` that exposes one active work epic labelled
/// `loom:active,spec:harness` while returning an empty ready queue. This
/// lets a multi-spec workspace exercise bare `loom loop`'s active-epic
/// default without dispatching a real bead.
fn install_bd_active_epic_stub(dir: &Path, argv_log: &Path) -> std::path::PathBuf {
    let bin_dir = dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let bd = bin_dir.join("bd");
    let active = r#"[{"id":"lm-active","title":"active","status":"open","priority":2,"issue_type":"epic","labels":["loom:active","spec:harness"],"metadata":{}}]"#;
    let script = format!(
        r#"#!/bin/sh
for a in "$@"; do printf '%s\t' "$a"; done >> {log}
printf '\n' >> {log}

cmd="${{1:-}}"
has_json=0
label_active=0
label_harness=0
type_epic=0
for arg in "$@"; do
  case "$arg" in
    --json) has_json=1 ;;
    --label=loom:active) label_active=1 ;;
    --label=spec:harness) label_harness=1 ;;
    --type=epic) type_epic=1 ;;
  esac
done

if [ "$cmd" = "list" ] && [ "$has_json" = "1" ]; then
  if [ "$label_active" = "1" ]; then
    printf '%s' '{active}'
    exit 0
  fi
  if [ "$label_harness" = "1" ] && [ "$type_epic" = "1" ]; then
    printf '%s' '{active}'
    exit 0
  fi
  printf '%s' '[]'
  exit 0
fi

if [ "$has_json" = "1" ]; then
  printf '%s' '[]'
  exit 0
fi
exit 0
"#,
        log = argv_log.display(),
        active = active,
    );
    std::fs::write(&bd, script).unwrap();
    let mut perm = std::fs::metadata(&bd).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&bd, perm).unwrap();
    bin_dir
}

fn install_bd_parallel_infra_stub(dir: &Path, argv_log: &Path) -> std::path::PathBuf {
    let bin_dir = dir.join("parallel-infra-bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let bd = bin_dir.join("bd");
    let active = r#"[{"id":"lm-active","title":"active","status":"open","priority":2,"issue_type":"epic","labels":["loom:active","spec:agent"],"metadata":{}}]"#;
    let ready = r#"[{"id":"lm-miss","title":"bad profile","description":"","status":"open","priority":2,"issue_type":"task","labels":["spec:agent","profile:base"],"metadata":{}}]"#;
    let script = format!(
        r#"#!/bin/sh
for a in "$@"; do printf '%s\t' "$a"; done >> {log}
printf '\n' >> {log}

cmd="${{1:-}}"
has_json=0
label_active=0
for arg in "$@"; do
  case "$arg" in
    --json) has_json=1 ;;
    --label=loom:active) label_active=1 ;;
  esac
done

if [ "$cmd" = "list" ] && [ "$has_json" = "1" ] && [ "$label_active" = "1" ]; then
  printf '%s' '{active}'
  exit 0
fi
if [ "$cmd" = "ready" ] && [ "$has_json" = "1" ]; then
  printf '%s' '{ready}'
  exit 0
fi
if [ "$cmd" = "update" ]; then
  exit 0
fi
if [ "$has_json" = "1" ]; then
  printf '%s' '[]'
  exit 0
fi
exit 0
"#,
        log = argv_log.display(),
        active = active,
        ready = ready,
    );
    std::fs::write(&bd, script).unwrap();
    let mut perm = std::fs::metadata(&bd).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&bd, perm).unwrap();
    bin_dir
}

#[test]
fn loom_loop_removed_selectors_are_rejected() {
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    for args in [
        vec!["loop", "--once"],
        vec!["loop", "--spec", "harness"],
        vec!["loop", "--all-specs"],
    ] {
        let output = Command::new(loom_bin)
            .args(&args)
            .env_remove("LOOM_INSIDE")
            .output()
            .expect("spawn loom");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "removed selector must fail for {args:?}. stdout={stdout} stderr={stderr}",
        );
        assert!(
            stderr.contains("unexpected argument"),
            "clap must reject removed selector before loop dispatch for {args:?}. stderr={stderr}",
        );
    }
}

#[test]
fn all_non_loop_wrix_launch_surfaces_preflight_repository_policy() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    init_workspace_repo(workspace);

    let deploy_key = workspace.join("repo-key");
    let signing_key = workspace.join("repo-key-signing");
    std::fs::write(&deploy_key, "deploy").unwrap();
    std::fs::write(&signing_key, "signing").unwrap();
    let log = workspace.join("policy.log");
    let wrix = workspace.join("wrix-policy");
    std::fs::write(
        &wrix,
        loom_test_support::bash_script(&format!(
            r#"set -euo pipefail
printf '%s\n' "$*" >> {log:?}
key_name=""
prev=""
for arg in "$@"; do
    if [[ "$prev" == "--key" ]]; then key_name="$arg"; break; fi
    prev="$arg"
done
git config --local gpg.format ssh
git config --local gpg.ssh.program wrix-git-sign
git config --local gpg.ssh.allowedSignersFile wrix/allowed_signers
git config --local user.signingkey "wrix/signing-key/${{key_name}}-signing"
git config --local commit.gpgsign true
git config --local core.sshCommand wrix/git-ssh
mkdir -p .git/wrix
printf allowed > .git/wrix/allowed_signers
printf '#!/bin/sh\n' > .git/wrix/git-ssh
chmod +x .git/wrix/git-ssh
"#,
            log = log,
        )),
    )
    .unwrap();
    std::fs::set_permissions(&wrix, std::fs::Permissions::from_mode(0o755)).unwrap();
    let missing_manifest = workspace.join("missing-profile-images.json");
    let commands: &[&[&str]] = &[
        &["plan"],
        &["todo"],
        &["gate", "review", "--tree"],
        &["gate", "mint", "--tree"],
        &["inbox", "chat"],
        &["tune", "skill", "fast", "test-target"],
    ];

    for args in commands {
        let output = Command::new(env!("CARGO_BIN_EXE_loom"))
            .arg("--workspace")
            .arg(workspace)
            .args(*args)
            .env("LOOM_WRIX_BIN", &wrix)
            .env("WRIX_DEPLOY_KEY", &deploy_key)
            .env("WRIX_SIGNING_KEY", &signing_key)
            .env("LOOM_PROFILES_MANIFEST", &missing_manifest)
            .env_remove("GIT_SSH_COMMAND")
            .env_remove("GIT_SSH")
            .env_remove("LOOM_INSIDE")
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "fixture must stop after policy preflight for {args:?}",
        );
    }

    let invocations = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        invocations.lines().count(),
        commands.len(),
        "every Wrix-bearing command must initialize repository policy: {invocations}",
    );
    assert!(
        invocations
            .lines()
            .all(|line| line == "init --offline --no-hooks --key repo-key"),
        "unexpected policy invocation: {invocations}",
    );
}

#[test]
fn loom_loop_missing_repository_keys_fails_before_bead_selection() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    init_workspace_repo(workspace);

    let manifest_path = workspace.join("profile-images.json");
    std::fs::write(&manifest_path, "{}").unwrap();
    let bin_dir = workspace.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let bd_marker = workspace.join("bd-invoked");
    let bd = bin_dir.join("bd");
    std::fs::write(
        &bd,
        format!(
            "#!/bin/sh\nprintf invoked > {}\nexit 99\n",
            bd_marker.display()
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&bd).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&bd, permissions).unwrap();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = vec![bin_dir];
    path_entries.extend(std::env::split_paths(&path));

    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .arg("--workspace")
        .arg(workspace)
        .arg("loop")
        .env("PATH", std::env::join_paths(path_entries).unwrap())
        .env("HOME", workspace.join("empty-home"))
        .env("LOOM_PROFILES_MANIFEST", &manifest_path)
        .env_remove("LOOM_INSIDE")
        .env_remove("GIT_SSH_COMMAND")
        .env_remove("GIT_SSH")
        .env_remove("WRIX_DEPLOY_KEY")
        .env_remove("WRIX_SIGNING_KEY")
        .output()
        .expect("spawn loom");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "missing keys must fail: {stderr}");
    assert!(
        stderr.contains("repository deploy key is unavailable"),
        "startup error must name the missing repository key: {stderr}",
    );
    assert!(
        stderr.contains("--host-key"),
        "startup error must name the explicit opt-in: {stderr}",
    );
    assert!(
        !bd_marker.exists(),
        "repository-key preflight must run before bead selection",
    );
}

#[test]
fn loom_loop_startup_initializes_repository_git_policy() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    init_workspace_repo(workspace);

    let manifest_path = workspace.join("profile-images.json");
    std::fs::write(&manifest_path, "{}").unwrap();
    let argv_log = workspace.join("bd-argv.log");
    let bin_dir = install_bd_active_epic_stub(workspace, &argv_log);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = vec![bin_dir];
    path_entries.extend(std::env::split_paths(&path));

    let deploy_key = workspace.join("repo-key");
    let signing_key = workspace.join("repo-key-signing");
    std::fs::write(&deploy_key, "deploy").unwrap();
    std::fs::write(&signing_key, "signing").unwrap();
    let wrix_log = workspace.join("wrix-init.log");
    let wrix = workspace.join("wrix");
    std::fs::write(
        &wrix,
        format!(
            r#"#!/bin/sh
set -eu
printf 'cwd=%s\nargs=%s\ndeploy=%s\nsigning=%s\n' "$PWD" "$*" "$WRIX_DEPLOY_KEY" "$WRIX_SIGNING_KEY" > {}
git config --local gpg.format ssh
git config --local gpg.ssh.program wrix-git-sign
git config --local gpg.ssh.allowedSignersFile wrix/allowed_signers
git config --local user.signingkey wrix/signing-key/repo-key-signing
git config --local commit.gpgsign true
git config --local core.sshCommand wrix/git-ssh
mkdir -p .git/wrix
printf allowed > .git/wrix/allowed_signers
printf '#!/bin/sh\n' > .git/wrix/git-ssh
chmod +x .git/wrix/git-ssh
"#,
            wrix_log.display(),
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&wrix).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&wrix, permissions).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .arg("--workspace")
        .arg(workspace)
        .arg("loop")
        .env("PATH", std::env::join_paths(path_entries).unwrap())
        .env("LOOM_PROFILES_MANIFEST", &manifest_path)
        .env("LOOM_WRIX_BIN", &wrix)
        .env("WRIX_DEPLOY_KEY", &deploy_key)
        .env("WRIX_SIGNING_KEY", &signing_key)
        .env("XDG_STATE_HOME", workspace.join(".loom-test-state"))
        .env_remove("LOOM_INSIDE")
        .env_remove("GIT_SSH_COMMAND")
        .env_remove("GIT_SSH")
        .output()
        .expect("spawn loom");

    let log = std::fs::read_to_string(&wrix_log).unwrap_or_else(|_| {
        panic!(
            "repository-key startup did not invoke wrix init: {}",
            String::from_utf8_lossy(&output.stderr),
        )
    });
    assert!(
        log.contains(&format!(
            "cwd={}",
            workspace.join(".loom/integration").display()
        )),
        "wrix init must target the integration clone: {log}",
    );
    assert!(
        log.contains("args=init --offline --no-hooks --key repo-key"),
        "unexpected wrix init argv: {log}",
    );
    assert!(log.contains(&format!("deploy={}", deploy_key.display())));
    assert!(log.contains(&format!("signing={}", signing_key.display())));
}

#[test]
fn loom_loop_without_spec_uses_active_epic_in_multi_spec_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    init_workspace_repo(workspace);
    std::fs::create_dir_all(workspace.join(".loom")).unwrap();
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(workspace.join("specs/harness.md"), "# harness\n").unwrap();
    std::fs::write(workspace.join("specs/gate.md"), "# gate\n").unwrap();

    let argv_log = workspace.join("bd-argv.log");
    let bin_dir = install_bd_active_epic_stub(workspace, &argv_log);
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
        .arg("loop")
        .arg("--host-key")
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
        "bare loom loop must resolve the active epic instead of failing multi-spec resolution. \
         stdout={stdout} stderr={stderr}",
    );
    assert!(
        stdout.contains("gate=no-gate"),
        "empty active work epic should produce no-gate summary. stdout={stdout}",
    );

    let log = std::fs::read_to_string(&argv_log)
        .unwrap_or_else(|_| panic!("bd-argv log {} must exist", argv_log.display()));
    assert!(
        log.lines()
            .any(|line| line.contains("list\t") && line.contains("--label=loom:active")),
        "bare loop must query the active work epic rather than tree spec resolution:\n{log}",
    );
    let ready_line = log
        .lines()
        .find(|line| line.contains("ready\t"))
        .unwrap_or_else(|| panic!("no `bd ready` call recorded in log:\n{log}"));
    assert!(
        ready_line.contains("--parent=lm-active"),
        "ready queue must scope to the active work epic parent:\n{log}",
    );
    assert!(
        !ready_line.contains("--label=spec:harness"),
        "ready queue for a multi-spec active work epic must not narrow by spec:\n{log}",
    );
}

/// FR1: the `--parallel N` path of `loom loop` must call `bd ready`
/// WITHOUT `--exclude-label=loom:clarify` / `--exclude-label=loom:blocked`.
/// Dedup of clarify/blocked beads relies on the paired `status=blocked`
/// transition the apply paths write alongside the label; `bd ready`
/// natively excludes status=blocked. The historical exclude-label flags
/// papered over a bd `--exclude-label` regression where the filter was
/// silently dropped, causing every loop iteration to re-dispatch the same
/// labelled bead.
#[test]
fn loom_loop_parallel_does_not_pass_exclude_label_to_bd_ready() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    init_workspace_repo(workspace);
    std::fs::create_dir_all(workspace.join(".loom")).unwrap();
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(workspace.join("specs/harness.md"), "# harness\n").unwrap();

    let db = loom_driver::state::CacheDb::open(workspace.join(".loom/cache.db")).unwrap();
    db.upsert_spec(
        &loom_driver::identifier::SpecLabel::new("harness"),
        "specs/harness.md",
    )
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
        .arg("loop")
        .arg("--host-key")
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
        "loom loop --parallel 2 must exit zero against an empty bd queue. \
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
        !argv.iter().any(|a| a.starts_with("--exclude-label")),
        "parallel `bd ready` must NOT pass --exclude-label — dedup happens via \
         the paired status=blocked transition; argv={argv:?}",
    );
}

/// Spec criterion (`specs/harness.md` § Loop Outcome Types): the parallel
/// codepath returns the same `LoopOutcome` shape as the sequential one,
/// with a `gate` field that the binary's exit-code mapping consumes. The
/// summary line printed by `loom loop --parallel N` includes a `gate=...`
/// column whenever the parallel path returns a real `LoopOutcome`; the
/// absence of that column would mean the parallel path is still returning
/// the old `ParallelLoopSummary` shape (no gate).
#[test]
fn parallel_codepath_returns_loop_outcome_with_gate_field() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    init_workspace_repo(workspace);
    std::fs::create_dir_all(workspace.join(".loom")).unwrap();
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(workspace.join("specs/harness.md"), "# harness\n").unwrap();

    let db = loom_driver::state::CacheDb::open(workspace.join(".loom/cache.db")).unwrap();
    db.upsert_spec(
        &loom_driver::identifier::SpecLabel::new("harness"),
        "specs/harness.md",
    )
    .unwrap();
    drop(db);

    let argv_log = workspace.join("bd-argv.log");
    let bin_dir = install_bd_active_epic_stub(workspace, &argv_log);
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
        .arg("loop")
        .arg("--host-key")
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
        "loom loop --parallel 2 must exit zero. stdout={stdout} stderr={stderr}",
    );
    assert!(
        stdout.contains("gate="),
        "parallel summary must include the `gate=` column proving LoopOutcome \
         is the return shape (not the old ParallelLoopSummary). stdout={stdout}",
    );
    assert!(
        stdout.contains("gate=no-gate"),
        "empty bd queue under --parallel must produce GateOutcome::NoGate. stdout={stdout}",
    );
}

#[test]
fn loom_loop_parallel_static_infra_parks_as_loom_infra() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    init_workspace_repo(workspace);
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(workspace.join("specs/agent.md"), "# agent\n").unwrap();

    let argv_log = workspace.join("bd-argv.log");
    let bin_dir = install_bd_parallel_infra_stub(workspace, &argv_log);
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
        .arg("--agent")
        .arg("pi")
        .arg("loop")
        .arg("--host-key")
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
        "parallel static infra should park the bead and exit zero. stdout={stdout} stderr={stderr}",
    );
    let log = std::fs::read_to_string(&argv_log)
        .unwrap_or_else(|_| panic!("bd argv log {} must exist", argv_log.display()));
    let update = log
        .lines()
        .find(|line| line.starts_with("update\tlm-miss\t"))
        .unwrap_or_else(|| panic!("no update call for infra bead:\n{log}"));
    let argv = update.split('\t').collect::<Vec<_>>();
    assert!(argv.windows(2).any(|w| w == ["--status", "blocked"]));
    assert!(
        argv.windows(2).any(|w| w == ["--add-label", "loom:infra"]),
        "update argv must add loom:infra: {argv:?}",
    );
    assert!(
        argv.iter()
            .any(|arg| arg == &"loom.infra.cause=unknown-profile"),
        "update argv must persist infra cause metadata: {argv:?}",
    );
    assert!(
        !log.contains("loom:blocked"),
        "static infra must not apply semantic loom:blocked: {log}",
    );
}

#[test]
fn loom_loop_recognizes_subcommand() {
    // Regression guard: `loom loop --help` must exit cleanly. A binary
    // that does not expose `run` prints "unrecognized subcommand: run".
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .arg("loop")
        .arg("--help")
        .output()
        .expect("spawn loom");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom loop --help must exit zero. stdout={stdout} stderr={stderr}",
    );
    assert!(
        stdout.contains("BEAD_OR_EPIC_ID")
            && stdout.contains("--parallel")
            && !stdout.contains("--once"),
        "loom loop --help must document work roots and omit removed selectors. stdout={stdout}",
    );
}

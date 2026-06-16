//! Cross-cutting integration test: host -> wrix spawn -> agent dispatch.
//!
//! Verifies the contract loom owes the wrix wrapper:
//!
//! 1. `wrix spawn --spawn-config <file> --stdio` is the only argv shape
//!    loom hands to the wrapper. `<file>` resolves to a JSON-serialized
//!    [`SpawnConfig`] containing the resolved profile image.
//! 2. The container child receives stdin via a pipe (not a TTY) so JSONL
//!    framing flows correctly and EOF semantics work when loom closes its
//!    end of the pipe.
//!
//! Both tests drive `loom --agent pi todo` through a wrix shim that
//! records what the loom binary actually exec'd. The shim then hands the
//! exchange off to the existing `mock-pi.sh` so the pi backend's startup
//! probe + prompt round-trip completes naturally — without that, the loom
//! binary would hang waiting for `agent_end` and the test would never see
//! the recorded argv.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

/// Resolve the absolute path to `bash` from `PATH`. Used so the shim's
/// shebang points at a concrete interpreter rather than `/usr/bin/env`,
/// which is not present in the default nix-build sandbox (`sandbox = true`).
fn find_bash() -> PathBuf {
    let path_var = std::env::var_os("PATH").expect("PATH must be set");
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("bash");
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!("bash not found in PATH");
}

/// Write the wrix shim into `dir` and return its path. The shim records
/// argv (one quoted token per line) and stdin TTY/pipe state into the two
/// sibling files, copies the `--spawn-config` JSON aside (so the test can
/// inspect it without racing the temp-file delete), then exec's mock-pi in
/// `happy-path` mode so the pi backend handshake AND the prompt round-trip
/// complete; otherwise the loom binary would hang waiting for `agent_end`.
fn install_wrix_shim(
    dir: &Path,
    argv_file: &Path,
    stdin_info: &Path,
    spawn_config_copy: &Path,
    mock_agent: &Path,
    mock_agent_mode: &str,
) -> PathBuf {
    let shim = dir.join("wrix");
    let bash = find_bash();
    let body = format!(
        "#!{bash}\n\
         set -euo pipefail\n\
         ARGV_FILE='{argv}'\n\
         STDIN_INFO='{stdin}'\n\
         SPAWN_CONFIG_COPY='{copy}'\n\
         MOCK_AGENT='{mock}'\n\
         MOCK_AGENT_MODE='{mode}'\n\
         \n\
         {{ for a in \"$@\"; do printf '%s\\n' \"$a\"; done; }} > \"$ARGV_FILE\"\n\
         \n\
         {{ if [ -t 0 ]; then echo 'stdin_is_tty=1'; else echo 'stdin_is_tty=0'; fi\n\
            if [ -p /dev/stdin ]; then echo 'stdin_is_pipe=1'; else echo 'stdin_is_pipe=0'; fi\n\
         }} > \"$STDIN_INFO\"\n\
         \n\
         prev=''\n\
         for a in \"$@\"; do\n\
             if [ \"$prev\" = '--spawn-config' ]; then\n\
                 cp \"$a\" \"$SPAWN_CONFIG_COPY\"\n\
                 break\n\
             fi\n\
             prev=\"$a\"\n\
         done\n\
         \n\
         echo '[wrix] Starting container (mock)...' >&2\n\
         exec '{bash}' \"$MOCK_AGENT\" \"$MOCK_AGENT_MODE\"\n",
        bash = bash.display(),
        argv = argv_file.display(),
        stdin = stdin_info.display(),
        copy = spawn_config_copy.display(),
        mock = mock_agent.display(),
        mode = mock_agent_mode,
    );
    std::fs::write(&shim, body).unwrap();
    let mut perm = std::fs::metadata(&shim).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&shim, perm).unwrap();
    shim
}

/// Locate a shim script under `tests/<rel>` by walking ancestors of the
/// crate manifest dir. Two layouts are supported transparently:
///   - dev tree: `repo/crates/loom/` is the manifest dir, mock scripts
///     live under `repo/tests/`.
///   - nix sandbox (crane buildPackage): the loom workspace IS the staged
///     root and mock scripts live next to it under `<staged>/tests/`.
fn locate_mock(rel: &str) -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    for ancestor in manifest_dir.ancestors() {
        let candidate = ancestor.join("tests").join(rel);
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!(
        "could not locate tests/{rel} above {} — neither dev-tree nor \
         nix-sandbox layout matched.",
        manifest_dir.display(),
    );
}

/// Locate `tests/mock-pi/pi.sh` relative to the loom-binary crate.
fn mock_pi_path() -> PathBuf {
    locate_mock("mock-pi/pi.sh")
}

/// Locate `tests/mock-claude/claude.sh`. Used by the shutdown-watchdog
/// gate to drive `loom todo --agent claude` end-to-end against a mock that
/// emits stream-json then ignores SIGTERM/stdin close.
fn mock_claude_path() -> PathBuf {
    locate_mock("mock-claude/claude.sh")
}

/// Run `loom --workspace <ws> --agent pi todo` against a shim wrix and
/// return the captured `Output`. The active spec is set via `loom use`
/// before dispatch (the `--spec` override was removed from `Command::Todo`
/// per `specs/harness.md` *Removed surface*). Shared by both tests so the
/// assertions stay focused on what they verify.
fn drive_loom_todo_pi(workspace: &Path, shim: &Path, loom_bin: &str) -> std::process::Output {
    // Spawn-bound subcommands (`todo` is one) read LOOM_PROFILES_MANIFEST at
    // startup. The production todo controller resolves the `base` profile
    // through this manifest, so it must contain a real entry — an empty
    // `{}` would surface as ProfileError::UnknownProfile.
    let manifest_path = workspace.join("profile-images.json");
    let image_source = workspace.join("base.tar");
    std::fs::write(&image_source, "").expect("write stub image source");
    let manifest_body = format!(
        r#"{{
          "base": {{ "pi": {{ "ref": "localhost/wrix-base-pi:test", "source": {source:?} }}, "claude": {{ "ref": "localhost/wrix-base-claude:test", "source": {source:?} }}, "direct": {{ "ref": "localhost/wrix-base-direct:test", "source": {source:?} }} }}
        }}"#,
        source = image_source.display().to_string(),
    );
    std::fs::write(&manifest_path, manifest_body).expect("write manifest stub");
    init_workspace_repo(workspace);
    seed_active_spec(workspace, loom_bin, "agent");
    let new_path = bd_stub_path(workspace, "[]");
    Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .arg("--agent")
        .arg("pi")
        .arg("todo")
        .env("PATH", new_path)
        .env("LOOM_WRIX_BIN", shim)
        .env("LOOM_BIN", loom_bin)
        .env("LOOM_PROFILES_MANIFEST", &manifest_path)
        .env("XDG_STATE_HOME", workspace.join(".loom-test-state"))
        // Bypass the nested-loom guard so cargo test inside a loom container
        // still reaches the todo dispatch path under test.
        .env_remove("LOOM_INSIDE")
        .output()
        .expect("spawn loom")
}

fn enable_workspace_sccache(workspace: &Path) -> PathBuf {
    let host_cache = workspace.join(".loom/sccache");
    std::fs::write(
        workspace.join("loom.toml"),
        "[loom]\nsccache_dir = \".loom/sccache\"\n",
    )
    .expect("write loom config");
    host_cache
}

fn assert_spawn_config_uses_sccache(spawn_copy: &Path, host_cache: &Path) {
    let bytes = std::fs::read(spawn_copy).expect("shim should copy spawn-config aside");
    let cfg: loom_driver::agent::SpawnConfig =
        serde_json::from_slice(&bytes).expect("spawn-config must deserialize");
    let mount = cfg
        .mounts
        .iter()
        .find(|mount| mount.container_path.as_path() == Path::new("/sccache"))
        .unwrap_or_else(|| panic!("spawn config missing /sccache mount: {:?}", cfg.mounts));
    assert_eq!(mount.host_path, host_cache);
    assert!(
        host_cache.is_dir(),
        "host sccache dir must exist before spawn"
    );
    assert!(!mount.read_only, "sccache mount must be writable");
    assert!(
        cfg.env
            .iter()
            .any(|(key, value)| key == "SCCACHE_DIR" && value == "/sccache"),
        "spawn env missing SCCACHE_DIR=/sccache: {:?}",
        cfg.env,
    );
    assert!(
        cfg.env
            .iter()
            .any(|(key, value)| key == "RUSTC_WRAPPER" && value == "sccache"),
        "spawn env missing RUSTC_WRAPPER=sccache: {:?}",
        cfg.env,
    );
}

/// Install [`install_bd_bead_stub`] with `bead_json` and return a PATH
/// value with the stub's bin dir prepended. `loom todo` / `loom loop`
/// drive bd through `tokio::process::Command::new("bd")`, which is not
/// on PATH in the nix build sandbox; without the stub every spawn-bound
/// test aborts inside `build_prompt` at the first bd query.
fn bd_stub_path(workspace: &Path, bead_json: &str) -> std::ffi::OsString {
    let bd_bin_dir = install_bd_bead_stub(workspace, bead_json);
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let mut entries = vec![bd_bin_dir];
    entries.extend(std::env::split_paths(&path_var));
    std::env::join_paths(entries).expect("join PATH")
}

/// Initialise loom's cache DB and seed the named spec for the test.
fn seed_active_spec(workspace: &Path, _loom_bin: &str, label: &str) {
    use loom_driver::identifier::SpecLabel;
    use loom_driver::state::CacheDb;
    let spec_dir = workspace.join("specs");
    let docs_dir = workspace.join("docs");
    std::fs::create_dir_all(&spec_dir).expect("mkdir specs");
    std::fs::create_dir_all(&docs_dir).expect("mkdir docs");
    std::fs::write(spec_dir.join(format!("{label}.md")), format!("# {label}\n"))
        .expect("write spec");
    std::fs::write(
        docs_dir.join("README.md"),
        format!("- [{label}](../specs/{label}.md)\n"),
    )
    .expect("write spec index");
    let spec_path = format!("specs/{label}.md");
    let add = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["add", "docs/README.md", &spec_path])
        .status()
        .expect("git add seeded spec");
    assert!(add.success(), "git add seeded spec failed: {add}");
    let commit = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["commit", "-q", "-m", "seed active spec"])
        .status()
        .expect("git commit seeded spec");
    assert!(commit.success(), "git commit seeded spec failed: {commit}");
    let state_dir = workspace.join(".loom");
    std::fs::create_dir_all(&state_dir).expect("mkdir .loom");
    let db = CacheDb::open(state_dir.join("cache.db")).expect("open state db");
    let spec = SpecLabel::new(label);
    // `replace_companions` is the canonical insert-or-ignore on `specs`
    // (no companions seeded here — we just need the row to exist).
    db.replace_companions(&spec, &[]).expect("seed spec row");
}

/// Seed the workspace as a real git repo plus the loom-owned integration
/// workspace at `.loom/integration/` and a bare `origin` so
/// `loom loop`'s per-bead dispatch + push gate both succeed.
///
/// `loom todo` opens a `GitClient` during setup so the tier-1 detection
/// has a real ref database to query even when the test exits before any
/// tier-1 work happens.
fn init_workspace_repo(workspace: &Path) {
    loom_driver::git::init_test_repo_with_integration(workspace)
        .expect("init test repo with loom integration");
}

/// Install a stub `loom` shim that exits 0 for any args. Threaded via
/// `LOOM_BIN` so the per-bead gate's `loom gate verify --diff` /
/// `loom gate review --diff --bead` subprocesses (per `specs/gate.md` § *Per-diff
/// stage checks*) are no-ops in tests that exercise only the run-phase
/// path; without it the review subprocess spawns an agent backend the
/// test fixtures don't fully wire.
fn install_loom_noop_stub(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let stub = dir.join("loom-noop-stub.sh");
    std::fs::write(
        &stub,
        "#!/usr/bin/env bash\n\
         set -euo pipefail\n\
         if [[ \"${2:-}\" == \"review\" ]]; then\n\
             echo 'LOOM_COMPLETE'\n\
         fi\n",
    )
    .expect("write loom stub");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
        .expect("chmod loom stub");
    stub
}

/// Loom hands the wrapper exactly `wrix spawn --spawn-config <file>
/// --stdio`, and the file resolves to a JSON [`SpawnConfig`] carrying
/// the per-bead profile image. A future profile-resolution change that
/// drops the `image_ref`/`image_source` fields or renames the
/// subcommand will trip this assertion before the wrapper ever sees the
/// malformed argv.
#[test]
fn wrix_spawn_invocation_records_correct_argv() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let shim_dir = dir.path().join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let argv_file = shim_dir.join("argv.txt");
    let stdin_info = shim_dir.join("stdin-info.txt");
    let spawn_copy = shim_dir.join("spawn-config.json");
    let shim = install_wrix_shim(
        &shim_dir,
        &argv_file,
        &stdin_info,
        &spawn_copy,
        &mock_pi_path(),
        "happy-path",
    );

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = drive_loom_todo_pi(workspace, &shim, loom_bin);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom todo --agent pi must exit 0 against the mock pi shim. stdout={stdout} stderr={stderr}",
    );

    let argv = std::fs::read_to_string(&argv_file).expect("shim should record argv");
    let tokens: Vec<&str> = argv.lines().collect();
    assert_eq!(
        tokens.first().copied(),
        Some("spawn"),
        "first arg must be spawn. argv={tokens:?}",
    );
    let spawn_idx = tokens
        .iter()
        .position(|t| *t == "--spawn-config")
        .unwrap_or_else(|| panic!("--spawn-config flag missing from argv. argv={tokens:?}"));
    let spawn_config_path = tokens.get(spawn_idx + 1).unwrap_or_else(|| {
        panic!("--spawn-config without a value. argv={tokens:?}");
    });
    assert!(
        Path::new(spawn_config_path).is_absolute(),
        "spawn-config path must be absolute (wrapper consumes it from /tmp). got={spawn_config_path}",
    );
    assert!(
        tokens.contains(&"--stdio"),
        "--stdio flag missing from argv. argv={tokens:?}",
    );

    // The spawn-config JSON must round-trip through SpawnConfig and carry
    // the resolved image_ref + image_source from the manifest written by
    // `drive_loom_todo_pi` (`base` + `pi` maps to `localhost/wrix-base-pi:test`).
    let bytes = std::fs::read(&spawn_copy).expect("shim should copy spawn-config aside");
    let cfg: loom_driver::agent::SpawnConfig =
        serde_json::from_slice(&bytes).expect("spawn-config must deserialize");
    assert_eq!(
        cfg.image_ref, "localhost/wrix-base-pi:test",
        "spawn-config image_ref must match the resolved profile image",
    );
    assert!(
        !cfg.image_source.as_os_str().is_empty(),
        "spawn-config image_source must be populated. got={}",
        cfg.image_source.display(),
    );
    assert!(
        cfg.initial_prompt.contains("agent"),
        "initial prompt should reference the spec label. prompt={}",
        cfg.initial_prompt,
    );
}

#[test]
fn wrix_spawn_config_includes_configured_sccache_mount_and_env() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let host_cache = enable_workspace_sccache(workspace);

    let shim_dir = dir.path().join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let argv_file = shim_dir.join("argv.txt");
    let stdin_info = shim_dir.join("stdin-info.txt");
    let spawn_copy = shim_dir.join("spawn-config.json");
    let shim = install_wrix_shim(
        &shim_dir,
        &argv_file,
        &stdin_info,
        &spawn_copy,
        &mock_pi_path(),
        "happy-path",
    );

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = drive_loom_todo_pi(workspace, &shim, loom_bin);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom todo --agent pi must exit 0 against the mock pi shim. stdout={stdout} stderr={stderr}",
    );
    assert_spawn_config_uses_sccache(&spawn_copy, &host_cache);
}

/// The run-time promise from `specs/harness.md` *Run UX & Logging*
/// is that every `loom todo` invocation emits a per-phase JSONL file
/// under `<workspace>/.loom/logs/<spec-label>/todo-<utc>.jsonl`.
/// Without this gate the workflow happily ran agents to completion while
/// `run_agent` previously discarded every event with a `trace!` call;
/// users saw two INFO lines and an empty `loom logs`. The test drives
/// the same mock-pi handshake as the dispatch tests above, then asserts
/// the log file appears at the documented path with at least one valid
/// event line that round-trips through `serde_json`. A future regression
/// that removes the sink wiring trips this assertion before any
/// user-visible breakage.
#[test]
fn loom_todo_writes_jsonl_log_under_workspace_logs_dir() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let shim_dir = dir.path().join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let argv_file = shim_dir.join("argv.txt");
    let stdin_info = shim_dir.join("stdin-info.txt");
    let spawn_copy = shim_dir.join("spawn-config.json");
    let shim = install_wrix_shim(
        &shim_dir,
        &argv_file,
        &stdin_info,
        &spawn_copy,
        &mock_pi_path(),
        "happy-path",
    );

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = drive_loom_todo_pi(workspace, &shim, loom_bin);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom todo --agent pi must exit 0 against the mock pi shim. stdout={stdout} stderr={stderr}",
    );

    // The todo phase log path is `<workspace>/.loom/logs/todo/todo-<utc>.jsonl`.
    let logs_dir = workspace.join(".loom/logs/todo");
    assert!(
        logs_dir.is_dir(),
        "phase log directory must exist after `loom todo`: {}\nstdout={stdout}\nstderr={stderr}",
        logs_dir.display(),
    );
    let entries: Vec<_> = std::fs::read_dir(&logs_dir)
        .expect("read logs dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "exactly one JSONL file must appear under {}: got {entries:?}",
        logs_dir.display(),
    );
    let log_path = &entries[0];
    let stem = log_path.file_stem().and_then(|s| s.to_str()).unwrap();
    assert!(
        stem.starts_with("todo-"),
        "phase log file stem must start with `todo-`: got {stem}",
    );

    let body = std::fs::read_to_string(log_path).expect("read log");
    let lines: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
    assert!(
        !lines.is_empty(),
        "log file must contain at least one event line, got empty body. path={}",
        log_path.display(),
    );
    for (i, line) in lines.iter().enumerate() {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("line {i} is not valid JSON: {e}\nline={line}"));
        assert!(
            v.get("kind").and_then(|k| k.as_str()).is_some(),
            "every event must carry a `kind` field. line {i}: {line}",
        );
    }
    let last: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert_eq!(
        last["kind"], "session_complete",
        "the final event must be session_complete. lines={lines:?}",
    );
}

/// Spec promise (`specs/harness.md` *Run UX & Logging*): every
/// bead processed by `loom loop` writes a per-bead JSONL log under
/// `<workspace>/.loom/logs/<spec>/<bead-id>-<utc>.jsonl`. Guards
/// against the regression where the production sequential controller
/// passed `None` for the sink and every agent event was discarded. The
/// bd stub returns one ready bead so `LoopMode::Once` exercises the full
/// `next_ready_bead` → `run_bead` → `close_bead` path; the wrix shim
/// and mock-pi finish the protocol so the sink reaches `session_complete`
/// before being dropped.
#[test]
fn loom_loop_once_writes_per_bead_jsonl_log() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    init_workspace_repo(workspace);

    let manifest_path = workspace.join("profile-images.json");
    let image_source = workspace.join("base.tar");
    std::fs::write(&image_source, "").unwrap();
    let manifest_body = format!(
        r#"{{
          "base": {{ "pi": {{ "ref": "localhost/wrix-base-pi:test", "source": {source:?} }}, "claude": {{ "ref": "localhost/wrix-base-claude:test", "source": {source:?} }}, "direct": {{ "ref": "localhost/wrix-base-direct:test", "source": {source:?} }} }}
        }}"#,
        source = image_source.display().to_string(),
    );
    std::fs::write(&manifest_path, manifest_body).unwrap();

    let shim_dir = workspace.join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let argv_file = shim_dir.join("argv.txt");
    let stdin_info = shim_dir.join("stdin-info.txt");
    let spawn_copy = shim_dir.join("spawn-config.json");
    let shim = install_wrix_shim(
        &shim_dir,
        &argv_file,
        &stdin_info,
        &spawn_copy,
        &mock_pi_path(),
        "happy-path",
    );

    let bead_json = r#"[{"id":"lm-runtest","title":"run gate bead","description":"","status":"open","priority":2,"issue_type":"task","labels":["spec:agent","profile:base"]}]"#;
    let bd_bin_dir = install_bd_bead_stub(workspace, bead_json);
    let loom_noop_stub = install_loom_noop_stub(workspace);

    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = vec![bd_bin_dir];
    path_entries.extend(std::env::split_paths(&path_var));
    let new_path = std::env::join_paths(path_entries).unwrap();

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .arg("--agent")
        .arg("pi")
        .arg("loop")
        .arg("--once")
        .arg("-s")
        .arg("agent")
        .env("PATH", new_path)
        .env("LOOM_WRIX_BIN", &shim)
        // Point `LOOM_BIN` at a no-op shim so the per-bead gate's
        // `loom gate verify --diff` + `loom gate review --diff --bead` calls
        // exit 0 silently — this test asserts run-phase JSONL log
        // writes, not gate dispatch.
        .env("LOOM_BIN", &loom_noop_stub)
        .env("LOOM_PROFILES_MANIFEST", &manifest_path)
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
        "loom loop --once must exit 0 against the bd + wrix stubs. stdout={stdout} stderr={stderr}",
    );

    let logs_dir = workspace.join(".loom/logs/agent");
    assert!(
        logs_dir.is_dir(),
        "per-bead log directory must exist after `loom loop --once`: {}\nstdout={stdout}\nstderr={stderr}",
        logs_dir.display(),
    );
    let entries: Vec<_> = std::fs::read_dir(&logs_dir)
        .expect("read logs dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .filter(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with("lm-runtest-"))
        })
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "exactly one per-bead JSONL file must appear at `<logs>/loom-agent/lm-runtest-*.jsonl`: got {entries:?}",
    );

    let body = std::fs::read_to_string(&entries[0]).expect("read log");
    let lines: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
    assert!(
        !lines.is_empty(),
        "bead log must contain at least one event line. path={}",
        entries[0].display(),
    );
    for (i, line) in lines.iter().enumerate() {
        let _: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("line {i} is not valid JSON: {e}\nline={line}"));
    }
    let session_complete_idx = lines
        .iter()
        .position(|line| {
            let v: serde_json::Value = serde_json::from_str(line).expect("json");
            v["kind"] == "session_complete"
        })
        .unwrap_or_else(|| panic!("no session_complete event in log. lines={lines:?}"));
    // After session_complete the run-phase verdict gate appends
    // driver events for bead_branch_pushed / merge_ok /
    // worktree_cleanup_ok so operators tailing the loop see the
    // dispatch-to-dispatch gap as named steps.
    for line in &lines[session_complete_idx + 1..] {
        let v: serde_json::Value = serde_json::from_str(line).expect("json");
        assert_eq!(
            v["kind"], "driver_event",
            "every post-session_complete line must be a driver_event. line={line}",
        );
    }
}

/// `loom gate review` must write its phase log under
/// `<workspace>/.loom/logs/<spec>/review-<utc>.jsonl` (same spec
/// section as the run gate). Guards against the regression where the
/// production review controller passed `None` for the sink and the
/// reviewer agent's events were discarded. The bd stub returns one bead
/// carrying `loom:clarify` so the post-snapshot diff yields
/// `ReviewVerdict::PushBlocked` and the gate
/// exits without touching `git push` / `beads-push` / `loom loop` — keeping
/// the test environment-independent.
#[test]
fn loom_gate_review_writes_phase_jsonl_log() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    init_workspace_repo(workspace);

    // build_review_prompt loads `[verify]`/`[judge]` sources from
    // specs/<label>.md; seed an empty Success Criteria section so the
    // loader succeeds with no bodies.
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(workspace.join("specs/agent.md"), "## Success Criteria\n\n").unwrap();

    let manifest_path = workspace.join("profile-images.json");
    let image_source = workspace.join("base.tar");
    std::fs::write(&image_source, "").unwrap();
    let manifest_body = format!(
        r#"{{
          "base": {{ "pi": {{ "ref": "localhost/wrix-base-pi:test", "source": {source:?} }}, "claude": {{ "ref": "localhost/wrix-base-claude:test", "source": {source:?} }}, "direct": {{ "ref": "localhost/wrix-base-direct:test", "source": {source:?} }} }}
        }}"#,
        source = image_source.display().to_string(),
    );
    std::fs::write(&manifest_path, manifest_body).unwrap();

    let shim_dir = workspace.join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let argv_file = shim_dir.join("argv.txt");
    let stdin_info = shim_dir.join("stdin-info.txt");
    let spawn_copy = shim_dir.join("spawn-config.json");
    let shim = install_wrix_shim(
        &shim_dir,
        &argv_file,
        &stdin_info,
        &spawn_copy,
        &mock_pi_path(),
        "happy-path",
    );

    {
        use loom_driver::identifier::SpecLabel;
        use loom_driver::state::CacheDb;
        let db = CacheDb::open(workspace.join(".loom/cache.db")).expect("open cache db");
        db.upsert_spec(&SpecLabel::new("agent"), "specs/agent.md")
            .expect("seed spec");
    }

    // `loom:clarify` on the post-snapshot bead → ReviewVerdict::PushBlocked →
    // ReviewResult::PushBlocked, no push gates fire.
    let bead_json = r#"[{"id":"lm-reviewtest","title":"review gate bead","description":"","status":"open","priority":2,"issue_type":"task","labels":["spec:agent","loom:clarify"]}]"#;
    let bd_bin_dir = install_bd_bead_stub(workspace, bead_json);

    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = vec![bd_bin_dir];
    path_entries.extend(std::env::split_paths(&path_var));
    let new_path = std::env::join_paths(path_entries).unwrap();

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .arg("--agent")
        .arg("pi")
        .arg("gate")
        .arg("review")
        .arg("--diff")
        .arg("HEAD..HEAD")
        .env("PATH", new_path)
        .env("LOOM_WRIX_BIN", &shim)
        .env("LOOM_BIN", loom_bin)
        .env("LOOM_PROFILES_MANIFEST", &manifest_path)
        .env("XDG_STATE_HOME", workspace.join(".loom-test-state"))
        // Bypass the nested-loom guard so cargo test inside a loom container
        // still reaches the review dispatch path under test.
        .env_remove("LOOM_INSIDE")
        .output()
        .expect("spawn loom");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom gate review must exit 0 against the bd + wrix stubs. stdout={stdout} stderr={stderr}",
    );

    let logs_dir = workspace.join(".loom/logs/agent");
    assert!(
        logs_dir.is_dir(),
        "phase log directory must exist after `loom gate review`: {}\nstdout={stdout}\nstderr={stderr}",
        logs_dir.display(),
    );
    let entries: Vec<_> = std::fs::read_dir(&logs_dir)
        .expect("read logs dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .filter(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with("review-"))
        })
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "exactly one phase JSONL file must appear at `<logs>/loom-agent/review-*.jsonl`: got {entries:?}",
    );

    let body = std::fs::read_to_string(&entries[0]).expect("read log");
    let lines: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
    assert!(
        !lines.is_empty(),
        "phase log must contain at least one event line. path={}",
        entries[0].display(),
    );
    let parsed: Vec<serde_json::Value> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line {i} is not valid JSON: {e}\nline={line}"))
        })
        .collect();
    // The reviewer agent emits `session_complete` once when its session
    // ends; the verdict gate then appends one or more `driver_event`
    // records (push_gate_walk + the branch event) AFTER
    // session_complete. Both contracts must hold:
    let session_complete_count = parsed
        .iter()
        .filter(|v| v["kind"] == "session_complete")
        .count();
    assert_eq!(
        session_complete_count, 1,
        "exactly one session_complete must appear in the phase log. lines={lines:?}",
    );
    let driver_events: Vec<&serde_json::Value> = parsed
        .iter()
        .filter(|v| v["kind"] == "driver_event")
        .collect();
    assert!(
        !driver_events.is_empty(),
        "verdict gate must emit at least one push_gate_* driver event after session_complete. lines={lines:?}",
    );
    // The first driver event is always `push_gate_walk` — the fence
    // every branch shares.
    assert_eq!(
        driver_events[0]["driver_kind"], "push_gate_walk",
        "first driver event must be push_gate_walk. got: {}",
        driver_events[0],
    );
}

/// Install a `bd` shim that returns `bead_json` for JSON list subcommands,
/// returns a synthetic id for `bd create --json`, and exits 0 silently for
/// everything else (`bd close`, `bd update`). Returns the bin directory the
/// caller should prepend to PATH.
fn install_bd_bead_stub(dir: &Path, bead_json: &str) -> PathBuf {
    let bin_dir = dir.join("bd-bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let bd = bin_dir.join("bd");
    let bash = find_bash();
    let body = format!(
        "#!{bash}\n\
         set -euo pipefail\n\
         if [[ \"${{1:-}}\" == 'create' ]]; then\n\
             echo 'lm-work'\n\
             exit 0\n\
         fi\n\
         for arg in \"$@\"; do\n\
             if [[ \"$arg\" == '--json' ]]; then\n\
                 cat <<'__BD_BEAD_JSON__'\n\
{bead}\n\
__BD_BEAD_JSON__\n\
                 exit 0\n\
             fi\n\
         done\n\
         exit 0\n",
        bash = bash.display(),
        bead = bead_json,
    );
    std::fs::write(&bd, body).unwrap();
    let mut perm = std::fs::metadata(&bd).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&bd, perm).unwrap();
    bin_dir
}

/// The agent process receives stdin as a pipe, never a TTY. EOF on that
/// pipe is the signal loom uses to tell the agent "no more input is
/// coming"; if the underlying handle were a TTY (or a regular file), the
/// agent's `read` would either block or return non-EOF, breaking the
/// shutdown contract.
#[test]
fn child_stdin_is_a_pipe_not_a_tty() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let shim_dir = dir.path().join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let argv_file = shim_dir.join("argv.txt");
    let stdin_info = shim_dir.join("stdin-info.txt");
    let spawn_copy = shim_dir.join("spawn-config.json");
    let shim = install_wrix_shim(
        &shim_dir,
        &argv_file,
        &stdin_info,
        &spawn_copy,
        &mock_pi_path(),
        "happy-path",
    );

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = drive_loom_todo_pi(workspace, &shim, loom_bin);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom todo --agent pi must exit 0 against the mock pi shim. stdout={stdout} stderr={stderr}",
    );

    let info = std::fs::read_to_string(&stdin_info).expect("shim should record stdin info");
    assert!(
        info.contains("stdin_is_tty=0"),
        "child stdin must NOT be a TTY (got {info:?})",
    );
    assert!(
        info.contains("stdin_is_pipe=1"),
        "child stdin must be a pipe — both backends call Stdio::piped() (got {info:?})",
    );

    // The mock-pi handshake completing end-to-end is the second half of
    // the EOF contract: the pi backend writes get_state then prompt
    // through the same pipe, mock-pi reads each line, responds, and the
    // session reaches agent_end. If stdin were not a pipe, those `read`
    // calls would either block forever (TTY without echo) or return
    // wrong data; either way `loom todo` would not exit 0.
    assert!(
        stdout.contains("loom todo:"),
        "expected the loom todo summary line, indicating the agent reached agent_end. \
         stdout={stdout} stderr={stderr}",
    );
}

/// Spec promise (`specs/agent.md` Compaction repin): the
/// production driver detects `compaction_start` and sends
/// `RePinContent::to_prompt()` via `steer`. A prior regression was
/// silent because the only test of this behavior lived inside
/// `loom-agent/src/pi/backend.rs` and stood in for the workflow layer:
/// the test itself called `session.steer(...)` instead of driving
/// through `run_agent`. Production wiring was missing for months
/// without a failing test.
///
/// This test drives `loom todo --agent pi` end-to-end through `dispatch`
/// → `run_agent::<PiBackend>` → `PiBackend::on_compaction_start`. The
/// shim hands stdio to `mock-pi compaction`, which BLOCKS on `read` until
/// it observes the steer carrying the re-pin payload. If the production
/// `run_agent` event loop fails to call `on_compaction_start`, the mock
/// reads no steer line, never emits `agent_end`, and the loom binary
/// hangs — the wall-clock timeout below converts that hang into a clean
/// test failure.
#[test]
fn loom_todo_pi_compaction_drives_repin_steer_through_run_agent() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let shim_dir = dir.path().join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let argv_file = shim_dir.join("argv.txt");
    let stdin_info = shim_dir.join("stdin-info.txt");
    let spawn_copy = shim_dir.join("spawn-config.json");
    let shim = install_wrix_shim(
        &shim_dir,
        &argv_file,
        &stdin_info,
        &spawn_copy,
        &mock_pi_path(),
        "compaction",
    );

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = drive_loom_todo_pi(workspace, &shim, loom_bin);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom todo --agent pi must exit 0 against mock-pi compaction. \
         If this hangs/fails, the production driver is not sending the \
         re-pin steer on CompactionStart. stdout={stdout} stderr={stderr}",
    );

    // The mock echoes "repin: <payload>" as a text_delta after it
    // observes the steer. The on-disk JSONL log contains every event the
    // driver consumed, so we can confirm both the compaction_start event
    // arrived and the steer reached the mock by inspecting the log.
    let logs_dir = workspace.join(".loom/logs/todo");
    let log_path = std::fs::read_dir(&logs_dir)
        .expect("read logs dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .expect("phase log file should exist after loom todo");
    let body = std::fs::read_to_string(&log_path).expect("read log");
    let lines: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
    let events: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).expect("valid JSON"))
        .collect();

    assert!(
        events.iter().any(|e| e["kind"] == "compaction_start"),
        "compaction_start event must appear in the log. events={events:?}",
    );
    assert!(
        events.iter().any(|e| {
            e["kind"] == "text_delta"
                && e["text"].as_str().is_some_and(|t| t.starts_with("repin: "))
        }),
        "mock-pi must echo the re-pin payload back as a text_delta — \
         absence means the production driver did not steer on CompactionStart. \
         events={events:?}",
    );
    assert_eq!(
        events.last().expect("at least one event")["kind"],
        "session_complete",
        "the final event must be session_complete. events={events:?}",
    );
}

/// Spec promise (`specs/agent.md` Functional #4 second bullet):
/// the production driver runs the SIGTERM → SIGKILL escalation after
/// observing `result`. A prior regression was silent because the only
/// test of this behavior lived inside `loom-agent/src/claude/backend.rs`
/// and called `ClaudeBackend::shutdown_after_result` directly —
/// production wiring through `run_agent::<ClaudeBackend>` was missing
/// without a failing test.
///
/// This test drives `loom todo --agent claude` end-to-end. The shim hands
/// stdio to mock-claude in `ignore-stdin` mode, which emits `result/success`,
/// then traps SIGTERM and loops forever. Without the wiring, `run_agent`
/// returns immediately on SessionComplete and the loom binary exits in
/// milliseconds; with the wiring, the watchdog waits `grace=1s` for the
/// child to exit on its own, sends SIGTERM (ignored by the mock), waits
/// another second, then escalates to SIGKILL — total elapsed ≥ ~2s.
/// The elapsed-time assertion is what makes this test catch a regression.
#[test]
fn loom_todo_claude_runs_shutdown_watchdog_through_run_agent() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    init_workspace_repo(workspace);

    std::fs::write(
        workspace.join("loom.toml"),
        "[claude]\npost_result_grace_secs = 1\n",
    )
    .unwrap();

    let manifest_path = workspace.join("profile-images.json");
    let image_source = workspace.join("base.tar");
    std::fs::write(&image_source, "").unwrap();
    let manifest_body = format!(
        r#"{{
          "base": {{ "pi": {{ "ref": "localhost/wrix-base-pi:test", "source": {source:?} }}, "claude": {{ "ref": "localhost/wrix-base-claude:test", "source": {source:?} }}, "direct": {{ "ref": "localhost/wrix-base-direct:test", "source": {source:?} }} }}
        }}"#,
        source = image_source.display().to_string(),
    );
    std::fs::write(&manifest_path, manifest_body).unwrap();

    let shim_dir = workspace.join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let argv_file = shim_dir.join("argv.txt");
    let stdin_info = shim_dir.join("stdin-info.txt");
    let spawn_copy = shim_dir.join("spawn-config.json");
    let shim = install_wrix_shim(
        &shim_dir,
        &argv_file,
        &stdin_info,
        &spawn_copy,
        &mock_claude_path(),
        "ignore-stdin",
    );

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    seed_active_spec(workspace, loom_bin, "agent");
    let new_path = bd_stub_path(workspace, "[]");
    let started = std::time::Instant::now();
    let output = Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .arg("--agent")
        .arg("claude")
        .arg("todo")
        .env("PATH", new_path)
        .env("LOOM_WRIX_BIN", &shim)
        .env("LOOM_BIN", loom_bin)
        .env("LOOM_PROFILES_MANIFEST", &manifest_path)
        .env("RUST_LOG", "loom_agent=warn")
        .env("XDG_STATE_HOME", workspace.join(".loom-test-state"))
        // Bypass the nested-loom guard so cargo test inside a loom container
        // still reaches the todo dispatch path under test.
        .env_remove("LOOM_INSIDE")
        .output()
        .expect("spawn loom");
    let elapsed = started.elapsed();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom todo --agent claude must exit 0 against mock-claude ignore-stdin. \
         stdout={stdout} stderr={stderr}",
    );

    assert!(
        elapsed >= Duration::from_millis(1500),
        "elapsed {elapsed:?} too short — the shutdown watchdog was not \
         invoked from run_agent. With grace=1s the watchdog must wait once \
         for stdin-close, escalate to SIGTERM (ignored), then SIGKILL — \
         total ≥ ~2s. stderr={stderr}",
    );

    assert!(
        stderr.contains("SIGKILL"),
        "expected SIGKILL escalation log in stderr — mock-claude ignores \
         SIGTERM so the watchdog must escalate. Absence means \
         after_session_complete was not invoked. stderr={stderr}",
    );
}

/// Pi handshake against an unresponsive launcher must surface
/// `ProtocolError::HandshakeTimeout` within the configured budget
/// instead of hanging silently. mock-pi `hang-probe` reads the
/// `get_state` line and then sleeps; without the bounded handshake the
/// loom binary would block forever waiting for the response.
#[test]
fn loom_todo_pi_hang_probe_surfaces_handshake_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    init_workspace_repo(workspace);

    let manifest_path = workspace.join("profile-images.json");
    let image_source = workspace.join("base.tar");
    std::fs::write(&image_source, "").unwrap();
    let manifest_body = format!(
        r#"{{
          "base": {{ "pi": {{ "ref": "localhost/wrix-base-pi:test", "source": {source:?} }}, "claude": {{ "ref": "localhost/wrix-base-claude:test", "source": {source:?} }}, "direct": {{ "ref": "localhost/wrix-base-direct:test", "source": {source:?} }} }}
        }}"#,
        source = image_source.display().to_string(),
    );
    std::fs::write(&manifest_path, manifest_body).unwrap();

    let shim_dir = workspace.join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let argv_file = shim_dir.join("argv.txt");
    let stdin_info = shim_dir.join("stdin-info.txt");
    let spawn_copy = shim_dir.join("spawn-config.json");
    let shim = install_wrix_shim(
        &shim_dir,
        &argv_file,
        &stdin_info,
        &spawn_copy,
        &mock_pi_path(),
        "hang-probe",
    );

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    seed_active_spec(workspace, loom_bin, "agent");
    let new_path = bd_stub_path(workspace, "[]");
    let started = Instant::now();
    let output = Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .arg("--agent")
        .arg("pi")
        .arg("todo")
        .env("PATH", new_path)
        .env("LOOM_WRIX_BIN", &shim)
        .env("LOOM_BIN", loom_bin)
        .env("LOOM_PROFILES_MANIFEST", &manifest_path)
        .env("LOOM_HANDSHAKE_TIMEOUT_MS", "500")
        .env("RUST_LOG", "loom_agent=warn")
        .env("XDG_STATE_HOME", workspace.join(".loom-test-state"))
        // Bypass the nested-loom guard so cargo test inside a loom container
        // still reaches the todo dispatch path under test.
        .env_remove("LOOM_INSIDE")
        .output()
        .expect("spawn loom");
    let elapsed = started.elapsed();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "loom todo must fail when the pi probe hangs — exited successfully \
         which means the timeout is not wired. stdout={stdout} stderr={stderr}",
    );
    assert!(
        elapsed < Duration::from_secs(15),
        "loom todo must surface HandshakeTimeout within the configured \
         budget — elapsed {elapsed:?} suggests a hung probe. stderr={stderr}",
    );
    assert!(
        stderr.contains("pi handshake timed out") || stderr.contains("HandshakeTimeout"),
        "expected handshake-timeout signal in stderr; got stderr={stderr}",
    );
}

/// Mid-session silence must trip the run_agent stall heartbeat.
/// mock-pi `stall-mid-session` answers the probe and acks one
/// prompt, then sleeps; with `LOOM_STALL_WARN_MS=300` the run loop must
/// emit `"no agent event for stall window"` to stderr while the agent
/// remains spawned. The test kills loom after observing the warning so
/// the never-exiting mock does not stretch the suite.
#[test]
fn loom_todo_pi_stall_mid_session_emits_stall_warning() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    init_workspace_repo(workspace);

    let manifest_path = workspace.join("profile-images.json");
    let image_source = workspace.join("base.tar");
    std::fs::write(&image_source, "").unwrap();
    let manifest_body = format!(
        r#"{{
          "base": {{ "pi": {{ "ref": "localhost/wrix-base-pi:test", "source": {source:?} }}, "claude": {{ "ref": "localhost/wrix-base-claude:test", "source": {source:?} }}, "direct": {{ "ref": "localhost/wrix-base-direct:test", "source": {source:?} }} }}
        }}"#,
        source = image_source.display().to_string(),
    );
    std::fs::write(&manifest_path, manifest_body).unwrap();

    let shim_dir = workspace.join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let argv_file = shim_dir.join("argv.txt");
    let stdin_info = shim_dir.join("stdin-info.txt");
    let spawn_copy = shim_dir.join("spawn-config.json");
    let shim = install_wrix_shim(
        &shim_dir,
        &argv_file,
        &stdin_info,
        &spawn_copy,
        &mock_pi_path(),
        "stall-mid-session",
    );

    // Spawn loom as the leader of a fresh process group so the cleanup
    // step at the end can kill the entire group. The mock-pi `exec sleep`
    // grandchild inherits stderr from loom; without group-kill it would
    // outlive `child.kill()` (SIGKILL doesn't run loom's drop chain, so
    // tokio's `kill_on_drop` doesn't fire on the grandchild) and keep the
    // stderr pipe open, hanging `reader.join()` forever.
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    seed_active_spec(workspace, loom_bin, "agent");
    let new_path = bd_stub_path(workspace, "[]");
    let mut child = Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .arg("--agent")
        .arg("pi")
        .arg("todo")
        .env("PATH", new_path)
        .env("LOOM_WRIX_BIN", &shim)
        .env("LOOM_BIN", loom_bin)
        .env("LOOM_PROFILES_MANIFEST", &manifest_path)
        .env("LOOM_STALL_WARN_MS", "300")
        .env("RUST_LOG", "loom_workflow=warn")
        .env("XDG_STATE_HOME", workspace.join(".loom-test-state"))
        // Bypass the nested-loom guard so cargo test inside a loom container
        // still reaches the todo dispatch path under test.
        .env_remove("LOOM_INSIDE")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("spawn loom");

    let pgid = Pid::from_raw(-(child.id() as i32));

    let stderr = child.stderr.take().expect("stderr piped");
    let buf: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let buf_thread = Arc::clone(&buf);
    let reader = thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let mut g = buf_thread.lock().unwrap();
            g.push_str(&line);
            g.push('\n');
        }
    });

    let needle = "no agent event for stall window";
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_warning = false;
    while Instant::now() < deadline {
        if buf.lock().unwrap().contains(needle) {
            saw_warning = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }

    let _ = kill(pgid, Signal::SIGKILL);
    let _ = child.wait();
    let _ = reader.join();

    let body = buf.lock().unwrap().clone();
    assert!(
        saw_warning,
        "expected stall warning `{needle}` within 10s of LOOM_STALL_WARN_MS=300 \
         — absence means the heartbeat is not wired through run_agent. \
         stderr=\n{body}",
    );
}

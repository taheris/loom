//! Live-path test for `run_gate_mint` dispatching through the production
//! `MintWalker`.
//!
//! Pins `specs/gate.md` § *Production walker wiring* criterion
//! `run_gate_mint_dispatches_through_production_walker_not_empty_vec`:
//! the CLI arm must obtain its `Vec<Finding>` from
//! `mint::walk::walk(walker, scope, validator)` and never short-circuit
//! to `Vec::<Finding>::new()` (unconditionally or behind a never-true
//! guard). The walker is the only path findings reach the mint pipeline.
//!
//! Invokes the compiled `loom` binary as a subprocess against a fixture
//! workspace whose profile manifest cannot resolve the default profile.
//! The walker's `run_rubric` issues `bd list` against the spec label
//! *before* the manifest lookup that fails — so a bd-shim invocation
//! log carrying a matching `list --json --label=spec:<X>` line proves
//! `mint_via_walker` was reached from `run_gate_mint`. A regression
//! that bypassed the walker would leave the invocation log empty and
//! the command would exit zero with no findings minted, failing both
//! assertions below.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const SPEC_LABEL: &str = "walker_pin";

/// Install the `bd-shim` test helper as `bd` on a fresh PATH dir. The
/// shim logs every invocation to `BD_STATE_DIR/.invocations.log`.
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

/// Pin a PATH starting at `bin_dir` plus the ambient PATH so the loom
/// child resolves `bd` to the shim while still finding `git` and the
/// other host tools `current_commit` shells out to.
fn pinned_path(bin_dir: &Path) -> std::ffi::OsString {
    let ambient = std::env::var_os("PATH").unwrap_or_default();
    let mut entries: Vec<PathBuf> = vec![bin_dir.to_path_buf()];
    entries.extend(std::env::split_paths(&ambient));
    std::env::join_paths(entries).expect("join PATH")
}

/// Run `loom --workspace <ws> gate mint --diff HEAD --spec <SPEC_LABEL>`
/// with the bd shim on PATH and an empty profile manifest. Returns the
/// subprocess output and the bd-shim invocation log path so callers can
/// inspect both.
fn run_gate_mint_subprocess(workspace: &Path) -> (std::process::Output, PathBuf) {
    let bin_dir = install_bd_shim(workspace);
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).expect("mkdir bd-state");

    let manifest_path = workspace.join("profile-images.json");
    std::fs::write(&manifest_path, "{}").expect("write empty manifest");

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .args(["gate", "mint", "--diff", "HEAD", "--spec", SPEC_LABEL])
        .env("PATH", pinned_path(&bin_dir))
        .env("LOOM_PROFILES_MANIFEST", &manifest_path)
        .env("BD_STATE_DIR", &state_dir)
        .env("LOOM_BIN", loom_bin)
        .env("XDG_STATE_HOME", workspace.join(".loom-test-state"))
        .env_remove("LOOM_INSIDE")
        .output()
        .expect("spawn loom");
    (output, state_dir.join(".invocations.log"))
}

/// Initialise the fixture workspace as a git repo with one commit so the
/// `--diff HEAD` scope resolves. `resolve_gate_scope` fails loudly when a
/// `--diff` range can't be parsed (e.g. outside a git repo), so the
/// walker-wiring fixture must be a real git workspace — the same shape
/// mint runs against in production.
fn git_init_workspace(workspace: &Path) {
    let run = |args: &[&str]| {
        let ok = Command::new("git")
            .arg("-C")
            .arg(workspace)
            .args(args)
            .status()
            .expect("spawn git")
            .success();
        assert!(ok, "git {args:?} failed");
    };
    run(&["init", "-q", "-b", "main"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test"]);
    run(&["config", "commit.gpgsign", "false"]);
    std::fs::write(workspace.join("README.md"), "fixture\n").expect("write README");
    run(&["add", "README.md"]);
    run(&["commit", "-q", "-m", "init"]);
}

/// Write a minimal spec the walker can render against. No `[test]` /
/// `[judge]` annotations — `load_review_sources` returns empty vecs so
/// the prompt build proceeds to the manifest lookup that fails.
fn write_fixture_spec(workspace: &Path) {
    let specs_dir = workspace.join("specs");
    std::fs::create_dir_all(&specs_dir).expect("mkdir specs");
    std::fs::write(
        specs_dir.join(format!("{SPEC_LABEL}.md")),
        "# walker pin\n\n## Success Criteria\n\n- a fixture criterion with no annotation\n",
    )
    .expect("write spec");
}

/// Walking the bd-shim invocation log, return whether at least one
/// `list` invocation carries `--label=spec:<SPEC_LABEL>`. The walker's
/// `build_rubric_prompt` issues this exact shape before any failure
/// point, so its presence pins that `mint_via_walker` was reached from
/// `run_gate_mint`.
fn invocation_log_records_spec_label_list(log: &str) -> bool {
    let want_label = format!("--label=spec:{SPEC_LABEL}");
    log.lines().any(|line| {
        let mut tokens = line.split_whitespace();
        tokens.next() == Some("list") && line.split_whitespace().any(|t| t == want_label)
    })
}

/// Spec contract: `specs/gate.md` § *Production walker wiring* —
/// `run_gate_mint` MUST dispatch through the production `MintWalker`
/// to obtain its `Vec<Finding>`, never via an unconditional
/// `Vec::<Finding>::new()` shortcut. A regression that constructed the
/// walker but bypassed its dispatch — including behind an always-false
/// guard — would leave the bd-shim invocation log empty.
#[test]
fn run_gate_mint_dispatches_through_production_walker_not_empty_vec() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = dir.path();
    git_init_workspace(workspace);
    write_fixture_spec(workspace);

    let (output, log_path) = run_gate_mint_subprocess(workspace);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let log = std::fs::read_to_string(&log_path).unwrap_or_default();

    assert!(
        !output.status.success(),
        "loom gate mint must exit non-zero — the empty profile manifest \
         causes the walker's manifest.lookup to fail, surfacing the \
         walker's runtime engagement. status={:?}\nstdout={stdout}\n\
         stderr={stderr}\nbd-invocations:\n{log}",
        output.status,
    );
    assert!(
        invocation_log_records_spec_label_list(&log),
        "production MintWalker.build_rubric_prompt must have issued a \
         `bd list --label=spec:{SPEC_LABEL}` call before the manifest \
         lookup failed; an empty invocation log proves run_gate_mint \
         bypassed mint_via_walker (e.g. via Vec::<Finding>::new()). \
         bd-invocations:\n{log}\nstderr:\n{stderr}",
    );
}

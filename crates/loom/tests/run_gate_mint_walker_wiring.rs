//! Live-path test for `run_gate_mint` dispatching through the production
//! `MintWalker`.
//!
//! Pins `specs/gate.md` § *Production walker wiring* criterion
//! `run_gate_mint_dispatches_tree_through_walker_and_molecule_through_promotion`:
//! the CLI arm must dispatch `--tree` through `mint::walk::walk` and
//! dispatch `-m/--molecule` through deferred promotion rather than a
//! fabricated empty finding vector.
//!
//! Invokes the compiled `loom` binary as a subprocess against a fixture
//! workspace whose profile manifest cannot resolve the default profile.
//! The walker's `run_rubric` issues `bd list` against the spec label
//! *before* the manifest lookup that fails — so a bd-shim invocation
//! log carrying a matching `list --json --label=spec:<X>` line proves
//! the production walker was reached from `run_gate_mint`. A regression
//! that bypassed the walker would leave the invocation log empty and
//! the command would exit zero with no findings minted, failing both
//! assertions below.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const SPEC_LABEL: &str = "walker-pin";

fn git_command() -> Command {
    let mut command = Command::new("git");
    loom_test_support::scrub_git_local_env(&mut command);
    command
}

/// Install the `bd-shim` test helper as `bd` on a fresh PATH dir. The
/// shim logs every invocation to `BD_STATE_DIR/.invocations.log`.
fn install_bd_shim(dir: &Path) -> PathBuf {
    let bin_dir = dir.join("bd-bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir bd-bin");
    let bd_path = bin_dir.join("bd");
    let source = PathBuf::from(env!("CARGO_BIN_EXE_bd-shim"));
    if matches!(std::os::unix::fs::symlink(&source, &bd_path), Ok(())) {
    } else {
        std::fs::copy(&source, &bd_path).expect("copy bd-shim");
        let mut perm = std::fs::metadata(&bd_path).expect("stat bd").permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&bd_path, perm).expect("chmod bd");
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

/// Run `loom --workspace <ws> gate mint --tree` with the bd shim on PATH
/// and an empty profile manifest. Returns the
/// subprocess output and the bd-shim invocation log path so callers can
/// inspect both.
fn run_gate_mint_tree_subprocess(workspace: &Path) -> (std::process::Output, PathBuf) {
    let bin_dir = install_bd_shim(workspace);
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).expect("mkdir bd-state");

    let manifest_path = workspace.join("profile-images.json");
    std::fs::write(&manifest_path, "{}").expect("write empty manifest");

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .arg("--host-key")
        .args(["gate", "mint", "--tree"])
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

/// Initialise the fixture workspace as a git repo with one commit so
/// `current_commit` resolves the same shape mint uses in production.
fn git_init_workspace(workspace: &Path) {
    let run = |args: &[&str]| {
        let ok = git_command()
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

fn write_bead(state_dir: &Path, id: &str, status: &str, issue_type: &str, labels: &[&str]) {
    let dir = state_dir.join(id);
    std::fs::create_dir_all(&dir).expect("mkdir bead");
    std::fs::write(dir.join("title"), id).expect("title");
    std::fs::write(dir.join("description"), "deferred evidence").expect("description");
    std::fs::write(dir.join("status"), status).expect("status");
    std::fs::write(dir.join("priority"), "2").expect("priority");
    std::fs::write(dir.join("issue_type"), issue_type).expect("issue_type");
    std::fs::write(dir.join("labels"), labels.join("\n")).expect("labels");
}

struct ShimRunner {
    state_dir: PathBuf,
    binary: PathBuf,
}

impl loom_driver::bd::CommandRunner for ShimRunner {
    async fn run(
        &self,
        args: Vec<std::ffi::OsString>,
        timeout: std::time::Duration,
    ) -> Result<loom_driver::bd::RunOutput, loom_driver::bd::BdError> {
        let mut command = tokio::process::Command::new(&self.binary);
        command
            .args(args)
            .env("BD_STATE_DIR", &self.state_dir)
            .env("BD_CREATE_ID", "lm-mol.1");
        let output = loom_driver::process::output(
            &mut command,
            &loom_driver::clock::SystemClock::new(),
            timeout,
        )
        .await
        .expect("isolated Beads shim completed");
        Ok(loom_driver::bd::RunOutput {
            status: output.status.code().unwrap(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

fn record_deferred_batch(workspace: &Path, state_dir: &Path, binary: PathBuf) {
    use loom_protocol::gate::{
        ConcernToken, DispatchScope, FindingRoute, FindingTarget, RawFinding,
    };
    let label = loom_driver::identifier::SpecLabel::new(SPEC_LABEL).unwrap();
    let finding = RawFinding {
        token: ConcernToken::SpecConventionsViolation,
        route: FindingRoute::Deferred,
        bonds: vec![label.clone()],
        target: FindingTarget::Criterion {
            spec: label,
            anchor: "success-criteria".into(),
        },
        evidence: "The fixture criterion lacks its verifier annotation.".into(),
    }
    .resolve(
        DispatchScope::Tree,
        &loom_workflow::review::WorkspaceFindingValidator::new(workspace),
    )
    .unwrap();
    let bd = loom_driver::bd::BdClient::with_runner(ShimRunner {
        state_dir: state_dir.to_path_buf(),
        binary,
    });
    let summary = tokio::runtime::Runtime::new().unwrap().block_on(
        loom_workflow::mint::route_molecule_findings(
            &bd,
            &loom_driver::identifier::MoleculeId::new("lm-mol").unwrap(),
            &[finding],
            &loom_workflow::mint::MintOptions::default(),
        ),
    );
    assert_eq!(summary.minted, 1, "{summary:?}");
    std::fs::write(state_dir.join(".invocations.log"), "").unwrap();
}

fn run_gate_mint_molecule_subprocess(workspace: &Path) -> (std::process::Output, PathBuf) {
    let bin_dir = install_bd_shim(workspace);
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).expect("mkdir bd-state");
    write_bead(&state_dir, "lm-mol", "open", "epic", &["spec:walker-pin"]);
    record_deferred_batch(workspace, &state_dir, bin_dir.join("bd"));

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .arg("--host-key")
        .args(["gate", "mint", "-m", "lm-mol"])
        .env("PATH", pinned_path(&bin_dir))
        .env("BD_STATE_DIR", &state_dir)
        .env("XDG_STATE_HOME", workspace.join(".loom-test-state"))
        .env_remove("LOOM_INSIDE")
        .output()
        .expect("spawn loom");
    (output, state_dir.join(".invocations.log"))
}

/// Spec contract: `specs/gate.md` § *Production walker wiring* —
/// `run_gate_mint` dispatches `--tree` through the production
/// `MintWalker` and `-m/--molecule` through deferred promotion.
#[test]
fn run_gate_mint_dispatches_tree_through_walker_and_molecule_through_promotion() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = dir.path();
    git_init_workspace(workspace);
    write_fixture_spec(workspace);

    let (output, log_path) = run_gate_mint_tree_subprocess(workspace);
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
         bypassed the production walker (e.g. via Vec::<Finding>::new()). \
         bd-invocations:\n{log}\nstderr:\n{stderr}",
    );
    assert!(
        stdout.contains("errors 1") && stdout.contains("walk:rubric:"),
        "tree mint must preserve the walker failure in the mint summary \
         instead of aborting before summary rendering. stdout={stdout}\nstderr={stderr}",
    );

    let molecule_workspace = tempfile::tempdir().expect("molecule tempdir");
    git_init_workspace(molecule_workspace.path());
    write_fixture_spec(molecule_workspace.path());
    let (molecule_output, molecule_log_path) =
        run_gate_mint_molecule_subprocess(molecule_workspace.path());
    let molecule_stdout = String::from_utf8_lossy(&molecule_output.stdout);
    let molecule_stderr = String::from_utf8_lossy(&molecule_output.stderr);
    let molecule_log = std::fs::read_to_string(&molecule_log_path).unwrap_or_default();

    assert!(
        molecule_output.status.success(),
        "molecule promotion succeeds without constructing or walking findings. \
         status={:?}\nstdout={molecule_stdout}\nstderr={molecule_stderr}\nlog:\n{molecule_log}",
        molecule_output.status,
    );
    assert!(
        molecule_stdout.contains("promoted 1 deferred"),
        "molecule summary names promoted deferred count: {molecule_stdout}",
    );
    assert!(
        molecule_log.lines().any(|line| {
            line.starts_with(
                "update lm-mol.1 --status open --remove-label loom:deferred --set-metadata ",
            ) && line.contains("loom.acceptance=")
        }),
        "molecule promotion must update the existing deferred bead: {molecule_log}",
    );
    assert!(
        molecule_log
            .lines()
            .all(|line| !line.starts_with("create ")),
        "molecule promotion must not mint a placeholder finding batch: {molecule_log}",
    );
}

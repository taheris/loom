//! Marker CLI and wrapper composition use Cargo's actual Loom binary, never nested builds.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Command, Output};

use anyhow::Context as _;
use loom_gate::gate_outcome::{
    GateRun, GateSuccess, HandoffEvidence, HookCoverage, MoleculeState,
    append_gate_run_lifecycle_events, parse_gate_runs_from_jsonl,
};
use loom_gate::marker::{MARKER_PATH, MarkerProof};

const HOOK_ID: &str = "loom-gate-verify-diff";
const HOOK_ENTRY: &str = "loom gate verify --diff @{u}..HEAD";
const PUSH_RANGE: &str = "origin/main..HEAD";

fn workspace() -> anyhow::Result<tempfile::TempDir> {
    let dir = tempfile::tempdir()?;
    loom_driver::git::init_test_repo(dir.path())?;
    std::fs::write(dir.path().join(".pre-commit-config.yaml"), "repos: []\n")?;
    install_wrapper(dir.path())?;
    loom_driver::git::commit_all_in(dir.path(), "Add marker CLI fixture")?;
    Ok(dir)
}

fn install_wrapper(root: &Path) -> anyhow::Result<()> {
    install(
        root,
        "bin/pre-push-checks",
        include_str!("../../../bin/pre-push-checks"),
    )
}

fn install(root: &Path, name: &str, body: &str) -> anyhow::Result<()> {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().context("fixture script parent")?)?;
    std::fs::write(&path, body)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

fn mint(root: &Path) -> anyhow::Result<tempfile::NamedTempFile> {
    let log = tempfile::NamedTempFile::new()?;
    let tree = loom_driver::git::head_tree_oid_sync(root)?.to_string();
    let config = blake3::hash(&std::fs::read(root.join(".pre-commit-config.yaml"))?)
        .to_hex()
        .to_string();
    let verify = GateRun::successful_verify(
        PUSH_RANGE.to_owned(),
        tree.clone(),
        config.clone(),
        log.path().to_path_buf(),
        vec![HookCoverage {
            id: HOOK_ID.to_owned(),
            entry: HOOK_ENTRY.to_owned(),
        }],
    );
    append_gate_run_lifecycle_events(log.path(), &verify)?;
    let review = GateRun::successful_review(
        PUSH_RANGE.to_owned(),
        tree,
        config,
        log.path().to_path_buf(),
        loom_protocol::gate::ExitSignal::Complete,
    );
    append_gate_run_lifecycle_events(log.path(), &review)?;
    let mut evidence = HandoffEvidence::from_runs(parse_gate_runs_from_jsonl(log.path()));
    evidence.molecule_state = MoleculeState::Clean;
    let success = GateSuccess::new(&evidence, 1)
        .map_err(|error| anyhow::anyhow!("marker evidence: {error:?}"))?;
    MarkerProof::mint(success, root, &loom_driver::clock::SystemClock::new())?;
    Ok(log)
}

fn verify(root: &Path) -> anyhow::Result<Output> {
    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["gate", "verify-marker"])
        .current_dir(root)
        .output()
        .context("execute actual marker CLI")?;
    assert_no_fixture_compilation()?;
    Ok(output)
}

fn assert_no_fixture_compilation() -> anyhow::Result<()> {
    if std::env::var_os("LOOM_TEST_STORAGE").is_some() {
        let state = std::env::var_os("XDG_STATE_HOME").context("fixture state")?;
        assert!(
            !Path::new(&state)
                .join("loom/trusted-cargo/state.db")
                .exists()
        );
        let temporary = std::env::var_os("TMPDIR").context("fixture temporary root")?;
        assert!(!Path::new(&temporary).join("cargo-target").exists());
    }
    Ok(())
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_rejected(output: &Output, expected: &str) {
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(expected),
        "{output:?}"
    );
}

#[test]
fn verify_marker_exits_zero_on_match() -> anyhow::Result<()> {
    let dir = workspace()?;
    let _log = mint(dir.path())?;
    assert_success(&verify(dir.path())?);
    Ok(())
}

#[test]
fn verify_marker_exits_nonzero_on_missing() -> anyhow::Result<()> {
    let dir = workspace()?;
    assert_rejected(&verify(dir.path())?, "marker file not present");
    Ok(())
}

#[test]
fn verify_marker_exits_nonzero_on_tree_mismatch() -> anyhow::Result<()> {
    let dir = workspace()?;
    let _log = mint(dir.path())?;
    std::fs::write(dir.path().join("README.md"), "new committed tree\n")?;
    loom_driver::git::commit_all_in(dir.path(), "Advance tree after mint")?;
    assert_rejected(&verify(dir.path())?, "does not match marker");
    Ok(())
}

#[test]
fn verify_marker_exits_nonzero_on_dirty_tree() -> anyhow::Result<()> {
    let dir = workspace()?;
    let _log = mint(dir.path())?;
    std::fs::write(dir.path().join("README.md"), "dirty contents\n")?;
    assert_rejected(&verify(dir.path())?, "workspace porcelain is not clean");
    Ok(())
}

fn wrapper(root: &Path, tools: &Path) -> anyhow::Result<Output> {
    wrapper_for(root, tools, [HOOK_ID, HOOK_ENTRY, PUSH_RANGE])
}

fn wrapper_for(root: &Path, tools: &Path, request: [&str; 3]) -> anyhow::Result<Output> {
    let mut paths = vec![tools.to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").context("fixture PATH")?,
    ));
    install(
        tools,
        "loom",
        &format!(
            "#!/usr/bin/env bash\nset -euo pipefail\nexec '{}' \"$@\"\n",
            env!("CARGO_BIN_EXE_loom").replace('\'', "'\\''"),
        ),
    )?;
    install(
        tools,
        "sentinel",
        &format!(
            "#!/bin/sh\nset -eu\ntouch '{}'\n",
            tools
                .join("sentinel.flag")
                .display()
                .to_string()
                .replace('\'', "'\\''"),
        ),
    )?;
    install_wrapper(root)?;
    let output = Command::new("bash")
        .arg("bin/pre-push-checks")
        .args([
            "--hook-id",
            request[0],
            "--hook-entry",
            request[1],
            "--push-range",
            request[2],
            "--",
            "sentinel",
        ])
        .current_dir(root)
        .env("PATH", std::env::join_paths(paths)?)
        .output()
        .context("execute actual pre-push wrapper")?;
    assert_no_fixture_compilation()?;
    Ok(output)
}

#[test]
fn pre_push_checks_short_circuits_only_on_covered_marker() -> anyhow::Result<()> {
    let dir = workspace()?;
    let _log = mint(dir.path())?;
    let tools = tempfile::tempdir()?;
    assert_success(&wrapper(dir.path(), tools.path())?);
    let sentinel = tools.path().join("sentinel.flag");
    assert!(!sentinel.exists(), "covered marker must short-circuit");
    for request in [
        ["other-hook", HOOK_ENTRY, PUSH_RANGE],
        [HOOK_ID, "loom gate verify --tree", PUSH_RANGE],
        [HOOK_ID, HOOK_ENTRY, "origin/other..HEAD"],
    ] {
        assert_success(&wrapper_for(dir.path(), tools.path(), request)?);
        assert!(
            sentinel.exists(),
            "uncovered hook, entry or range must fall through"
        );
        std::fs::remove_file(&sentinel)?;
    }
    Ok(())
}

#[test]
fn pre_push_checks_falls_through_on_uncovered_or_invalid_marker() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    loom_driver::state::CacheDb::open(loom_driver::state::path(dir.path()))?.put_record(
        loom_driver::state::RecordKind::PublicationReceipt,
        dir.path().to_str().context("UTF-8 fixture path")?,
        &serde_json::json!({}),
    )?;
    let tools = tempfile::tempdir()?;
    assert_success(&wrapper(dir.path(), tools.path())?);
    assert!(
        tools.path().join("sentinel.flag").exists(),
        "invalid marker must fall through"
    );
    Ok(())
}

#[test]
fn mint_and_pre_push_checks_short_circuit_joint_path() -> anyhow::Result<()> {
    let dir = workspace()?;
    let _log = mint(dir.path())?;
    assert!(
        dir.path().join(MARKER_PATH).exists(),
        "mint must write the canonical receipt"
    );
    let tools = tempfile::tempdir()?;
    assert_success(&wrapper(dir.path(), tools.path())?);
    assert!(
        !tools.path().join("sentinel.flag").exists(),
        "mint and wrapper must share the receipt path"
    );
    Ok(())
}

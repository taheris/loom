//! Publication receipts cross the real CLI/hook boundary using Cargo's prebuilt binary.

#![allow(
    clippy::unwrap_used,
    reason = "disposable CLI fixture setup failures must fail these integration tests"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use loom_driver::git;
use loom_driver::state::{CacheDb, RecordKind};
use loom_gate::gate_outcome::{
    self, GateRun, GateSuccess, HandoffEvidence, HookCoverage, MoleculeState,
};
use loom_gate::marker::{MARKER_PATH, MarkerProof};

const HOOK_ID: &str = "loom-gate-verify-diff";
const HOOK_ENTRY: &str = "loom gate verify --diff @{u}..HEAD";
const PUSH_RANGE: &str = "origin/main..HEAD";

struct Fixture {
    root: tempfile::TempDir,
    workspace: PathBuf,
    sentinel: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        git::init_test_repo(&workspace).unwrap();
        fs::write(workspace.join(".pre-commit-config.yaml"), "repos: []\n").unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bin/pre-push-checks");
        fs::create_dir(workspace.join("bin")).unwrap();
        fs::copy(source, workspace.join("bin/pre-push-checks")).unwrap();
        git::commit_all_in(&workspace, "Add publication hook").unwrap();
        let tools = root.path().join("tools");
        fs::create_dir(&tools).unwrap();
        let sentinel = root.path().join("sentinel.flag");
        executable(
            &tools.join("loom"),
            &format!(
                "exec {} \"$@\"",
                quoted(Path::new(env!("CARGO_BIN_EXE_loom")))
            ),
        );
        executable(
            &tools.join("sentinel"),
            &format!("touch {}", quoted(&sentinel)),
        );
        Self {
            root,
            workspace,
            sentinel,
        }
    }

    fn mint(&self) {
        let log = self.workspace.join(".loom/logs/gate/marker-test.jsonl");
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        let tree = git::head_tree_oid_sync(&self.workspace)
            .unwrap()
            .to_string();
        let digest =
            blake3::hash(&fs::read(self.workspace.join(".pre-commit-config.yaml")).unwrap())
                .to_hex()
                .to_string();
        let verify = GateRun::successful_verify(
            PUSH_RANGE.into(),
            tree.clone(),
            digest.clone(),
            log.clone(),
            vec![HookCoverage {
                id: HOOK_ID.into(),
                entry: HOOK_ENTRY.into(),
            }],
        );
        gate_outcome::append_gate_run_lifecycle_events(&log, &verify).unwrap();
        let review = GateRun::successful_review(
            PUSH_RANGE.into(),
            tree,
            digest,
            log.clone(),
            loom_protocol::gate::ExitSignal::Complete,
        );
        gate_outcome::append_gate_run_lifecycle_events(&log, &review).unwrap();
        let mut evidence =
            HandoffEvidence::from_runs(gate_outcome::parse_gate_runs_from_jsonl(&log));
        evidence.molecule_state = MoleculeState::Clean;
        MarkerProof::mint(
            GateSuccess::new(&evidence, 1).unwrap(),
            &self.workspace,
            &loom_driver::clock::SystemClock,
        )
        .unwrap();
    }

    fn mutate_receipt(&self, mutate: impl FnOnce(&mut serde_json::Value)) {
        let db = CacheDb::open(loom_driver::state::path(&self.workspace)).unwrap();
        let (key, mut value) = db
            .records::<serde_json::Value>(RecordKind::PublicationReceipt)
            .unwrap()
            .pop()
            .unwrap();
        mutate(&mut value);
        db.put_record(RecordKind::PublicationReceipt, &key, &value)
            .unwrap();
    }

    fn verify(&self) -> Output {
        let output = Command::new(env!("CARGO_BIN_EXE_loom"))
            .args(["gate", "verify-marker"])
            .current_dir(&self.workspace)
            .output()
            .unwrap();
        assert_no_fixture_compilation();
        output
    }

    fn hook(&self) -> Output {
        let mut path = vec![self.root.path().join("tools")];
        path.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let output = Command::new("bash")
            .arg(self.workspace.join("bin/pre-push-checks"))
            .args([
                "--hook-id",
                HOOK_ID,
                "--hook-entry",
                HOOK_ENTRY,
                "--push-range",
                PUSH_RANGE,
                "--",
                "sentinel",
            ])
            .current_dir(&self.workspace)
            .env("PATH", std::env::join_paths(path).unwrap())
            .output()
            .unwrap();
        assert_no_fixture_compilation();
        output
    }
}

fn quoted(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn executable(path: &Path, body: &str) {
    fs::write(
        path,
        format!("#!/usr/bin/env bash\nset -euo pipefail\n{body}\n"),
    )
    .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn assert_no_fixture_compilation() {
    if std::env::var_os("LOOM_TEST_STORAGE").is_some() {
        let state = PathBuf::from(std::env::var_os("XDG_STATE_HOME").unwrap());
        assert!(!state.join("loom/trusted-cargo/state.db").exists());
        let temporary = PathBuf::from(std::env::var_os("TMPDIR").unwrap());
        assert!(!temporary.join("cargo-target").exists());
    }
}

fn assert_cli_failed_with(output: &Output, expected: &str) {
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(expected),
        "{output:?}"
    );
}

#[test]
fn verify_marker_exits_zero_on_match() {
    let fixture = Fixture::new();
    fixture.mint();
    for _ in 0..2 {
        let output = fixture.verify();
        assert!(output.status.success(), "{output:?}");
    }
}

#[test]
fn verify_marker_exits_nonzero_on_missing() {
    let fixture = Fixture::new();
    assert_cli_failed_with(&fixture.verify(), "marker file not present");
}

#[test]
fn verify_marker_exits_nonzero_on_tree_mismatch() {
    let fixture = Fixture::new();
    fixture.mint();
    fixture.mutate_receipt(|value| {
        value["tree_oid"] = serde_json::json!("deadbeefcafe1234567890abcdef0123456789ab");
    });
    assert_cli_failed_with(&fixture.verify(), "does not match marker");
}

#[test]
fn verify_marker_exits_nonzero_on_dirty_tree() {
    let fixture = Fixture::new();
    fixture.mint();
    fs::write(fixture.workspace.join("README.md"), "dirty contents\n").unwrap();
    assert_cli_failed_with(&fixture.verify(), "workspace porcelain is not clean");
}

#[test]
fn pre_push_checks_short_circuits_only_on_covered_marker() {
    let fixture = Fixture::new();
    fixture.mint();
    for _ in 0..2 {
        let output = fixture.hook();
        assert!(output.status.success(), "{output:?}");
        assert!(!fixture.sentinel.exists());
    }
}

#[test]
fn pre_push_checks_falls_through_on_uncovered_or_invalid_marker() {
    let fixture = Fixture::new();
    fixture.mint();
    fixture.mutate_receipt(|value| *value = serde_json::json!({}));
    let output = fixture.hook();
    assert!(output.status.success(), "{output:?}");
    assert!(fixture.sentinel.exists());
}

#[test]
fn mint_and_pre_push_checks_short_circuit_joint_path() {
    let fixture = Fixture::new();
    fixture.mint();
    assert!(fixture.workspace.join(MARKER_PATH).exists());
    let output = fixture.hook();
    assert!(output.status.success(), "{output:?}");
    assert!(!fixture.sentinel.exists());
}

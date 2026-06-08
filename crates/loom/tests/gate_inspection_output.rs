//! Live-path tests for inspection-only gate finding output.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use loom_driver::identifier::SpecLabel;
use loom_workflow::review::{ConcernToken, Finding, FindingTarget};

const SPEC_LABEL: &str = "acme";
const FINDING_AGENT_MODE: &str = "finding-concern";
const COMPLETE_AGENT_MODE: &str = "complete-marker";

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

fn pinned_path(bin_dir: &Path) -> std::ffi::OsString {
    let ambient = std::env::var_os("PATH").unwrap_or_default();
    let mut entries: Vec<PathBuf> = vec![bin_dir.to_path_buf()];
    entries.extend(std::env::split_paths(&ambient));
    std::env::join_paths(entries).expect("join PATH")
}

fn write_profile_manifest(workspace: &Path) -> PathBuf {
    let manifest_path = workspace.join("profile-images.json");
    std::fs::write(
        &manifest_path,
        r#"{
          "base": { "ref": "localhost/wrix-base:test", "source": "/nix/store/aaa-image-base" }
        }"#,
    )
    .expect("write profile manifest");
    manifest_path
}

fn write_specs(workspace: &Path, labels: &[&str]) {
    let specs_dir = workspace.join("specs");
    std::fs::create_dir_all(&specs_dir).expect("mkdir specs");
    for label in labels {
        let title = label.replace('-', " ");
        std::fs::write(
            specs_dir.join(format!("{label}.md")),
            format!("# {title}\n"),
        )
        .expect("write spec");
    }
}

fn run_gate_command_with_agent(
    workspace: &Path,
    args: &[&str],
    labels: &[&str],
    agent_mode: &str,
) -> (std::process::Output, String) {
    write_specs(workspace, labels);
    let bin_dir = install_bd_shim(workspace);
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).expect("mkdir bd-state");
    let manifest = write_profile_manifest(workspace);
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let mock_agent = env!("CARGO_BIN_EXE_mock-loom-agent");

    let output = Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .arg("--agent")
        .arg("pi")
        .args(args)
        .env("PATH", pinned_path(&bin_dir))
        .env("LOOM_WRIX_BIN", mock_agent)
        .env("LOOM_TEST_AGENT_MODE", agent_mode)
        .env("LOOM_BIN", loom_bin)
        .env("LOOM_PROFILES_MANIFEST", manifest)
        .env("BD_STATE_DIR", &state_dir)
        .env("XDG_STATE_HOME", workspace.join(".loom-test-state"))
        .env_remove("LOOM_INSIDE")
        .output()
        .expect("spawn loom");
    let log = std::fs::read_to_string(state_dir.join(".invocations.log")).unwrap_or_default();
    (output, log)
}

fn run_gate_command(workspace: &Path, args: &[&str]) -> (std::process::Output, String) {
    run_gate_command_with_agent(workspace, args, &[SPEC_LABEL], FINDING_AGENT_MODE)
}

fn expected_finding() -> Finding {
    Finding {
        token: ConcernToken::SpecCoherenceFail,
        route: loom_workflow::review::FindingRoute::Deferred,
        bonds: vec![SpecLabel::new(SPEC_LABEL)],
        target: FindingTarget::Criterion {
            spec: SpecLabel::new(SPEC_LABEL),
            anchor: "finding-status-output".to_owned(),
        },
        evidence: "status output fixture".to_owned(),
    }
}

fn status_payload(stdout: &str) -> serde_json::Value {
    let payload = stdout
        .lines()
        .find_map(|line| line.strip_prefix("LOOM_FINDING_STATUS:"))
        .expect("finding status line")
        .trim();
    serde_json::from_str(payload).expect("status json")
}

#[test]
fn audit_tree_scope_makes_no_bd_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = dir.path();
    let (output, log) = run_gate_command(workspace, &["gate", "audit", "--tree"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "loom gate audit --tree must complete inspection without bd writes. status={:?}\nstdout={stdout}\nstderr={stderr}\nbd log:\n{log}",
        output.status,
    );
    assert!(
        !log.lines().any(|line| {
            let command = line.split_whitespace().next();
            matches!(command, Some("create" | "update" | "close" | "dep"))
        }),
        "inspection-only audit must not issue bd write commands. bd log:\n{log}",
    );
    assert!(
        stdout.contains("LOOM_FINDING_STATUS:"),
        "audit should still report finding status JSON. stdout:\n{stdout}",
    );
}

#[test]
fn mint_tree_without_spec_filter_walks_every_workspace_spec() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = dir.path();
    let (output, log) = run_gate_command_with_agent(
        workspace,
        &["gate", "mint", "--tree", "--dry-run"],
        &["alpha", "beta"],
        COMPLETE_AGENT_MODE,
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "loom gate mint --tree must complete the workspace sweep. status={:?}\nstdout={stdout}\nstderr={stderr}\nbd log:\n{log}",
        output.status,
    );
    for label in ["spec:alpha", "spec:beta"] {
        assert!(
            log.lines()
                .any(|line| line.contains("list") && line.contains(label)),
            "mint --tree must build a rubric prompt for {label}. bd log:\n{log}",
        );
    }
}

#[test]
fn driver_emits_finding_status_json_with_identity_and_action() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = dir.path();
    let (output, log) = run_gate_command(workspace, &["gate", "review", "--tree"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let finding = expected_finding();
    let json = status_payload(&stdout);

    assert!(
        output.status.success(),
        "loom gate review --tree must surface reported finding statuses. status={:?}\nstdout={stdout}\nstderr={stderr}\nbd log:\n{log}",
        output.status,
    );
    assert_eq!(json["id"], finding.id());
    assert_eq!(json["hash"], finding.hash());
    assert_eq!(json["label"], format!("finding:{}", finding.hash()));
    assert_eq!(json["token"], "spec-coherence-fail");
    assert_eq!(json["target"]["kind"], "Criterion");
    assert_eq!(json["target"]["spec"], SPEC_LABEL);
    assert_eq!(json["target"]["anchor"], "finding-status-output");
    assert_eq!(json["action"], "reported");
}

#[test]
fn rubric_tree_scope_emits_reported_finding_status() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = dir.path();
    let (output, log) = run_gate_command(workspace, &["gate", "rubric", "--tree"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let json = status_payload(&stdout);

    assert!(
        output.status.success(),
        "loom gate rubric --tree must surface reported finding statuses. status={:?}\nstdout={stdout}\nstderr={stderr}\nbd log:\n{log}",
        output.status,
    );
    assert_eq!(json["action"], "reported");
    assert_eq!(json["target"]["anchor"], "finding-status-output");
}

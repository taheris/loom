//! Live-path tests for gate finding output and mint progress.

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
          "base": { "pi": { "ref": "localhost/wrix-base-pi:test", "source": "/nix/store/aaa-image-base-pi", "source_kind": "nix-descriptor" }, "claude": { "ref": "localhost/wrix-base-claude:test", "source": "/nix/store/aaa-image-base-claude", "source_kind": "nix-descriptor" }, "direct": { "ref": "localhost/wrix-base-direct:test", "source": "/nix/store/aaa-image-base-direct", "source_kind": "nix-descriptor" } }
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
            format!("# {title}\n\n## Success Criteria\n\n- Finding status output\n"),
        )
        .expect("write spec");
    }
}

fn run_gate_command_with_agent_and_setup<F>(
    workspace: &Path,
    args: &[&str],
    labels: &[&str],
    agent_mode: &str,
    setup: F,
) -> (std::process::Output, String, PathBuf)
where
    F: FnOnce(&Path),
{
    write_specs(workspace, labels);
    let bin_dir = install_bd_shim(workspace);
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).expect("mkdir bd-state");
    setup(&state_dir);
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
        .env_remove("LOOM_WRIX_SPAWN_BIN")
        .env("LOOM_TEST_AGENT_MODE", agent_mode)
        .env("LOOM_BIN", loom_bin)
        .env("LOOM_PROFILES_MANIFEST", manifest)
        .env("BD_STATE_DIR", &state_dir)
        .env("XDG_STATE_HOME", workspace.join(".loom-test-state"))
        .env_remove("LOOM_INSIDE")
        .output()
        .expect("spawn loom");
    let log = std::fs::read_to_string(state_dir.join(".invocations.log")).unwrap_or_default();
    (output, log, state_dir)
}

fn run_gate_command_with_agent(
    workspace: &Path,
    args: &[&str],
    labels: &[&str],
    agent_mode: &str,
) -> (std::process::Output, String) {
    let (output, log, _) =
        run_gate_command_with_agent_and_setup(workspace, args, labels, agent_mode, |_| {});
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

fn write_bead(state_dir: &Path, id: &str, status: &str, issue_type: &str, labels: &[&str]) {
    let dir = state_dir.join(id);
    std::fs::create_dir_all(&dir).expect("mkdir bead");
    std::fs::write(dir.join("title"), id).expect("title");
    std::fs::write(dir.join("description"), "fixture").expect("description");
    std::fs::write(dir.join("status"), status).expect("status");
    std::fs::write(dir.join("priority"), "2").expect("priority");
    std::fs::write(dir.join("issue_type"), issue_type).expect("issue_type");
    std::fs::write(dir.join("labels"), labels.join("\n")).expect("labels");
}

fn latest_gate_mint_log(workspace: &Path) -> PathBuf {
    let gate_dir = workspace.join(".loom/logs/gate");
    let mut entries = std::fs::read_dir(&gate_dir)
        .unwrap_or_else(|err| panic!("read gate log dir {}: {err}", gate_dir.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("mint-") && name.ends_with(".jsonl"))
        })
        .collect::<Vec<_>>();
    entries.sort();
    entries
        .pop()
        .unwrap_or_else(|| panic!("missing gate mint log in {}", gate_dir.display()))
}

fn read_jsonl(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read jsonl {}: {err}", path.display()))
        .lines()
        .map(|line| serde_json::from_str(line).expect("event json"))
        .collect()
}

fn state_contains_label(state_dir: &Path, label: &str) -> bool {
    std::fs::read_dir(state_dir)
        .expect("read state dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("labels"))
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .any(|labels| labels.lines().any(|candidate| candidate == label))
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
fn gate_mint_tree_streams_progress_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = dir.path();
    let finding = expected_finding();
    let finding_label = format!("finding:{}", finding.hash());
    let (output, bd_log, state_dir) = run_gate_command_with_agent_and_setup(
        workspace,
        &["gate", "mint", "--tree"],
        &[SPEC_LABEL],
        FINDING_AGENT_MODE,
        |state_dir| {
            write_bead(
                state_dir,
                "lm-specmeta",
                "closed",
                "epic",
                &["loom:spec", "spec:acme"],
            )
        },
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "loom gate mint --tree must mint from parsed findings while streaming progress. status={:?}\nstdout={stdout}\nstderr={stderr}\nbd log:\n{bd_log}",
        output.status,
    );
    for needle in [
        "gate_run_start: mint tree run started",
        "gate_run_lane: verifier run started",
        "gate_run_lane: rubric walk started for spec:acme",
        "gate_run_lane: minting decision minted",
        "gate_run_end: mint tree run finished",
    ] {
        assert!(
            stdout.contains(needle),
            "stdout missing {needle:?}:\n{stdout}"
        );
    }
    assert!(
        stdout.contains("minted 1 batches"),
        "final mint summary must still report minted findings: {stdout}",
    );
    assert!(
        state_contains_label(&state_dir, &finding_label),
        "mint must materialize the parsed finding label {finding_label}; bd log:\n{bd_log}",
    );

    let events = read_jsonl(&latest_gate_mint_log(workspace));
    assert!(
        events.iter().all(|event| event["kind"] == "driver_event"),
        "gate mint progress log must use driver_event rows only: {events:#?}",
    );
    assert!(events.iter().any(|event| {
        event["driver_kind"] == "gate_run_lane"
            && event["payload"]["stage"] == "rubric"
            && event["payload"]["action"] == "end"
            && event["payload"]["parsed_findings"] == 1
    }));
    assert!(events.iter().any(|event| {
        event["driver_kind"] == "gate_run_lane"
            && event["payload"]["action"] == "finding-status"
            && event["payload"]["status"]["action"] == "minted"
            && event["payload"]["status"]["label"] == finding_label
    }));
    assert!(events.iter().any(|event| {
        event["driver_kind"] == "gate_run_end"
            && event["payload"]["counts"]["minted"] == 1
            && event["payload"]["counts"]["exit_code"] == 0
    }));
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

//! End-to-end `loom inbox` list/view surface tests against the real `bd`
//! subprocess path through the test shim.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const CLARIFY_DESC: &str = "## Options — pick a path

### Option 1 — Choose A
Detail A

### Option 2 — Choose B
Detail B
";

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

fn seed_notes(state_dir: &Path, id: &str, notes: &str) {
    std::fs::write(state_dir.join(id).join("notes"), notes).expect("write notes");
}

fn install_bd_shim(dir: &Path) -> PathBuf {
    let bin_dir = dir.join("bd-bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir bd-bin");
    let bd_path = bin_dir.join("bd");
    let source = PathBuf::from(env!("CARGO_BIN_EXE_bd-shim"));
    match std::os::unix::fs::symlink(&source, &bd_path) {
        Ok(()) => {}
        Err(_) => {
            std::fs::copy(&source, &bd_path).expect("copy bd-shim into bin dir");
            let mut perm = std::fs::metadata(&bd_path).expect("stat bd").permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&bd_path, perm).expect("chmod bd");
        }
    }
    bin_dir
}

fn run_loom_inbox(
    workspace: &Path,
    bin_dir: &Path,
    state_dir: &Path,
    args: &[&str],
) -> std::process::Output {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let mut entries: Vec<PathBuf> = vec![bin_dir.to_path_buf()];
    entries.extend(std::env::split_paths(&path_var));
    let new_path = std::env::join_paths(entries).expect("join PATH");

    Command::new(env!("CARGO_BIN_EXE_loom"))
        .arg("--workspace")
        .arg(workspace)
        .arg("inbox")
        .args(args)
        .env("PATH", new_path)
        .env("BD_STATE_DIR", state_dir)
        .env("XDG_STATE_HOME", workspace.join(".loom-state"))
        .env_remove("LOOM_INSIDE")
        .output()
        .expect("spawn loom")
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout utf-8")
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr utf-8")
}

#[test]
fn inbox_bare_prints_help_and_list_prints_items() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let bin_dir = install_bd_shim(workspace);

    seed_bead(
        &state_dir,
        "lm-open",
        "open clarify",
        CLARIFY_DESC,
        "open",
        &["loom:clarify", "spec:agent"],
    );

    let bare = run_loom_inbox(workspace, &bin_dir, &state_dir, &[]);
    assert!(bare.status.success(), "stderr={}", stderr(&bare));
    let out = stdout(&bare);
    assert!(out.contains("Usage: loom inbox"), "{out}");
    assert!(
        !out.contains("lm-open"),
        "bare inbox should not list items: {out}"
    );

    let list = run_loom_inbox(workspace, &bin_dir, &state_dir, &["list"]);
    assert!(list.status.success(), "stderr={}", stderr(&list));
    assert!(stdout(&list).contains("lm-open"));
}

#[test]
fn inbox_list_excludes_closed_blocked_or_clarify_beads() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let bin_dir = install_bd_shim(workspace);

    seed_bead(
        &state_dir,
        "lm-closed",
        "closed clarify",
        CLARIFY_DESC,
        "closed",
        &["loom:clarify", "spec:agent"],
    );
    seed_bead(
        &state_dir,
        "lm-open",
        "open clarify",
        CLARIFY_DESC,
        "open",
        &["loom:clarify", "spec:agent"],
    );
    seed_bead(
        &state_dir,
        "lm-tune",
        "tune proposal",
        "Tune body",
        "open",
        &["loom:tune", "spec:skills"],
    );
    seed_metadata(
        &state_dir,
        "lm-tune",
        serde_json::json!({"loom.tune.state":"pending","loom.tune.id":"lm-tune"}),
    );

    let output = run_loom_inbox(workspace, &bin_dir, &state_dir, &["list"]);
    assert!(output.status.success(), "stderr={}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains("lm-open"), "{out}");
    assert!(out.contains("lm-tune"), "{out}");
    assert!(!out.contains("lm-closed"), "{out}");
}

#[test]
fn inbox_spec_filter_narrows_list_to_matching_spec() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let bin_dir = install_bd_shim(workspace);

    seed_bead(
        &state_dir,
        "lm-alpha",
        "alpha",
        CLARIFY_DESC,
        "open",
        &["loom:clarify", "spec:alpha"],
    );
    seed_bead(
        &state_dir,
        "lm-beta",
        "beta",
        CLARIFY_DESC,
        "open",
        &["loom:blocked", "spec:beta"],
    );

    let output = run_loom_inbox(workspace, &bin_dir, &state_dir, &["list", "-s", "alpha"]);
    assert!(output.status.success(), "stderr={}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains("lm-alpha"), "{out}");
    assert!(!out.contains("lm-beta"), "{out}");
    assert!(
        !out.contains("[spec:"),
        "filtered list drops repeated spec column: {out}"
    );
}

#[test]
fn inbox_kind_filter_narrows_list() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let bin_dir = install_bd_shim(workspace);

    seed_bead(
        &state_dir,
        "lm-a",
        "blocked",
        "blocked",
        "open",
        &["loom:blocked"],
    );
    seed_bead(
        &state_dir,
        "lm-b",
        "clarify",
        CLARIFY_DESC,
        "open",
        &["loom:clarify"],
    );
    seed_bead(
        &state_dir,
        "lm-c",
        "tune",
        "tune",
        "blocked",
        &["loom:tune", "loom:blocked"],
    );
    seed_metadata(
        &state_dir,
        "lm-c",
        serde_json::json!({"loom.tune.state":"apply_failed"}),
    );

    let all = run_loom_inbox(workspace, &bin_dir, &state_dir, &["list"]);
    assert!(all.status.success(), "stderr={}", stderr(&all));
    let out = stdout(&all);
    let clarify_pos = out.find("lm-b").expect("clarify row");
    let blocked_pos = out.find("lm-a").expect("blocked row");
    let tune_pos = out.find("lm-c").expect("tune row");
    assert!(clarify_pos < blocked_pos && blocked_pos < tune_pos, "{out}");

    let blocked = run_loom_inbox(workspace, &bin_dir, &state_dir, &["list", "-k", "blocked"]);
    assert!(blocked.status.success(), "stderr={}", stderr(&blocked));
    let out = stdout(&blocked);
    assert!(out.contains("  1. lm-a [blocked]"), "{out}");
    assert!(!out.contains("lm-b"), "{out}");
    assert!(!out.contains("lm-c"), "{out}");
}

#[test]
fn inbox_view_modes_render_host_side() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let bin_dir = install_bd_shim(workspace);

    seed_bead(
        &state_dir,
        "lm-view",
        "view me",
        "body without options",
        "open",
        &["loom:clarify", "spec:agent"],
    );
    seed_notes(
        &state_dir,
        "lm-view",
        "## Options — from notes\n\n### Option 1 — note option\nNote body\n",
    );
    seed_bead(
        &state_dir,
        "lm-prop",
        "proposal",
        "Proposal report body",
        "open",
        &["loom:tune", "spec:skills"],
    );
    seed_metadata(
        &state_dir,
        "lm-prop",
        serde_json::json!({
            "loom.tune.state":"pending",
            "loom.tune.proposal_branch":"loom/tune/lm-prop",
            "loom.tune.proposal_head":"abc123"
        }),
    );
    std::fs::create_dir_all(workspace.join(".loom/tune/lm-prop/repo")).unwrap();

    let by_bead = run_loom_inbox(workspace, &bin_dir, &state_dir, &["view", "-b", "lm-view"]);
    assert!(by_bead.status.success(), "stderr={}", stderr(&by_bead));
    let out = stdout(&by_bead);
    assert!(out.contains("inbox item lm-view [clarify]"), "{out}");
    assert!(out.contains("options summary: from notes"), "{out}");
    assert!(out.contains("option 1: note option"), "{out}");
    assert!(out.contains("manual escape hatches"), "{out}");

    let by_number = run_loom_inbox(workspace, &bin_dir, &state_dir, &["view", "1"]);
    assert!(by_number.status.success(), "stderr={}", stderr(&by_number));
    assert!(stdout(&by_number).contains("lm-view"));

    let by_proposal = run_loom_inbox(workspace, &bin_dir, &state_dir, &["view", "-p", "lm-prop"]);
    assert!(
        by_proposal.status.success(),
        "stderr={}",
        stderr(&by_proposal)
    );
    let out = stdout(&by_proposal);
    assert!(out.contains("inbox item lm-prop [tune]"), "{out}");
    assert!(out.contains("proposal branch: loom/tune/lm-prop"), "{out}");
    assert!(out.contains("repo:"), "{out}");
}

#[test]
fn inbox_removed_flags_and_address_exclusivity() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let bin_dir = install_bd_shim(workspace);

    for args in [
        vec!["--chat"],
        vec!["-c"],
        vec!["--dismiss"],
        vec!["-d"],
        vec!["--option", "1"],
        vec!["--text", "answer"],
        vec!["apply"],
        vec!["reply"],
        vec!["resolve"],
        vec!["pick"],
        vec!["view", "1", "-b", "lm-x"],
        vec!["chat", "1", "-p", "lm-x"],
    ] {
        let output = run_loom_inbox(workspace, &bin_dir, &state_dir, &args);
        assert!(
            !output.status.success(),
            "loom inbox {args:?} must reject removed/conflicting surface"
        );
    }

    let msg = Command::new(env!("CARGO_BIN_EXE_loom"))
        .arg("msg")
        .output()
        .expect("spawn loom msg");
    assert!(!msg.status.success(), "loom msg must be removed");
}

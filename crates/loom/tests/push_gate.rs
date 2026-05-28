//! End-to-end live-path tests for the four-condition push gate.
//!
//! Drives `loom gate review` against a Rust mock agent (mock-loom-agent
//! under `LOOM_WRAPIX_BIN`) and a stub `bd` (bd-shim) so the test
//! exercises `ProductionReviewController::review_loop` — the production
//! wiring — rather than `decide_verdict` in isolation. The May-19 lesson
//! pinned by `specs/harness.md` § FR9 is that unit tests on
//! `decide_verdict` passed throughout while the real binary discarded
//! exit codes; the live-path tests below are the contract that catches
//! that class of regression next time.
//!
//! Scenarios mirror the four push-gate causes wired in
//! `loom-workflow/src/review/runner.rs`:
//!
//! - `review-concern`: mock agent emits `LOOM_CONCERN: <token> -- <reason>`
//!   as the sole final-line marker → push refused.
//! - `integrity-finding`: a spec file in the molecule's diff scope
//!   carries an unresolved `[check]` annotation → push refused and the
//!   molecule's epic gets `loom:clarify` with the auto-`## Options — …`
//!   block.
//! - clean: mock agent emits `LOOM_COMPLETE` only and no integrity
//!   findings surface → gate reaches the `Pushed` branch.
//!
//! Plus a live-path replay of the literal May-19 sequence — a `concern`
//! line followed by a `LOOM_COMPLETE` line — to confirm that
//! `parse_exit_signal` (A.13) picks the trailing marker through the
//! real binary, not just the unit test. The rendered-template fixture
//! that pins A.7's "emit only one" instruction lives next to the other
//! review-template tests in `loom-templates/tests/render.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use loom_driver::identifier::{MoleculeId, SpecLabel};
use loom_driver::state::{ActiveMolecule, StateDb};
use loom_gate::{IntegrityFinding, Tier, format_clarify_options};

// -------------------------------------------------------------------
// Workspace + state setup
// -------------------------------------------------------------------

/// Initialise `workspace` as a real git repo and create one seed commit.
/// `loom gate review`'s integrity walk opens a [`GitClient`] and queries
/// `git diff <base>..HEAD -- specs/`; without a repo the controller's
/// `integrity_findings()` returns an [`std::io::Error`] before the
/// four-condition AND runs. Returns the seed commit's full SHA.
fn init_workspace_repo(workspace: &Path) -> String {
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "test@example.com"][..],
        &["config", "user.name", "Test"][..],
        &["config", "commit.gpgsign", "false"][..],
    ] {
        let status = Command::new("git")
            .arg("-C")
            .arg(workspace)
            .args(args)
            .status()
            .expect("git spawn");
        assert!(status.success(), "git {args:?} failed: {status}");
    }
    // .gitignore must shield the state dirs the test creates inside the
    // workspace; otherwise `git diff` would surface them on every run.
    std::fs::write(
        workspace.join(".gitignore"),
        "bd-state/\nbd-bin/\nbin/\n.loom-test-state/\n.wrapix/\n*.tar\nprofile-images.json\n",
    )
    .expect("write .gitignore");
    let status = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["add", "."])
        .status()
        .expect("git add spawn");
    assert!(status.success(), "git add failed: {status}");
    let status = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["commit", "-q", "-m", "seed"])
        .status()
        .expect("git commit spawn");
    assert!(status.success(), "git commit failed: {status}");
    let out = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse spawn");
    assert!(out.status.success(), "git rev-parse failed");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Stage every file under `workspace`, commit it with `msg`, and return
/// the new HEAD sha. Used by the integrity-finding scenario to land a
/// spec mutation between the molecule's `base_commit` and `HEAD`.
fn commit_all(workspace: &Path, msg: &str) -> String {
    let status = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["add", "."])
        .status()
        .expect("git add spawn");
    assert!(status.success(), "git add failed");
    let status = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["commit", "-q", "-m", msg])
        .status()
        .expect("git commit spawn");
    assert!(status.success(), "git commit failed");
    let out = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse spawn");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Seed a bd-shim bead directory at `state_dir/<id>/` as an `epic`.
/// The push-gate review controller resolves the molecule via
/// `bd find --type=epic --label=spec:<X> --status=open`, so a `task`
/// seed would not surface as a recovery epic.
fn seed_bead(state_dir: &Path, id: &str, title: &str, description: &str, labels: &[&str]) {
    let bead_dir = state_dir.join(id);
    std::fs::create_dir_all(&bead_dir).expect("mkdir bead dir");
    std::fs::write(bead_dir.join("title"), title).expect("write title");
    std::fs::write(bead_dir.join("description"), description).expect("write description");
    std::fs::write(bead_dir.join("status"), "open").expect("write status");
    std::fs::write(bead_dir.join("priority"), "2").expect("write priority");
    std::fs::write(bead_dir.join("issue_type"), "epic").expect("write issue_type");
    std::fs::write(bead_dir.join("labels"), labels.join("\n")).expect("write labels");
}

/// Install the `bd-shim` binary as `bd` on a fresh PATH entry, plus
/// stub `beads-push` / `git` shims as requested. Returns the directory
/// the caller prepends to `PATH`.
fn install_path_shims(workspace: &Path, want_beads_push: bool) -> PathBuf {
    let bin_dir = workspace.join("bd-bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir bd-bin");
    let bd_path = bin_dir.join("bd");
    let source = PathBuf::from(env!("CARGO_BIN_EXE_bd-shim"));
    match std::os::unix::fs::symlink(&source, &bd_path) {
        Ok(_) => {}
        Err(_) => {
            std::fs::copy(&source, &bd_path).expect("copy bd-shim");
            let mut perm = std::fs::metadata(&bd_path).expect("stat bd").permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&bd_path, perm).expect("chmod bd");
        }
    }
    if want_beads_push {
        let beads_push = bin_dir.join("beads-push");
        std::fs::write(
            &beads_push,
            "#!/bin/sh\necho beads-push stub: $@ >&2\nexit 0\n",
        )
        .expect("write beads-push stub");
        let mut perm = std::fs::metadata(&beads_push)
            .expect("stat beads-push")
            .permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&beads_push, perm).expect("chmod beads-push");
    }
    bin_dir
}

/// Write a `profile-images.json` pointing at an empty tar source. The
/// mock agent never instantiates the image, but
/// `ProductionReviewController::run_review` resolves the `base` profile
/// through the manifest before dispatch; a missing entry would surface
/// as `ProfileError::UnknownProfile`.
fn write_minimal_manifest(workspace: &Path) -> PathBuf {
    let source = workspace.join("base.tar");
    std::fs::write(&source, "").expect("write base.tar");
    let manifest = workspace.join("profile-images.json");
    let body = format!(
        r#"{{"base": {{"ref":"localhost/wrapix-base:test","source":{source:?}}}}}"#,
        source = source.display().to_string(),
    );
    std::fs::write(&manifest, body).expect("write manifest");
    manifest
}

/// Seed `state.db` with one active molecule for `label` whose
/// `base_commit` points at `base_sha`. `ProductionReviewController`'s
/// `integrity_findings()` short-circuits to an empty list when this row
/// is missing, so every scenario that wants to exercise the integrity
/// branch must call this with a real seed commit. Resolution itself
/// goes through `bd find` against the bd-shim's epic bead seed.
fn seed_active_molecule(workspace: &Path, label: &str, mol_id: &str, base_sha: &str) {
    std::fs::create_dir_all(workspace.join(".wrapix/loom")).expect("mkdir state dir");
    let db = StateDb::open(workspace.join(".wrapix/loom/state.db")).expect("open state.db");
    db.rebuild(
        workspace,
        &[ActiveMolecule {
            id: MoleculeId::new(mol_id),
            spec_label: SpecLabel::new(label),
            base_commit: Some(base_sha.to_string()),
        }],
    )
    .expect("rebuild state.db");
    drop(db);
}

/// Drive `loom gate review -s <label>` against the wired stubs and
/// return the captured `Output`. `verify_exit` adds `--verify-exit <CODE>`
/// to the child argv when `Some`, mirroring how `loom loop`'s
/// molecule-completion handoff threads the verify exit into the push
/// gate's four-condition AND (FR9 condition 2).
fn run_loom_gate_review(
    workspace: &Path,
    bin_dir: &Path,
    state_dir: &Path,
    manifest: &Path,
    agent_mode: &str,
    spec_label: &str,
    verify_exit: Option<i32>,
) -> std::process::Output {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let mut entries: Vec<PathBuf> = vec![bin_dir.to_path_buf()];
    entries.extend(std::env::split_paths(&path_var));
    let new_path = std::env::join_paths(entries).expect("join PATH");

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let mock_agent = env!("CARGO_BIN_EXE_mock-loom-agent");

    let mut cmd = Command::new(loom_bin);
    cmd.arg("--workspace")
        .arg(workspace)
        .arg("--agent")
        .arg("pi")
        .arg("gate")
        .arg("review")
        .arg("-s")
        .arg(spec_label);
    if let Some(code) = verify_exit {
        cmd.arg("--verify-exit").arg(code.to_string());
    }
    cmd.env("PATH", new_path)
        .env("LOOM_WRAPIX_BIN", mock_agent)
        .env("LOOM_TEST_AGENT_MODE", agent_mode)
        .env("LOOM_BIN", loom_bin)
        .env("LOOM_PROFILES_MANIFEST", manifest)
        .env("BD_STATE_DIR", state_dir)
        .env("XDG_STATE_HOME", workspace.join(".loom-test-state"))
        .env_remove("LOOM_INSIDE")
        .output()
        .expect("spawn loom")
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
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

// -------------------------------------------------------------------
// push_gate_walk event introspection
// -------------------------------------------------------------------

/// Read every `driver_event` record from the review phase JSONL log
/// and return them in emission order. Empty when no review log was
/// written (e.g. the controller errored before opening the phase log
/// or no `emit_driver_event` call landed for this phase).
fn read_driver_events(workspace: &Path, label: &str) -> Vec<serde_json::Value> {
    let logs_dir = workspace.join(format!(".wrapix/loom/logs/{label}"));
    let entries = match std::fs::read_dir(&logs_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .filter(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with("review-"))
        })
        .collect();
    paths.sort();
    let mut out = Vec::new();
    for path in paths {
        let body = std::fs::read_to_string(&path).unwrap_or_default();
        for line in body.lines() {
            if line.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if v["kind"] == "driver_event" {
                out.push(v);
            }
        }
    }
    out
}

/// Pull the first `push_gate_refuse` event's `cause` field, if any.
fn refuse_cause(events: &[serde_json::Value]) -> Option<String> {
    events
        .iter()
        .find(|e| e["driver_kind"] == "push_gate_refuse")
        .and_then(|e| e["payload"]["cause"].as_str())
        .map(str::to_string)
}

/// True iff any recorded bd-shim invocation matches `bd update <id>
/// --add-label loom:clarify`.
fn bd_applied_clarify(log: &str, target_id: &str) -> bool {
    log.lines().any(|line| {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        tokens.first() == Some(&"update")
            && tokens.get(1) == Some(&target_id)
            && tokens.contains(&"--add-label")
            && tokens.contains(&"loom:clarify")
    })
}

// -------------------------------------------------------------------
// Scenario 2 — `LOOM_CONCERN: verifier-bypass -- …` refuses the push
// -------------------------------------------------------------------

/// Production wiring requirement (FR9): a reviewer agent emitting
/// `LOOM_CONCERN: <token> -- <reason>` as its sole final-line marker
/// MUST refuse the push with cause `review-concern`. The earlier
/// label-only verdict path silently let the molecule push despite the
/// concern; this test pins the inverse contract against `loom gate
/// review`'s real wiring rather than a `decide_verdict` unit fake.
#[test]
fn push_gate_refuses_on_review_concern_via_live_path() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let label = "pushconcern";

    // No diff scope between base_commit and HEAD → integrity walk yields
    // empty findings; only the review marker drives the verdict.
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(
        workspace.join(format!("specs/{label}.md")),
        "## Success Criteria\n\n",
    )
    .unwrap();

    let base_sha = init_workspace_repo(workspace);

    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();
    seed_bead(
        &state_dir,
        "lm-mol",
        "molecule epic",
        "Epic for push-gate review-concern test.\n",
        &["spec:pushconcern"],
    );

    seed_active_molecule(workspace, label, "lm-mol", &base_sha);

    let bin_dir = install_path_shims(workspace, false);
    let manifest = write_minimal_manifest(workspace);

    let output = run_loom_gate_review(
        workspace,
        &bin_dir,
        &state_dir,
        &manifest,
        "concern-marker",
        label,
        None,
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let log = read_invocation_log(&state_dir);
    let events = read_driver_events(workspace, label);

    assert!(
        output.status.success(),
        "loom gate review must exit 0 on a refused push (the verdict gate \
         applies labels and exits without erroring).\n\
         stdout={stdout}\nstderr={stderr}\nbd-shim log:\n{log}",
    );
    assert_eq!(
        refuse_cause(&events).as_deref(),
        Some("review-concern"),
        "push_gate_refuse must tag cause=review-concern. events:\n{events:#?}\n\
         stdout={stdout}\nstderr={stderr}\nbd-shim log:\n{log}",
    );
    // No remote is configured; a refused push must NOT shell out to
    // `git push` (which would surface as `GitPushFailed` in stderr).
    assert!(
        !stderr.contains("GitPushFailed"),
        "push must NOT have been attempted on a review-concern verdict. \
         stderr={stderr}\nbd-shim log:\n{log}",
    );
}

// -------------------------------------------------------------------
// Scenario 3 — integrity finding terminal at the push gate
// -------------------------------------------------------------------

/// FR9 condition 4: an `UnresolvedAnnotation` finding within the
/// molecule's diff scope MUST refuse the push (`integrity-finding`) and
/// apply `loom:clarify` to the molecule's epic with the auto-generated
/// `## Options — …` block. The mock review agent emits `LOOM_COMPLETE`,
/// so the only failing input is the integrity finding — the test pins
/// that branch in isolation against the live binary.
#[test]
fn push_gate_refuses_on_integrity_finding_via_live_path() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let label = "pushintegrity";

    // Base commit: spec file with no annotation.
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    let spec_path = workspace.join(format!("specs/{label}.md"));
    std::fs::write(&spec_path, "## Success Criteria\n\n- baseline criterion\n").unwrap();
    let base_sha = init_workspace_repo(workspace);

    // HEAD: spec gains an unresolved `[check]` annotation whose first
    // token resolves neither on PATH nor against the workspace.
    let unresolvable_target = "definitely-not-a-real-command-xyz-loomtest";
    std::fs::write(
        &spec_path,
        format!(
            "## Success Criteria\n\n\
             - baseline criterion\n\
             - new criterion needing a verifier\n  [check]({unresolvable_target})\n",
        ),
    )
    .unwrap();
    commit_all(workspace, "add unresolved annotation");

    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();
    seed_bead(
        &state_dir,
        "lm-mol",
        "molecule epic",
        "Epic for push-gate integrity test.\n",
        &["spec:pushintegrity"],
    );

    seed_active_molecule(workspace, label, "lm-mol", &base_sha);

    let bin_dir = install_path_shims(workspace, false);
    let manifest = write_minimal_manifest(workspace);

    let output = run_loom_gate_review(
        workspace,
        &bin_dir,
        &state_dir,
        &manifest,
        "complete-marker",
        label,
        None,
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let log = read_invocation_log(&state_dir);
    let events = read_driver_events(workspace, label);

    assert!(
        output.status.success(),
        "loom gate review must exit 0 on integrity-finding refusal.\n\
         stdout={stdout}\nstderr={stderr}\nbd-shim log:\n{log}",
    );
    assert_eq!(
        refuse_cause(&events).as_deref(),
        Some("integrity-finding"),
        "push_gate_refuse must tag cause=integrity-finding. events:\n{events:#?}\n\
         stdout={stdout}\nstderr={stderr}\nbd-shim log:\n{log}",
    );

    let epic_labels = read_labels(&state_dir, "lm-mol");
    assert!(
        epic_labels.iter().any(|l| l == "loom:clarify"),
        "epic must carry loom:clarify on integrity-finding refusal. \
         labels={epic_labels:?}\nbd-shim log:\n{log}",
    );
    assert!(
        bd_applied_clarify(&log, "lm-mol"),
        "bd update lm-mol --add-label loom:clarify must be recorded. \
         bd-shim log:\n{log}",
    );
    let notes = read_field(&state_dir, "lm-mol", "notes");
    assert!(
        notes.contains("## Options —"),
        "epic notes must carry the canonical Options block. notes:\n{notes}",
    );
    // The block must round-trip through `format_clarify_options` —
    // independently re-rendering the finding shape must reproduce
    // every byte of the notes the controller wrote. Spec line and
    // path are derived from the parsed annotation so the test cannot
    // diverge from the writer's coordinate by accident.
    let line = parse_options_spec_line(&notes);
    let expected = format_clarify_options(&[IntegrityFinding::UnresolvedAnnotation {
        spec: PathBuf::from(format!("specs/{label}.md")),
        line,
        tier: Tier::Check,
        target: unresolvable_target.to_string(),
    }]);
    assert_eq!(
        notes, expected,
        "epic notes must match format_clarify_options verbatim. \
         expected:\n{expected}\nactual:\n{notes}",
    );
    assert!(
        notes.contains(unresolvable_target),
        "epic notes must reference the unresolvable target. notes:\n{notes}",
    );
}

/// Extract the `:line` suffix from the canonical `## Options — Unresolved
/// [tier](target) at <spec>:<line>` heading the integrity-gate writer
/// produces. Lets the integrity-finding test compare the whole notes
/// body against `format_clarify_options` without hard-coding the
/// criterion line.
fn parse_options_spec_line(notes: &str) -> u32 {
    let heading = notes
        .lines()
        .next()
        .expect("notes must carry the canonical Options heading");
    let (_, tail) = heading.rsplit_once(':').expect("heading must end :<line>");
    tail.trim().parse().expect("line suffix must parse as u32")
}

// -------------------------------------------------------------------
// Scenario 4 — clean path: every input passes, push fires
// -------------------------------------------------------------------

/// Clean push: `LOOM_COMPLETE`, no `loom:blocked` / `loom:clarify`
/// beads, no integrity findings → the gate must reach `push_gate_clean`
/// and invoke `git_push` + `beads_push`. A bare git remote stands in
/// for `origin` so `git push` can succeed without network access; a
/// `beads-push` stub on PATH lets the second push exit 0.
#[test]
fn push_gate_fires_clean_when_all_conditions_pass_via_live_path() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let label = "pushclean";

    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(
        workspace.join(format!("specs/{label}.md")),
        "## Success Criteria\n\n",
    )
    .unwrap();
    let base_sha = init_workspace_repo(workspace);

    // Bare local remote so `git push` resolves without network access.
    let remote_dir = dir.path().join("remote.git");
    let status = Command::new("git")
        .args(["init", "--bare", "-q"])
        .arg(&remote_dir)
        .status()
        .expect("git init --bare spawn");
    assert!(status.success(), "git init --bare failed");
    let status = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["remote", "add", "origin"])
        .arg(&remote_dir)
        .status()
        .expect("git remote add spawn");
    assert!(status.success(), "git remote add failed");
    let status = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["push", "-q", "-u", "origin", "main"])
        .status()
        .expect("initial push spawn");
    assert!(status.success(), "initial push failed");

    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();
    seed_bead(
        &state_dir,
        "lm-mol",
        "molecule epic",
        "Epic for push-gate clean-path test.\n",
        &["spec:pushclean"],
    );

    seed_active_molecule(workspace, label, "lm-mol", &base_sha);

    let bin_dir = install_path_shims(workspace, true);
    let manifest = write_minimal_manifest(workspace);

    let output = run_loom_gate_review(
        workspace,
        &bin_dir,
        &state_dir,
        &manifest,
        "complete-marker",
        label,
        Some(0),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let log = read_invocation_log(&state_dir);
    let events = read_driver_events(workspace, label);

    assert!(
        output.status.success(),
        "loom gate review must exit 0 on the clean-push branch.\n\
         stdout={stdout}\nstderr={stderr}\nbd-shim log:\n{log}",
    );
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|e| e["driver_kind"].as_str())
        .collect();
    assert!(
        kinds.contains(&"push_gate_clean"),
        "clean-push branch must emit push_gate_clean. kinds={kinds:?}\n\
         events:\n{events:#?}\nstderr={stderr}",
    );
    assert!(
        !kinds.contains(&"push_gate_refuse"),
        "clean-push branch must NOT emit push_gate_refuse. kinds={kinds:?}",
    );
    assert!(
        stdout.contains("Pushed"),
        "controller must surface ReviewResult::Pushed on the clean path. \
         stdout={stdout}",
    );
}

// -------------------------------------------------------------------
// Scenario 1 — live-path parser disambiguation
// -------------------------------------------------------------------

/// Replay the May-19 sequence through the live binary: the mock agent
/// emits `LOOM_CONCERN: <token> -- <reason>` and then `LOOM_COMPLETE` on
/// later lines. `parse_exit_signal` (A.13) inspects only the final
/// non-empty line, so the trailing `LOOM_COMPLETE` wins and — with
/// every other condition passing — the push fires. The unit tests on
/// `parse_exit_signal` in `loom-workflow/src/todo/exit.rs` already pin
/// this; this test is the cross-binary witness that the production
/// dispatcher carries the same behaviour end-to-end.
#[test]
fn concern_then_complete_live_path_resolves_to_clean_push() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let label = "pushrelay";

    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(
        workspace.join(format!("specs/{label}.md")),
        "## Success Criteria\n\n",
    )
    .unwrap();
    let base_sha = init_workspace_repo(workspace);

    let remote_dir = dir.path().join("remote.git");
    let status = Command::new("git")
        .args(["init", "--bare", "-q"])
        .arg(&remote_dir)
        .status()
        .expect("git init --bare spawn");
    assert!(status.success(), "git init --bare failed");
    let status = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["remote", "add", "origin"])
        .arg(&remote_dir)
        .status()
        .expect("git remote add spawn");
    assert!(status.success(), "git remote add failed");
    let status = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["push", "-q", "-u", "origin", "main"])
        .status()
        .expect("initial push spawn");
    assert!(status.success(), "initial push failed");

    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();
    seed_bead(
        &state_dir,
        "lm-mol",
        "molecule epic",
        "Epic for May-19 sequence relay test.\n",
        &["spec:pushrelay"],
    );
    seed_active_molecule(workspace, label, "lm-mol", &base_sha);

    let bin_dir = install_path_shims(workspace, true);
    let manifest = write_minimal_manifest(workspace);

    let output = run_loom_gate_review(
        workspace,
        &bin_dir,
        &state_dir,
        &manifest,
        "concern-then-complete",
        label,
        Some(0),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let log = read_invocation_log(&state_dir);
    let events = read_driver_events(workspace, label);

    assert!(
        output.status.success(),
        "loom gate review must exit 0 — the final-line parser must pick \
         LOOM_COMPLETE over the earlier LOOM_CONCERN.\n\
         stdout={stdout}\nstderr={stderr}\nbd-shim log:\n{log}",
    );
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|e| e["driver_kind"].as_str())
        .collect();
    assert!(
        kinds.contains(&"push_gate_clean"),
        "two-marker session must still route to push_gate_clean once the \
         parser picks the final LOOM_COMPLETE. kinds={kinds:?}\n\
         events:\n{events:#?}",
    );
    assert!(
        !kinds.contains(&"push_gate_refuse"),
        "final-line LOOM_COMPLETE must NOT route to push_gate_refuse. \
         kinds={kinds:?}",
    );
}

// -------------------------------------------------------------------
// Scenario 5 — verifier-failed via the threaded --verify-exit flag
// -------------------------------------------------------------------

/// FR9 condition 2 production wiring (lm-e6c8r.25): when `loom loop`'s
/// molecule-completion handoff captures a non-zero `loom gate verify`
/// exit and threads it into `loom gate review --verify-exit <CODE>`,
/// `ProductionReviewController::verify_exit()` MUST return that value
/// and the push gate MUST refuse with cause `verifier-failed` — even
/// though the reviewer agent emits `LOOM_COMPLETE` and no other input
/// fails. The earlier wiring discarded the parent's verify exit and
/// returned the default `None`, collapsing the four-condition AND to a
/// three-condition one; this test pins the live binary against that
/// regression.
#[test]
fn push_gate_refuses_when_verify_exit_flag_is_nonzero_via_live_path() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let label = "pushverify";

    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(
        workspace.join(format!("specs/{label}.md")),
        "## Success Criteria\n\n",
    )
    .unwrap();
    let base_sha = init_workspace_repo(workspace);

    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();
    seed_bead(
        &state_dir,
        "lm-mol",
        "molecule epic",
        "Epic for push-gate verifier-failed test.\n",
        &["spec:pushverify"],
    );
    seed_active_molecule(workspace, label, "lm-mol", &base_sha);

    let bin_dir = install_path_shims(workspace, false);
    let manifest = write_minimal_manifest(workspace);

    // `complete-marker` → reviewer would otherwise vote clean. The only
    // failing input is the threaded verify exit, so push_gate_refuse
    // tagged `verifier-failed` proves the flag wired through.
    let output = run_loom_gate_review(
        workspace,
        &bin_dir,
        &state_dir,
        &manifest,
        "complete-marker",
        label,
        Some(1),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let log = read_invocation_log(&state_dir);
    let events = read_driver_events(workspace, label);

    assert!(
        output.status.success(),
        "loom gate review must exit 0 on a refused push.\n\
         stdout={stdout}\nstderr={stderr}\nbd-shim log:\n{log}",
    );
    assert_eq!(
        refuse_cause(&events).as_deref(),
        Some("verifier-failed"),
        "push_gate_refuse must tag cause=verifier-failed when --verify-exit \
         delivers a non-zero code. events:\n{events:#?}\n\
         stdout={stdout}\nstderr={stderr}\nbd-shim log:\n{log}",
    );
    // No remote is configured; a refused push must NOT shell out to
    // `git push` (which would surface as `GitPushFailed` in stderr).
    assert!(
        !stderr.contains("GitPushFailed"),
        "push must NOT have been attempted on a verifier-failed verdict. \
         stderr={stderr}\nbd-shim log:\n{log}",
    );
}

/// Companion to the non-zero case: `--verify-exit 0` MUST satisfy FR9
/// condition 2 and let the push gate reach `push_gate_clean` when every
/// other input passes. This pins the inverse contract — the
/// `verifier-failed` branch is reached on non-zero *only*, not on every
/// `Some(_)` value. (`Some(0)` is the dominant case: `loom loop` threads
/// `0` on every clean verify pass.)
#[test]
fn push_gate_fires_clean_when_verify_exit_flag_is_zero_via_live_path() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let label = "pushverifyzero";

    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(
        workspace.join(format!("specs/{label}.md")),
        "## Success Criteria\n\n",
    )
    .unwrap();
    let base_sha = init_workspace_repo(workspace);

    // Bare local remote so `git push` resolves without network access.
    let remote_dir = dir.path().join("remote.git");
    let status = Command::new("git")
        .args(["init", "--bare", "-q"])
        .arg(&remote_dir)
        .status()
        .expect("git init --bare spawn");
    assert!(status.success(), "git init --bare failed");
    let status = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["remote", "add", "origin"])
        .arg(&remote_dir)
        .status()
        .expect("git remote add spawn");
    assert!(status.success(), "git remote add failed");
    let status = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["push", "-q", "-u", "origin", "main"])
        .status()
        .expect("initial push spawn");
    assert!(status.success(), "initial push failed");

    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();
    seed_bead(
        &state_dir,
        "lm-mol",
        "molecule epic",
        "Epic for push-gate verifier-zero test.\n",
        &["spec:pushverifyzero"],
    );
    seed_active_molecule(workspace, label, "lm-mol", &base_sha);

    let bin_dir = install_path_shims(workspace, true);
    let manifest = write_minimal_manifest(workspace);

    let output = run_loom_gate_review(
        workspace,
        &bin_dir,
        &state_dir,
        &manifest,
        "complete-marker",
        label,
        Some(0),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let log = read_invocation_log(&state_dir);
    let events = read_driver_events(workspace, label);

    assert!(
        output.status.success(),
        "loom gate review must exit 0 on the clean-push branch with \
         --verify-exit 0.\nstdout={stdout}\nstderr={stderr}\nbd-shim log:\n{log}",
    );
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|e| e["driver_kind"].as_str())
        .collect();
    assert!(
        kinds.contains(&"push_gate_clean"),
        "--verify-exit 0 must satisfy FR9 condition 2; clean push expected. \
         kinds={kinds:?}\nevents:\n{events:#?}\nstderr={stderr}",
    );
    assert!(
        !kinds.contains(&"push_gate_refuse"),
        "zero verify exit must NOT trigger push_gate_refuse. kinds={kinds:?}",
    );
}

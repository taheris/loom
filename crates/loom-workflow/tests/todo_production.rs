//! Integration tests for [`ProductionTodoController`] that need a real git
//! repo. Pure logic for tier classification lives in
//! `src/todo/tier.rs::tests`; pure construction tests
//! (manifest lookup, template selection) live in
//! `src/todo/production.rs::tests`.
//!
//! These tests spawn the system `git` binary to seed and inspect a real
//! workspace (spec NFR #8): tier-1 fan-out resolves through
//! `LiveGitDiffSource` over `loom_driver::git::GitClient`, which only has
//! anything to observe against real refs/index/diff state.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use loom_driver::agent::SessionOutcome;
use loom_driver::bd::{BdClient, BdError, CommandRunner, RunOutput};
use loom_driver::git::GitClient;
use loom_driver::identifier::{MoleculeId, ProfileName, SpecLabel};
use loom_driver::profile_manifest::ProfileImageManifest;
use loom_driver::state::{ActiveMolecule, StateDb};
use loom_workflow::todo::{ExitSignal, ProductionTodoController, TodoController, TodoError};

fn run_git(workspace: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .status()
        .expect("git spawn");
    assert!(status.success(), "git {args:?} failed: {status}");
}

fn capture_head(workspace: &Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse");
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn init_repo(workspace: &Path) -> Arc<GitClient> {
    run_git(workspace, &["init", "-q", "-b", "main"]);
    run_git(workspace, &["config", "user.email", "test@example.com"]);
    run_git(workspace, &["config", "user.name", "Test"]);
    run_git(workspace, &["config", "commit.gpgsign", "false"]);
    std::fs::write(workspace.join("seed.txt"), "seed\n").unwrap();
    run_git(workspace, &["add", "seed.txt"]);
    run_git(workspace, &["commit", "-q", "-m", "seed"]);
    Arc::new(GitClient::open(workspace).unwrap())
}

fn stub_manifest(dir: &Path) -> Arc<ProfileImageManifest> {
    let body = r#"{
      "base": { "ref": "localhost/wrapix-base:abc", "source": "/nix/store/aaa-image-base" }
    }"#;
    let path = dir.join("profile-images.json");
    std::fs::write(&path, body).unwrap();
    Arc::new(ProfileImageManifest::from_path(&path).unwrap())
}

fn empty_state(workspace: &Path) -> Arc<StateDb> {
    Arc::new(StateDb::open(workspace.join(".wrapix/loom/state.db")).unwrap())
}

#[derive(Clone, Default)]
struct CapturingRunner {
    responses: Arc<Mutex<VecDeque<RunOutput>>>,
    calls: Arc<Mutex<Vec<Vec<OsString>>>>,
}

impl CapturingRunner {
    fn new(responses: impl IntoIterator<Item = RunOutput>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into_iter().collect())),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|argv| {
                argv.iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect()
            })
            .collect()
    }
}

impl CommandRunner for CapturingRunner {
    async fn run(&self, args: Vec<OsString>, _t: Duration) -> Result<RunOutput, BdError> {
        self.calls.lock().unwrap().push(args);
        Ok(self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(RunOutput {
                status: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
            }))
    }
}

fn stub_bd() -> Arc<BdClient<CapturingRunner>> {
    scripted_bd([])
}

fn scripted_bd(responses: impl IntoIterator<Item = RunOutput>) -> Arc<BdClient<CapturingRunner>> {
    Arc::new(BdClient::with_runner(CapturingRunner::new(responses)))
}

fn epic_response(mol_id: &str, label: &str, base_commit: Option<&str>) -> RunOutput {
    let metadata = match base_commit {
        Some(b) => format!(r#"{{ "loom.base_commit": "{b}" }}"#),
        None => "{}".to_string(),
    };
    let body = format!(
        r#"[{{
            "id": "{mol_id}",
            "title": "{label}: epic",
            "status": "open",
            "priority": 2,
            "issue_type": "epic",
            "labels": ["spec:{label}"],
            "metadata": {metadata}
        }}]"#,
    );
    RunOutput {
        status: 0,
        stdout: body.into_bytes(),
        stderr: Vec::new(),
    }
}

/// `bd list --type=epic --label=spec:<X> --status=open` response carrying
/// the epic's `parent` field (i.e. the molecule it's bonded to via
/// `bd mol bond`). Used by the multi-spec fan-out classifier to detect
/// whether two touched specs share a molecule.
fn epic_response_with_parent(epic_id: &str, label: &str, parent: Option<&str>) -> RunOutput {
    let parent_field = match parent {
        Some(p) => format!(r#"  "parent": "{p}","#),
        None => String::new(),
    };
    let body = format!(
        r#"[{{
            "id": "{epic_id}",
            "title": "{label}: epic",
            "status": "open",
            "priority": 2,
            "issue_type": "epic",
            "labels": ["spec:{label}"],
            {parent_field}
            "metadata": {{}}
        }}]"#,
    );
    RunOutput {
        status: 0,
        stdout: body.into_bytes(),
        stderr: Vec::new(),
    }
}

/// `bd create --silent` response — `bd` prints only the new bead's id on
/// stdout under `--silent`. The collision-clarify path consumes this to
/// learn the minted bead's id for the `MultiSpecCollision` error.
fn create_silent_response(new_id: &str) -> RunOutput {
    RunOutput {
        status: 0,
        stdout: format!("{new_id}\n").into_bytes(),
        stderr: Vec::new(),
    }
}

/// `bd list --type=epic --label=spec:<X> --status=open` returning an empty
/// result — the spec has no open epic.
fn empty_epic_response() -> RunOutput {
    RunOutput {
        status: 0,
        stdout: b"[]".to_vec(),
        stderr: Vec::new(),
    }
}

fn seeded_state(
    workspace: &Path,
    label: &str,
    mol: &str,
    base_commit: Option<String>,
) -> Arc<StateDb> {
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(
        workspace.join(format!("specs/{label}.md")),
        format!("# {label}\n"),
    )
    .unwrap();
    let db = StateDb::open(workspace.join(".wrapix/loom/state.db")).unwrap();
    db.rebuild(
        workspace,
        &[ActiveMolecule {
            id: MoleculeId::new(mol),
            spec_label: SpecLabel::new(label),
            base_commit,
        }],
    )
    .unwrap();
    Arc::new(db)
}

/// `loom todo` must build a `SpawnConfig` whose
/// `initial_prompt` carries the rendered phase template body (with the
/// scratchpad path partial), whose `RePinContent` is an empty placeholder
/// — the rendered phase prompt now flows from `<scratch_dir>/prompt.txt`
/// via post-compaction `repin.sh`, not from the `repin` field — and whose
/// scratch dir holds a `prompt.txt` whose contents equal `initial_prompt`.
/// Mirror of the `loom review` and `loom loop` dispatch-shape tests
/// (`src/review/production.rs`, `src/loop/production.rs`).
#[tokio::test]
async fn build_session_dispatches_rendered_todo_template_and_writes_prompt_txt() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    let state = empty_state(&workspace);
    let manifest = stub_manifest(&workspace);
    let git = init_repo(&workspace);
    let mut ctrl = ProductionTodoController::new(
        SpecLabel::new("harness"),
        workspace,
        state,
        manifest,
        ProfileName::new("base"),
        git,
        stub_bd(),
        None,
    );
    let session = ctrl.build_session().await.expect("build cfg");
    let cfg = &session.config;
    assert!(
        cfg.initial_prompt.contains("# Task Decomposition"),
        "prompt missing template heading: {}",
        cfg.initial_prompt,
    );
    assert!(
        cfg.initial_prompt.contains("specs/harness.md"),
        "prompt missing spec path: {}",
        cfg.initial_prompt,
    );
    assert!(
        cfg.initial_prompt.contains(".wrapix/loom/scratch"),
        "prompt missing scratchpad partial: {}",
        cfg.initial_prompt,
    );
    assert!(
        cfg.repin.orientation.is_empty()
            && cfg.repin.pinned_context.is_empty()
            && cfg.repin.partial_bodies.is_empty(),
        "RePinContent must be empty placeholder; rendered template lives in prompt.txt: {:?}",
        cfg.repin,
    );
    let written =
        std::fs::read_to_string(cfg.scratch_dir.join("prompt.txt")).expect("prompt.txt readable");
    assert_eq!(written, cfg.initial_prompt);
}

#[tokio::test]
async fn build_spawn_config_resolves_manifest_image_and_renders_new_template() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    let state = empty_state(&workspace);
    let manifest = stub_manifest(&workspace);
    let git = init_repo(&workspace);
    let mut ctrl = ProductionTodoController::new(
        SpecLabel::new("alpha"),
        workspace,
        state,
        manifest,
        ProfileName::new("base"),
        git,
        stub_bd(),
        None,
    );
    let session = ctrl.build_session().await.expect("build cfg");
    let cfg = &session.config;
    assert!(
        cfg.initial_prompt.contains("Task Decomposition"),
        "TodoNewContext renders todo_new.md (header marker missing): {}",
        cfg.initial_prompt,
    );
    assert!(
        cfg.initial_prompt.contains("alpha"),
        "spec label must appear in rendered prompt: {}",
        cfg.initial_prompt,
    );
}

#[tokio::test]
async fn build_spawn_config_uses_update_template_when_molecule_exists() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    let state = seeded_state(&workspace, "alpha", "lm-mol", None);
    let manifest = stub_manifest(&workspace);
    let git = init_repo(&workspace);
    let mut ctrl = ProductionTodoController::new(
        SpecLabel::new("alpha"),
        workspace,
        state,
        manifest,
        ProfileName::new("base"),
        git,
        scripted_bd([epic_response("lm-mol", "alpha", None)]),
        None,
    );
    let session = ctrl.build_session().await.expect("build cfg");
    let cfg = &session.config;
    assert!(
        cfg.initial_prompt.contains("lm-mol"),
        "molecule id must thread into update template: {}",
        cfg.initial_prompt,
    );
}

#[tokio::test]
async fn build_spawn_config_surfaces_unknown_profile_as_profile_error() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    let state = empty_state(&workspace);
    let manifest = stub_manifest(&workspace);
    let git = init_repo(&workspace);
    let mut ctrl = ProductionTodoController::new(
        SpecLabel::new("alpha"),
        workspace,
        state,
        manifest,
        ProfileName::new("missing"),
        git,
        stub_bd(),
        None,
    );
    let err = match ctrl.build_session().await {
        Ok(_) => panic!("expected Profile error, got Ok"),
        Err(e) => e,
    };
    assert!(
        matches!(err, TodoError::Profile(_)),
        "expected Profile, got {err:?}",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn build_spawn_config_tier_1_renders_diff_from_base_commit() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(workspace.join("specs/alpha.md"), "# alpha\n").unwrap();
    let git = init_repo(&workspace);
    run_git(&workspace, &["add", "specs"]);
    run_git(&workspace, &["commit", "-q", "-m", "seed alpha"]);
    let base = capture_head(&workspace);

    std::fs::write(
        workspace.join("specs/alpha.md"),
        "# alpha\n\ntier-1 marker line\n",
    )
    .unwrap();
    run_git(&workspace, &["commit", "-q", "-am", "update alpha"]);

    let state = seeded_state(&workspace, "alpha", "lm-mol", Some(base.clone()));
    let manifest = stub_manifest(&workspace);
    let mut ctrl = ProductionTodoController::new(
        SpecLabel::new("alpha"),
        workspace,
        state,
        manifest,
        ProfileName::new("base"),
        git,
        scripted_bd([epic_response("lm-mol", "alpha", Some(&base))]),
        None,
    );
    let session = ctrl.build_session().await.expect("build cfg");
    let cfg = &session.config;
    assert!(
        cfg.initial_prompt.contains("=== specs/alpha.md ==="),
        "tier-1 prompt must carry the per-spec diff header: {}",
        cfg.initial_prompt,
    );
    assert!(
        cfg.initial_prompt.contains("tier-1 marker line"),
        "tier-1 prompt must include the spec diff body: {}",
        cfg.initial_prompt,
    );
}

/// Spec criterion `test_todo_renders_notes_into_beads`: `loom todo` reads
/// implementation notes from the anchor's `notes` rows (kind =
/// 'implementation') and renders each note's text into the prompt so the
/// agent copies them into every new bead body it creates.
#[tokio::test]
async fn build_spawn_config_renders_implementation_notes_from_db() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    let state = seeded_state(&workspace, "alpha", "lm-mol", None);
    let label = SpecLabel::new("alpha");
    state
        .notes_add(&label, "implementation", "touch lib/foo/bar.rs", 100)
        .unwrap();
    state
        .notes_add(&label, "implementation", "beware FK cascade ordering", 200)
        .unwrap();
    // Non-implementation kinds must NOT bleed into the todo prompt.
    state
        .notes_add(&label, "design", "design-only context", 300)
        .unwrap();
    let manifest = stub_manifest(&workspace);
    let git = init_repo(&workspace);
    let mut ctrl = ProductionTodoController::new(
        label,
        workspace,
        state,
        manifest,
        ProfileName::new("base"),
        git,
        stub_bd(),
        None,
    );
    let session = ctrl.build_session().await.expect("build cfg");
    let prompt = &session.config.initial_prompt;
    assert!(
        prompt.contains("## Implementation Notes"),
        "prompt missing Implementation Notes header: {prompt}",
    );
    assert!(
        prompt.contains("touch lib/foo/bar.rs"),
        "prompt missing first impl note: {prompt}",
    );
    assert!(
        prompt.contains("beware FK cascade ordering"),
        "prompt missing second impl note: {prompt}",
    );
    assert!(
        !prompt.contains("design-only context"),
        "prompt must NOT include design-kind notes: {prompt}",
    );
    assert_eq!(
        prompt.matches("<implementation-note>").count(),
        2,
        "expected 2 implementation-note markers, got prompt: {prompt}",
    );
}

/// Empty notes table → prompt omits the Implementation Notes section entirely
/// (no empty `## Implementation Notes` header). Guards against the section
/// rendering with a stale header when no notes have been recorded.
#[tokio::test]
async fn build_spawn_config_omits_notes_section_when_notes_empty() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    let state = seeded_state(&workspace, "alpha", "lm-mol", None);
    let manifest = stub_manifest(&workspace);
    let git = init_repo(&workspace);
    let mut ctrl = ProductionTodoController::new(
        SpecLabel::new("alpha"),
        workspace,
        state,
        manifest,
        ProfileName::new("base"),
        git,
        stub_bd(),
        None,
    );
    let session = ctrl.build_session().await.expect("build cfg");
    assert!(
        !session
            .config
            .initial_prompt
            .contains("## Implementation Notes"),
        "empty notes must omit the Implementation Notes section: {}",
        session.config.initial_prompt,
    );
}

/// Productive completion (`exit_code == 0` AND `LOOM_COMPLETE` /
/// `LOOM_NOOP`) advances `loom.base_commit` on the molecule's epic
/// (via `bd update --set-metadata`) AND the local
/// `molecules.base_commit` cache; any other terminal state leaves both
/// untouched. Spec criterion
/// `base_commit_advances_only_on_complete_or_noop_with_clean_exit`.
#[tokio::test(flavor = "multi_thread")]
async fn base_commit_advances_only_on_complete_or_noop_with_clean_exit() {
    for (marker, exit_code, expected_advance, case) in [
        (Some(ExitSignal::Complete), 0, true, "complete + exit 0"),
        (Some(ExitSignal::Noop), 0, true, "noop + exit 0"),
        (Some(ExitSignal::Complete), 1, false, "complete + exit 1"),
        (None, 0, false, "missing marker + exit 0"),
        (
            Some(ExitSignal::Blocked { reason: "x".into() }),
            0,
            false,
            "blocked + exit 0",
        ),
        (
            Some(ExitSignal::Clarify {
                question: "x".into(),
            }),
            0,
            false,
            "clarify + exit 0",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        let state = seeded_state(&workspace, "alpha", "lm-alpha", Some("old-sha".into()));
        let label = SpecLabel::new("alpha");
        state
            .notes_add(&label, "implementation", "impl 1", 100)
            .unwrap();
        state
            .notes_add(&label, "implementation", "impl 2", 200)
            .unwrap();
        state.notes_add(&label, "design", "design 1", 300).unwrap();
        let manifest = stub_manifest(&workspace);
        let git = init_repo(&workspace);
        let head_after_seed = capture_head(&workspace);
        // Two bd responses cover the productive-completion path: a list
        // returning the open epic, then an empty update response. Non-
        // productive cases hit the list once or not at all and consume a
        // subset.
        let runner = CapturingRunner::new([
            epic_response("lm-alpha", "alpha", Some("old-sha")),
            epic_response("lm-alpha", "alpha", Some("old-sha")),
            RunOutput {
                status: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        ]);
        let runner_handle = runner.clone();
        let bd = Arc::new(BdClient::with_runner(runner));
        let mut ctrl = ProductionTodoController::new(
            label.clone(),
            workspace,
            Arc::clone(&state),
            manifest,
            ProfileName::new("base"),
            git,
            bd,
            None,
        );

        ctrl.record_outcome(
            &SessionOutcome {
                exit_code,
                cost_usd: None,
            },
            marker.as_ref(),
        )
        .await
        .unwrap_or_else(|e| panic!("case `{case}`: record_outcome failed: {e}"));

        let mol = state
            .molecule_for_spec(&label)
            .unwrap()
            .expect("molecule survives");
        let impl_notes_left = state
            .notes_list(Some(&label), Some("implementation"))
            .unwrap()
            .len();
        let bd_calls = runner_handle.calls();
        if expected_advance {
            assert_eq!(
                mol.base_commit,
                Some(head_after_seed.clone()),
                "case `{case}`: molecules.base_commit must advance to HEAD",
            );
            assert_eq!(
                impl_notes_left, 0,
                "case `{case}`: productive completion must delete implementation notes",
            );
            assert_eq!(
                state
                    .notes_list(Some(&label), Some("design"))
                    .unwrap()
                    .len(),
                1,
                "case `{case}`: non-implementation kinds must survive the gate",
            );
            let update_argv = bd_calls
                .iter()
                .find(|argv| argv.first().is_some_and(|a| a == "update"))
                .unwrap_or_else(|| panic!("case `{case}`: bd update call missing: {bd_calls:?}"));
            assert_eq!(update_argv[1], "lm-alpha");
            let pos = update_argv
                .iter()
                .position(|a| a == "--set-metadata")
                .unwrap_or_else(|| {
                    panic!("case `{case}`: --set-metadata flag missing in argv: {update_argv:?}")
                });
            assert_eq!(
                update_argv[pos + 1],
                format!("loom.base_commit={head_after_seed}"),
            );
        } else {
            assert_eq!(
                mol.base_commit,
                Some("old-sha".to_string()),
                "case `{case}`: non-productive terminal state must leave molecules.base_commit untouched",
            );
            assert_eq!(
                impl_notes_left, 2,
                "case `{case}`: non-productive terminal state must leave implementation notes intact",
            );
            let advanced_base_commit = bd_calls
                .iter()
                .flat_map(|argv| argv.iter())
                .any(|a| a.starts_with("loom.base_commit="));
            assert!(
                !advanced_base_commit,
                "case `{case}`: non-productive terminal state must not advance loom.base_commit: {bd_calls:?}",
            );
        }
    }
}

/// Spec gate (`specs/harness.md` § Marker routing for `loom todo`):
/// `LOOM_CLARIFY` emitted from a `loom todo_new` / `loom todo_update`
/// session MUST target the molecule epic — not the bead the agent
/// was working on — per templates.md Decomposition Discipline. The
/// agent has already persisted its `## Options — …` block to the
/// epic's notes per gate.md's Options Format Contract; the driver
/// stamps `loom:clarify` + status=blocked on the epic so `bd ready`
/// excludes it until a human resolves via `loom msg`.
#[tokio::test]
async fn todo_clarify_marks_molecule_epic() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    let label = "alpha";
    let epic_id = "lm-alpha";
    let git = init_repo(&workspace);
    let state = seeded_state(&workspace, label, epic_id, None);

    let runner = CapturingRunner::new([
        // bd list (resolve_open_epic) → the seeded epic
        epic_response(epic_id, label, None),
        // bd update with loom:clarify label
        RunOutput {
            status: 0,
            stdout: Vec::new(),
            stderr: Vec::new(),
        },
    ]);
    let bd = Arc::new(BdClient::with_runner(runner.clone()));
    let manifest = stub_manifest(&workspace);
    let mut ctrl = ProductionTodoController::new(
        SpecLabel::new(label),
        workspace,
        state,
        manifest,
        ProfileName::new("base"),
        git,
        bd,
        None,
    );

    let outcome = SessionOutcome {
        exit_code: 0,
        cost_usd: None,
    };
    let marker = ExitSignal::Clarify {
        question: "additive-only or breaking?".into(),
    };
    ctrl.record_outcome(&outcome, Some(&marker))
        .await
        .expect("record_outcome ok");

    let calls = runner.calls();
    assert!(
        !calls.is_empty(),
        "LOOM_CLARIFY MUST trigger a bd update on the molecule epic; got: {calls:?}",
    );
    let argv = calls
        .iter()
        .find(|argv| argv.first().map(String::as_str) == Some("update"))
        .expect("a `bd update` call must target the epic");
    assert_eq!(argv[1], epic_id, "update must target the molecule epic id");
    assert!(
        argv.iter().any(|a| a == "loom:clarify"),
        "update must add loom:clarify label: {argv:?}",
    );
    assert!(
        argv.windows(2)
            .any(|w| w[0] == "--status" && w[1] == "blocked"),
        "update must pair status=blocked with the label: {argv:?}",
    );
}

/// `specs/harness.md` *Workflow commands*: `loom todo` fans out across
/// every spec whose markdown differs from `HEAD`; touched specs that
/// span different molecules (or mix has-open-epic with no-open-epic)
/// produce a multi-spec collision. Loom mints nothing; it creates a
/// `loom:clarify` bead carrying a structured `## Options — …` block
/// per gate.md's *Options Format Contract* and exits without dispatch.
#[tokio::test(flavor = "multi_thread")]
async fn todo_fans_out_across_all_touched_specs_and_clarifies_on_collision() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(workspace.join("specs/alpha.md"), "# alpha\n").unwrap();
    std::fs::write(workspace.join("specs/beta.md"), "# beta\n").unwrap();
    let git = init_repo(&workspace);
    run_git(&workspace, &["add", "specs"]);
    run_git(&workspace, &["commit", "-q", "-m", "seed specs"]);

    // Touch both specs vs HEAD to populate the touched set.
    std::fs::write(workspace.join("specs/alpha.md"), "# alpha\n\nalpha edit\n").unwrap();
    std::fs::write(workspace.join("specs/beta.md"), "# beta\n\nbeta edit\n").unwrap();

    let state = empty_state(&workspace);
    let manifest = stub_manifest(&workspace);

    // Two touched specs, two distinct molecules → collision.
    // The order of `touched_specs` follows `git diff --name-only`'s
    // alphabetical output, so alpha is queried first.
    let runner = CapturingRunner::new([
        epic_response_with_parent("lm-alphae", "alpha", Some("lm-mola")),
        epic_response_with_parent("lm-betae", "beta", Some("lm-molb")),
        // bd create --silent for the clarify bead.
        create_silent_response("lm-clarify1"),
    ]);
    let runner_handle = runner.clone();
    let bd = Arc::new(BdClient::with_runner(runner));
    let mut ctrl = ProductionTodoController::new(
        SpecLabel::new("alpha"),
        workspace,
        state,
        manifest,
        ProfileName::new("base"),
        git,
        bd,
        None,
    );

    let err = match ctrl.build_session().await {
        Ok(_) => panic!("expected MultiSpecCollision, got Ok"),
        Err(e) => e,
    };
    match &err {
        TodoError::MultiSpecCollision { clarify_id } => {
            assert_eq!(clarify_id, "lm-clarify1");
        }
        other => panic!("expected MultiSpecCollision, got {other:?}"),
    }

    let calls = runner_handle.calls();
    // Must include a `bd create` call for the clarify bead.
    let create_argv = calls
        .iter()
        .find(|argv| argv.first().map(String::as_str) == Some("create"))
        .expect("collision must trigger a `bd create` call");
    assert!(
        create_argv.iter().any(|a| a == "--type") && create_argv.iter().any(|a| a == "task"),
        "clarify bead must be created as a task: {create_argv:?}",
    );
    assert!(
        create_argv.iter().any(|a| a == "--labels"),
        "clarify bead must carry labels flag: {create_argv:?}",
    );
    let labels_pos = create_argv
        .iter()
        .position(|a| a == "--labels")
        .expect("labels flag present");
    assert!(
        create_argv[labels_pos + 1].contains("loom:clarify"),
        "clarify bead must carry the loom:clarify label: {create_argv:?}",
    );
    // The description must carry the canonical `## Options — …` block.
    let desc_pos = create_argv
        .iter()
        .position(|a| a == "--description")
        .expect("description flag present");
    let description = &create_argv[desc_pos + 1];
    assert!(
        description.starts_with("## Options — "),
        "clarify description must lead with the Options Format Contract header: {description}",
    );
    assert!(
        description.contains("### Option 1"),
        "clarify description must enumerate options: {description}",
    );
    assert!(
        description.contains("lm-mola") || description.contains("lm-molb"),
        "clarify description must reference pre-existing molecule ids: {description}",
    );
}

/// `specs/harness.md` *Workflow commands*: the same multi-spec fan-out
/// classifier flags collisions when one touched spec has an open epic
/// and another does not — even though both individual single-tier
/// resolutions are well-formed. Loom mints nothing; clarify bead carries
/// the options block.
#[tokio::test(flavor = "multi_thread")]
async fn todo_fans_across_touched_specs_and_clarifies_on_collision() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(workspace.join("specs/gamma.md"), "# gamma\n").unwrap();
    std::fs::write(workspace.join("specs/delta.md"), "# delta\n").unwrap();
    let git = init_repo(&workspace);
    run_git(&workspace, &["add", "specs"]);
    run_git(&workspace, &["commit", "-q", "-m", "seed specs"]);

    std::fs::write(workspace.join("specs/gamma.md"), "# gamma\n\ngamma edit\n").unwrap();
    std::fs::write(workspace.join("specs/delta.md"), "# delta\n\ndelta edit\n").unwrap();

    let state = empty_state(&workspace);
    let manifest = stub_manifest(&workspace);

    // Mixed has-/has-not-open-epic: delta has an existing molecule,
    // gamma does not. The classifier flags this as a collision per
    // FR1's "mix has/has-not open epics" rule.
    let runner = CapturingRunner::new([
        // touched_specs order from `git diff --name-only` is
        // alphabetical: delta before gamma.
        epic_response_with_parent("lm-deltae", "delta", Some("lm-mold")),
        empty_epic_response(),
        create_silent_response("lm-clarify2"),
    ]);
    let runner_handle = runner.clone();
    let bd = Arc::new(BdClient::with_runner(runner));
    let mut ctrl = ProductionTodoController::new(
        SpecLabel::new("delta"),
        workspace,
        state,
        manifest,
        ProfileName::new("base"),
        git,
        bd,
        None,
    );

    let err = match ctrl.build_session().await {
        Ok(_) => panic!("expected MultiSpecCollision, got Ok"),
        Err(e) => e,
    };
    assert!(
        matches!(err, TodoError::MultiSpecCollision { .. }),
        "mix has/has-not must be a collision: {err:?}",
    );

    let calls = runner_handle.calls();
    let create_argv = calls
        .iter()
        .find(|argv| argv.first().map(String::as_str) == Some("create"))
        .expect("mix-has/has-not must trigger a `bd create` clarify");
    let desc_pos = create_argv
        .iter()
        .position(|a| a == "--description")
        .expect("description present");
    let description = &create_argv[desc_pos + 1];
    assert!(
        description.contains("Close existing epics and mint a fresh cross-cutting molecule"),
        "options block must offer the fresh-mint resolution: {description}",
    );
}

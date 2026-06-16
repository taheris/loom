//! Integration coverage for deterministic `loom todo` preflight.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use loom_driver::agent::SessionOutcome;
use loom_driver::bd::{BdClient, BdError, CommandRunner, RunOutput};
use loom_driver::git::GitClient;
use loom_driver::identifier::ProfileName;
use loom_driver::profile_manifest::ProfileImageManifest;
use loom_driver::state::CacheDb;
use loom_protocol::todo::{TODO_SUCCESS_PREFIX, parse_todo_success};
use loom_workflow::todo::{ExitSignal, ProductionTodoController, TodoController, TodoError};

fn run_git(workspace: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .status()?;
    if !status.success() {
        return Err(anyhow!("git {args:?} failed: {status}"));
    }
    Ok(())
}

fn git_output(workspace: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(anyhow!("git {args:?} failed"));
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn init_workspace(workspace: &Path) -> Result<(String, String)> {
    run_git(workspace, &["init", "-q", "-b", "main"])?;
    run_git(workspace, &["config", "user.email", "test@example.com"])?;
    run_git(workspace, &["config", "user.name", "Test"])?;
    run_git(workspace, &["config", "commit.gpgsign", "false"])?;
    std::fs::create_dir_all(workspace.join("docs"))?;
    std::fs::create_dir_all(workspace.join("specs"))?;
    std::fs::write(
        workspace.join("docs/README.md"),
        "- [Alpha](../specs/alpha.md)\n- [Beta](../specs/beta.md)\n",
    )?;
    std::fs::write(workspace.join("specs/alpha.md"), "# Alpha\n")?;
    std::fs::write(workspace.join("specs/beta.md"), "# Beta\n")?;
    run_git(
        workspace,
        &["add", "docs/README.md", "specs/alpha.md", "specs/beta.md"],
    )?;
    run_git(workspace, &["commit", "-q", "-m", "seed specs"])?;
    let base = git_output(workspace, &["rev-parse", "HEAD"])?;
    std::fs::write(workspace.join("specs/alpha.md"), "# Alpha\n\nchanged\n")?;
    std::fs::write(
        workspace.join("docs/README.md"),
        "- [Alpha](../specs/alpha.md)\n- [Beta](../specs/beta.md)\n- [Gamma](../specs/gamma.md)\n",
    )?;
    std::fs::write(workspace.join("specs/gamma.md"), "# Gamma\n")?;
    run_git(
        workspace,
        &["add", "docs/README.md", "specs/alpha.md", "specs/gamma.md"],
    )?;
    run_git(workspace, &["commit", "-q", "-m", "change alpha add gamma"])?;
    let head = git_output(workspace, &["rev-parse", "HEAD"])?;
    Ok((base, head))
}

fn manifest(dir: &Path) -> Result<Arc<ProfileImageManifest>> {
    let body = r#"{
      "base": {
        "pi": { "ref": "localhost/wrix-base-pi:abc", "source": "/nix/store/aaa-image-base-pi" },
        "claude": { "ref": "localhost/wrix-base-claude:abc", "source": "/nix/store/aaa-image-base-claude" },
        "direct": { "ref": "localhost/wrix-base-direct:abc", "source": "/nix/store/aaa-image-base-direct" }
      }
    }"#;
    let path = dir.join("profile-images.json");
    std::fs::write(&path, body)?;
    Ok(Arc::new(ProfileImageManifest::from_path(&path)?))
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

    fn calls(&self) -> Result<Vec<Vec<String>>> {
        Ok(self
            .calls
            .lock()
            .map_err(|_| anyhow!("calls lock poisoned"))?
            .iter()
            .map(|argv| {
                argv.iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect()
            })
            .collect())
    }
}

impl CommandRunner for CapturingRunner {
    async fn run(&self, args: Vec<OsString>, _timeout: Duration) -> Result<RunOutput, BdError> {
        self.calls
            .lock()
            .map_err(|_| BdError::Cli {
                status: 1,
                args: "bd fake".to_string(),
                stderr: "calls lock poisoned".to_string(),
            })?
            .push(args);
        let response = self
            .responses
            .lock()
            .map_err(|_| BdError::Cli {
                status: 1,
                args: "bd fake".to_string(),
                stderr: "responses lock poisoned".to_string(),
            })?
            .pop_front();
        Ok(response.unwrap_or_else(empty_json))
    }
}

fn empty_json() -> RunOutput {
    ok("[]")
}

fn ok(stdout: &str) -> RunOutput {
    RunOutput {
        status: 0,
        stdout: stdout.as_bytes().to_vec(),
        stderr: Vec::new(),
    }
}

fn spec_epic(id: &str, label: &str, cursor: &str) -> RunOutput {
    ok(&format!(
        r#"[{{"id":"{id}","title":"{label}","status":"open","issue_type":"epic","labels":["loom:spec","spec:{label}"],"metadata":{{"loom.todo_cursor":"{cursor}"}}}}]"#
    ))
}

fn pending_todo(id: &str, head: &str, fingerprint: &str) -> RunOutput {
    ok(&format!(
        r#"[{{"id":"{id}","title":"todo","status":"open","issue_type":"epic","labels":["loom:todo"],"metadata":{{"loom.todo_head":"{head}","loom.todo_fingerprint":"{fingerprint}"}}}}]"#
    ))
}

fn created(id: &str) -> RunOutput {
    ok(&format!("{id}\n"))
}

fn preflight_responses(base: &str, head: &str) -> Vec<RunOutput> {
    vec![
        spec_epic("lm-alpha", "alpha", base),
        empty_json(),
        empty_json(),
        spec_epic("lm-beta", "beta", head),
        empty_json(),
        empty_json(),
        empty_json(),
        empty_json(),
        empty_json(),
        created("lm-gamma"),
        empty_json(),
        created("lm-work"),
    ]
}

fn controller(
    workspace: &Path,
    runner: CapturingRunner,
) -> Result<ProductionTodoController<CapturingRunner>> {
    Ok(ProductionTodoController::for_workspace(
        workspace.to_path_buf(),
        Arc::new(CacheDb::open(workspace.join(".loom/cache.db"))?),
        manifest(workspace)?,
        ProfileName::new("base"),
        Arc::new(GitClient::open(workspace)?),
        Arc::new(BdClient::with_runner(runner)),
        None,
    ))
}

fn field(prompt: &str, name: &str) -> Result<String> {
    let prefix = format!("- **{name}**: ");
    prompt
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("field `{name}` missing"))
}

fn todo_success(prompt: &str, specs: &[&str]) -> Result<loom_protocol::todo::TodoSuccess> {
    let head = field(prompt, "Todo head")?;
    let fingerprint = field(prompt, "Todo fingerprint")?;
    let work_epic = field(prompt, "Work epic")?;
    let spec_json = specs
        .iter()
        .map(|label| format!(r#"{{"label":"{label}","outcome":"no-work","reason":"audited"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    Ok(parse_todo_success(&format!(
        "{TODO_SUCCESS_PREFIX}{{\"head\":\"{head}\",\"fingerprint\":\"{fingerprint}\",\"work_epic\":\"{work_epic}\",\"specs\":[{spec_json}]}}"
    ))?)
}

#[tokio::test]
async fn todo_discovers_active_inactive_and_new_specs_from_cursors() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (base, head) = init_workspace(dir.path())?;
    let runner = CapturingRunner::new(preflight_responses(&base, &head));
    let calls = runner.clone();
    let mut ctrl = controller(dir.path(), runner)?;

    let session = ctrl.build_session().await?;
    let prompt = session.config.initial_prompt;

    assert!(prompt.contains("### alpha"), "prompt: {prompt}");
    assert!(prompt.contains("### gamma"), "prompt: {prompt}");
    assert!(!prompt.contains("### beta"), "prompt: {prompt}");
    let all_calls = calls.calls()?;
    assert!(
        all_calls
            .iter()
            .any(|argv| argv.iter().any(|arg| arg.contains("loom:todo")))
    );
    assert!(
        all_calls
            .iter()
            .any(|argv| argv.iter().any(|arg| arg == "loom:spec,spec:gamma"))
    );
    Ok(())
}

#[tokio::test]
async fn generic_todo_marker_is_rejected_without_advancing() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (base, head) = init_workspace(dir.path())?;
    let runner = CapturingRunner::new(preflight_responses(&base, &head));
    let mut ctrl = controller(dir.path(), runner)?;
    let _session = ctrl.build_session().await?;

    let err = match ctrl
        .record_outcome(
            &SessionOutcome {
                exit_code: 0,
                cost_usd: None,
            },
            Some(&ExitSignal::Complete),
            None,
        )
        .await
    {
        Ok(()) => return Err(anyhow!("generic marker was accepted")),
        Err(err) => err,
    };

    assert!(matches!(err, TodoError::GenericTodoMarker));
    Ok(())
}

#[tokio::test]
async fn omitted_spec_payload_is_rejected_before_finalization() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (base, head) = init_workspace(dir.path())?;
    let runner = CapturingRunner::new(preflight_responses(&base, &head));
    let calls = runner.clone();
    let mut ctrl = controller(dir.path(), runner)?;
    let session = ctrl.build_session().await?;
    let success = todo_success(&session.config.initial_prompt, &["alpha"])?;

    let err = match ctrl
        .record_outcome(
            &SessionOutcome {
                exit_code: 0,
                cost_usd: None,
            },
            None,
            Some(&success),
        )
        .await
    {
        Ok(()) => return Err(anyhow!("omitted spec payload was accepted")),
        Err(err) => err,
    };

    assert!(matches!(err, TodoError::TodoValidation { .. }));
    let updates = calls
        .calls()?
        .into_iter()
        .filter(|argv| argv.first().is_some_and(|arg| arg == "update"))
        .collect::<Vec<_>>();
    assert!(updates.is_empty(), "no finalization updates: {updates:?}");
    Ok(())
}

#[tokio::test]
async fn valid_todo_success_sets_active_and_advances_all_cursors() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (base, head) = init_workspace(dir.path())?;
    let mut responses = preflight_responses(&base, &head);
    responses.push(empty_json());
    let runner = CapturingRunner::new(responses);
    let calls = runner.clone();
    let mut ctrl = controller(dir.path(), runner)?;
    let session = ctrl.build_session().await?;
    let success = todo_success(&session.config.initial_prompt, &["alpha", "gamma"])?;

    ctrl.record_outcome(
        &SessionOutcome {
            exit_code: 0,
            cost_usd: None,
        },
        None,
        Some(&success),
    )
    .await?;

    let calls = calls.calls()?;
    assert!(calls.iter().any(|argv| {
        argv == &[
            "update",
            "lm-alpha",
            "--set-metadata",
            &format!("loom.todo_cursor={head}"),
        ]
        .map(str::to_string)
    }));
    assert!(calls.iter().any(|argv| {
        argv == &[
            "update",
            "lm-gamma",
            "--set-metadata",
            &format!("loom.todo_cursor={head}"),
        ]
        .map(str::to_string)
    }));
    assert!(
        calls
            .iter()
            .any(|argv| argv.iter().any(|arg| arg == "--add-label")
                && argv.iter().any(|arg| arg == "loom:active"))
    );
    assert!(
        calls
            .iter()
            .any(|argv| argv.iter().any(|arg| arg == "--remove-label")
                && argv.iter().any(|arg| arg == "loom:todo"))
    );
    Ok(())
}

#[tokio::test]
async fn todo_reuses_matching_pending_work_epic_else_blocks() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (base, head) = init_workspace(dir.path())?;
    let mut initial = controller(
        dir.path(),
        CapturingRunner::new(preflight_responses(&base, &head)),
    )?;
    let initial_session = initial.build_session().await?;
    let todo_head = field(&initial_session.config.initial_prompt, "Todo head")?;
    let fingerprint = field(&initial_session.config.initial_prompt, "Todo fingerprint")?;

    let mut responses = preflight_responses(&base, &head);
    responses.pop();
    responses.pop();
    responses.push(pending_todo("lm-pending", &todo_head, &fingerprint));
    let mut ctrl = controller(dir.path(), CapturingRunner::new(responses))?;
    let session = ctrl.build_session().await?;
    assert_eq!(
        field(&session.config.initial_prompt, "Work epic")?,
        "lm-pending"
    );

    let conflict_dir = tempfile::tempdir()?;
    let (conflict_base, conflict_head) = init_workspace(conflict_dir.path())?;
    let mut responses = preflight_responses(&conflict_base, &conflict_head);
    responses.pop();
    responses.pop();
    responses.push(pending_todo(
        "lm-pending",
        "0123456789abcdef0123456789abcdef01234567",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ));
    let mut ctrl = controller(conflict_dir.path(), CapturingRunner::new(responses))?;

    let err = match ctrl.build_session().await {
        Ok(_) => panic!("pending mismatch should block"),
        Err(err) => err,
    };

    match err {
        TodoError::PendingTodoEpicConflict { diagnostic, .. } => {
            assert!(diagnostic.contains("## Options"), "{diagnostic}");
        }
        other => return Err(anyhow!("expected pending conflict, got {other:?}")),
    }
    Ok(())
}

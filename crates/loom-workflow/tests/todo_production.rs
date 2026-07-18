//! Integration coverage for deterministic `loom todo` preflight.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use loom_driver::agent::SessionOutcome;
use loom_driver::bd::{BdClient, BdError, CommandRunner, RunOutput};
use loom_driver::git::GitClient;
use loom_driver::identifier::{MoleculeId, ProfileName, SpecLabel};
use loom_driver::logging::phase_log_path;
use loom_driver::profile_manifest::ProfileImageManifest;
use loom_driver::state::{CacheDb, WorkEpicRow};
use loom_protocol::todo::{TODO_SUCCESS_PREFIX, parse_todo_success};
use loom_workflow::todo::{
    ExitSignal, ProductionTodoController, TodoController, TodoError, run as run_todo_workflow,
};

fn git_command() -> Command {
    let mut command = Command::new("git");
    loom_test_support::scrub_git_local_env(&mut command);
    command
}

fn run_git(workspace: &Path, args: &[&str]) -> Result<()> {
    let status = git_command().arg("-C").arg(workspace).args(args).status()?;
    if !status.success() {
        return Err(anyhow!("git {args:?} failed: {status}"));
    }
    Ok(())
}

fn git_output(workspace: &Path, args: &[&str]) -> Result<String> {
    let output = git_command().arg("-C").arg(workspace).args(args).output()?;
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
        "pi": { "ref": "localhost/wrix-base-pi:abc", "source": "/nix/store/aaa-image-base-pi", "source_kind": "nix-descriptor" },
        "claude": { "ref": "localhost/wrix-base-claude:abc", "source": "/nix/store/aaa-image-base-claude", "source_kind": "nix-descriptor" },
        "direct": { "ref": "localhost/wrix-base-direct:abc", "source": "/nix/store/aaa-image-base-direct", "source_kind": "nix-descriptor" }
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

#[derive(Debug, Clone, serde::Serialize)]
struct FakeBead {
    id: String,
    title: String,
    status: String,
    issue_type: String,
    labels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
    metadata: BTreeMap<String, serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
}

#[derive(Default)]
struct StatefulBd {
    beads: BTreeMap<String, FakeBead>,
    calls: Vec<Vec<String>>,
    update_attempt: usize,
    fail_update: Option<usize>,
}

#[derive(Clone)]
struct StatefulRunner {
    state: Arc<Mutex<StatefulBd>>,
}

impl StatefulRunner {
    fn new(base: &str, fail_update: Option<usize>) -> Self {
        let mut beads = BTreeMap::new();
        for (id, label) in [("lm-alpha", "alpha"), ("lm-beta", "beta")] {
            beads.insert(
                id.to_string(),
                FakeBead {
                    id: id.to_string(),
                    title: label.to_string(),
                    status: "closed".to_string(),
                    issue_type: "epic".to_string(),
                    labels: vec!["loom:spec".to_string(), format!("spec:{label}")],
                    parent: None,
                    metadata: BTreeMap::from([(
                        "loom.todo_cursor".to_string(),
                        serde_json::Value::String(base.to_string()),
                    )]),
                    notes: None,
                },
            );
        }
        beads.insert(
            "lm-oldactive".to_string(),
            FakeBead {
                id: "lm-oldactive".to_string(),
                title: "Previous work".to_string(),
                status: "open".to_string(),
                issue_type: "epic".to_string(),
                labels: vec!["loom:active".to_string()],
                parent: None,
                metadata: BTreeMap::new(),
                notes: None,
            },
        );
        Self {
            state: Arc::new(Mutex::new(StatefulBd {
                beads,
                fail_update,
                ..StatefulBd::default()
            })),
        }
    }

    fn bead(&self, id: &str) -> Result<FakeBead> {
        self.state
            .lock()
            .map_err(|_| anyhow!("stateful runner lock poisoned"))?
            .beads
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("fake bead `{id}` not found"))
    }
}

impl CommandRunner for StatefulRunner {
    async fn run(&self, args: Vec<OsString>, _timeout: Duration) -> Result<RunOutput, BdError> {
        let argv = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let mut state = self.state.lock().map_err(|_| BdError::Cli {
            status: 1,
            args: "bd fake".to_string(),
            stderr: "stateful runner lock poisoned".to_string(),
        })?;
        state.calls.push(argv.clone());
        match argv.first().map(String::as_str) {
            Some("list") => stateful_list(&state, &argv),
            Some("show") => stateful_show(&state, &argv),
            Some("create") => stateful_create(&mut state, &argv),
            Some("close") => stateful_close(&mut state, &argv),
            Some("update") => stateful_update(&mut state, &argv),
            command => Ok(failed(&format!("unsupported fake bd command {command:?}"))),
        }
    }
}

fn stateful_list(state: &StatefulBd, argv: &[String]) -> Result<RunOutput, BdError> {
    let status = prefixed_value(argv, "--status=");
    let label = prefixed_value(argv, "--label=");
    let issue_type = prefixed_value(argv, "--type=");
    let beads = state
        .beads
        .values()
        .filter(|bead| status.is_none_or(|value| bead.status == value))
        .filter(|bead| label.is_none_or(|value| bead.labels.iter().any(|label| label == value)))
        .filter(|bead| issue_type.is_none_or(|value| bead.issue_type == value))
        .cloned()
        .collect::<Vec<_>>();
    serde_json::to_vec(&beads)
        .map(|stdout| RunOutput {
            status: 0,
            stdout,
            stderr: Vec::new(),
        })
        .map_err(|source| BdError::Cli {
            status: 1,
            args: argv.join(" "),
            stderr: source.to_string(),
        })
}

fn stateful_show(state: &StatefulBd, argv: &[String]) -> Result<RunOutput, BdError> {
    let bead = argv.get(1).and_then(|id| state.beads.get(id));
    serde_json::to_vec(&bead.into_iter().collect::<Vec<_>>())
        .map(|stdout| RunOutput {
            status: 0,
            stdout,
            stderr: Vec::new(),
        })
        .map_err(|source| BdError::Cli {
            status: 1,
            args: argv.join(" "),
            stderr: source.to_string(),
        })
}

fn stateful_create(state: &mut StatefulBd, argv: &[String]) -> Result<RunOutput, BdError> {
    let labels = flag_value(argv, "--labels")
        .map(|value| value.split(',').map(ToOwned::to_owned).collect::<Vec<_>>())
        .unwrap_or_default();
    let id = if labels.iter().any(|label| label == "loom:todo") {
        "lm-work"
    } else {
        "lm-created"
    };
    let metadata = flag_value(argv, "--metadata")
        .map(serde_json::from_str)
        .transpose()
        .map_err(|source| BdError::Cli {
            status: 1,
            args: argv.join(" "),
            stderr: source.to_string(),
        })?
        .unwrap_or_default();
    state.beads.insert(
        id.to_string(),
        FakeBead {
            id: id.to_string(),
            title: flag_value(argv, "--title").unwrap_or_default().to_string(),
            status: "open".to_string(),
            issue_type: flag_value(argv, "--type").unwrap_or("task").to_string(),
            labels,
            parent: flag_value(argv, "--parent").map(ToOwned::to_owned),
            metadata,
            notes: flag_value(argv, "--notes").map(ToOwned::to_owned),
        },
    );
    Ok(created(id))
}

fn stateful_close(state: &mut StatefulBd, argv: &[String]) -> Result<RunOutput, BdError> {
    let Some(bead) = argv.get(1).and_then(|id| state.beads.get_mut(id)) else {
        return Ok(failed("missing fake bead"));
    };
    bead.status = "closed".to_string();
    Ok(closed())
}

fn stateful_update(state: &mut StatefulBd, argv: &[String]) -> Result<RunOutput, BdError> {
    state.update_attempt += 1;
    if state.fail_update == Some(state.update_attempt) {
        state.fail_update = None;
        return Ok(failed("injected update failure"));
    }
    let Some(bead) = argv.get(1).and_then(|id| state.beads.get_mut(id)) else {
        return Ok(failed("missing fake bead"));
    };
    let mut index = 2;
    while index < argv.len() {
        let value = argv.get(index + 1).cloned().unwrap_or_default();
        match argv[index].as_str() {
            "--title" => bead.title = value,
            "--status" => bead.status = value,
            "--notes" => bead.notes = Some(value),
            "--add-label" => {
                if !bead.labels.iter().any(|label| label == &value) {
                    bead.labels.push(value);
                }
            }
            "--remove-label" => bead.labels.retain(|label| label != &value),
            "--set-metadata" => {
                if let Some((key, value)) = value.split_once('=') {
                    bead.metadata.insert(
                        key.to_string(),
                        serde_json::Value::String(value.to_string()),
                    );
                }
            }
            "--unset-metadata" => {
                bead.metadata.remove(&value);
            }
            _ => return Ok(failed("unsupported fake update flag")),
        }
        index += 2;
    }
    Ok(empty_json())
}

fn prefixed_value<'a>(argv: &'a [String], prefix: &str) -> Option<&'a str> {
    argv.iter().find_map(|arg| arg.strip_prefix(prefix))
}

fn failed(stderr: &str) -> RunOutput {
    RunOutput {
        status: 1,
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
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

fn spec_epic_without_cursor(id: &str, label: &str) -> RunOutput {
    ok(&format!(
        r#"[{{"id":"{id}","title":"{label}","status":"open","issue_type":"epic","labels":["loom:spec","spec:{label}"],"metadata":{{}}}}]"#
    ))
}

fn child_bead(id: &str, parent: &str, notes: Option<&str>) -> RunOutput {
    let notes = notes
        .map(|text| {
            format!(
                r#","notes":{}"#,
                serde_json::Value::String(text.to_string())
            )
        })
        .unwrap_or_default();
    ok(&format!(
        r#"[{{"id":"{id}","title":"child","status":"open","issue_type":"task","parent":"{parent}","labels":[]{notes}}}]"#
    ))
}

fn active_work_epic(id: &str) -> RunOutput {
    ok(&format!(
        r#"[{{"id":"{id}","title":"active","status":"open","issue_type":"epic","labels":["loom:active"]}}]"#
    ))
}

fn work_epic_with_notes(id: &str, notes: &str) -> RunOutput {
    let notes_json = serde_json::Value::String(notes.to_string());
    ok(&format!(
        r#"[{{"id":"{id}","title":"todo","status":"open","issue_type":"epic","description":"plain","labels":["loom:todo"],"notes":{notes_json}}}]"#
    ))
}

fn created(id: &str) -> RunOutput {
    ok(&format!("{id}\n"))
}

fn closed() -> RunOutput {
    ok("")
}

fn preflight_responses(base: &str, head: &str) -> Vec<RunOutput> {
    vec![
        spec_epic("lm-alpha", "alpha", base),
        closed(),
        empty_json(),
        empty_json(),
        spec_epic("lm-beta", "beta", head),
        closed(),
        empty_json(),
        empty_json(),
        empty_json(),
        empty_json(),
        empty_json(),
        created("lm-gamma"),
        closed(),
        empty_json(),
        created("lm-work"),
    ]
}

fn init_two_changed_workspace(workspace: &Path) -> Result<(String, String)> {
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
    std::fs::write(workspace.join("specs/beta.md"), "# Beta\n\nchanged\n")?;
    run_git(workspace, &["add", "specs/alpha.md", "specs/beta.md"])?;
    run_git(workspace, &["commit", "-q", "-m", "change both specs"])?;
    let head = git_output(workspace, &["rev-parse", "HEAD"])?;
    Ok((base, head))
}

fn init_multi_spec_workspace(workspace: &Path) -> Result<(String, String)> {
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
        workspace.join("specs/beta.md"),
        "# Beta\n\nstale cursor changed\n",
    )?;
    std::fs::write(
        workspace.join("docs/README.md"),
        "- [Alpha](../specs/alpha.md)\n- [Beta](../specs/beta.md)\n- [Gamma](../specs/gamma.md)\n",
    )?;
    std::fs::write(workspace.join("specs/gamma.md"), "# Gamma\n")?;
    run_git(
        workspace,
        &[
            "add",
            "docs/README.md",
            "specs/alpha.md",
            "specs/beta.md",
            "specs/gamma.md",
        ],
    )?;
    run_git(workspace, &["commit", "-q", "-m", "change all specs"])?;
    let head = git_output(workspace, &["rev-parse", "HEAD"])?;
    Ok((base, head))
}

fn multi_spec_preflight_responses(base: &str) -> Vec<RunOutput> {
    vec![
        spec_epic("lm-alpha", "alpha", base),
        closed(),
        empty_json(),
        empty_json(),
        spec_epic("lm-beta", "beta", base),
        closed(),
        empty_json(),
        empty_json(),
        empty_json(),
        empty_json(),
        empty_json(),
        created("lm-gamma"),
        closed(),
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

fn flag_value<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    argv.iter()
        .position(|arg| arg == flag)
        .and_then(|idx| argv.get(idx + 1))
        .map(String::as_str)
}

fn todo_success(prompt: &str, specs: &[&str]) -> Result<loom_protocol::todo::TodoSuccess> {
    let spec_json = specs
        .iter()
        .map(|label| format!(r#"{{"label":"{label}","outcome":"no-work","reason":"audited"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    todo_success_with_specs(prompt, &spec_json)
}

fn todo_success_with_specs(
    prompt: &str,
    spec_json: &str,
) -> Result<loom_protocol::todo::TodoSuccess> {
    let head = field(prompt, "Todo head")?;
    let fingerprint = field(prompt, "Todo fingerprint")?;
    let work_epic = field(prompt, "Work epic")?;
    Ok(parse_todo_success(&format!(
        "{TODO_SUCCESS_PREFIX}{{\"head\":\"{head}\",\"fingerprint\":\"{fingerprint}\",\"work_epic\":\"{work_epic}\",\"title\":\"Pin changed spec follow-ups\",\"specs\":[{spec_json}]}}"
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
async fn todo_preflight_discovers_active_inactive_and_new_specs() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (base, _head) = init_multi_spec_workspace(dir.path())?;
    let runner = CapturingRunner::new(multi_spec_preflight_responses(&base));
    let calls = runner.clone();
    let mut ctrl = controller(dir.path(), runner)?;

    let session = ctrl.build_session().await?;
    let prompt = session.config.initial_prompt;

    assert!(prompt.contains("### alpha"), "prompt: {prompt}");
    assert!(prompt.contains("### beta"), "prompt: {prompt}");
    assert!(prompt.contains("### gamma"), "prompt: {prompt}");
    let all_calls = calls.calls()?;
    assert!(
        all_calls
            .iter()
            .any(|argv| argv.iter().any(|arg| arg == "loom:spec,spec:gamma"))
    );
    assert!(
        all_calls
            .iter()
            .all(|argv| argv.iter().all(|arg| arg != "loom:active")),
        "preflight must not discover changed specs through loom:active: {all_calls:?}",
    );
    Ok(())
}

#[tokio::test]
async fn todo_preflight_closes_spec_metadata_epics() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (base, head) = init_workspace(dir.path())?;
    let runner = CapturingRunner::new(preflight_responses(&base, &head));
    let calls = runner.clone();
    let mut ctrl = controller(dir.path(), runner)?;

    let _session = ctrl.build_session().await?;

    let close_calls = calls
        .calls()?
        .into_iter()
        .filter(|argv| argv.first().is_some_and(|arg| arg == "close"))
        .collect::<Vec<_>>();
    for id in ["lm-alpha", "lm-beta", "lm-gamma"] {
        assert!(
            close_calls.iter().any(|argv| argv
                == &["close", id, "--reason", "spec metadata carrier"].map(str::to_string)),
            "missing close for {id}: {close_calls:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn todo_prompt_uses_container_visible_scratchpad_path() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (base, head) = init_workspace(dir.path())?;
    let runner = CapturingRunner::new(preflight_responses(&base, &head));
    let mut ctrl = controller(dir.path(), runner)?;

    let session = ctrl.build_session().await?;
    let prompt = session.config.initial_prompt;
    let host_root = dir.path().to_string_lossy();

    assert!(
        prompt.contains("`/workspace/.loom/scratch/lm-work/scratch.md`"),
        "prompt should name the container-visible scratchpad path: {prompt}"
    );
    assert!(
        !prompt.contains(host_root.as_ref()),
        "prompt leaked host workspace path into container instructions: {prompt}"
    );
    assert_eq!(
        session.config.scratch_dir,
        dir.path().join(".loom/scratch/lm-work"),
        "spawn config must keep the host scratch dir for driver-side re-pin"
    );
    Ok(())
}

#[tokio::test]
async fn todo_creates_pending_work_epic_before_agent_prompt() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (base, _head) = init_multi_spec_workspace(dir.path())?;
    let runner = CapturingRunner::new(multi_spec_preflight_responses(&base));
    let calls = runner.clone();
    let mut ctrl = controller(dir.path(), runner)?;

    let session = ctrl.build_session().await?;

    let all_calls = calls.calls()?;
    let work_create = all_calls
        .iter()
        .find(|argv| {
            argv.first().is_some_and(|arg| arg == "create")
                && flag_value(argv, "--labels").is_some_and(|labels| labels.contains("loom:todo"))
        })
        .ok_or_else(|| anyhow!("work epic create call missing: {all_calls:?}"))?;
    assert_eq!(
        flag_value(work_create, "--title"),
        Some("Pending todo decomposition")
    );
    let labels = flag_value(work_create, "--labels")
        .ok_or_else(|| anyhow!("work epic labels missing: {work_create:?}"))?;
    assert!(labels.contains("loom:todo"), "labels: {labels}");
    assert!(!labels.contains("loom:active"), "labels: {labels}");
    let metadata = flag_value(work_create, "--metadata")
        .ok_or_else(|| anyhow!("work epic metadata missing: {work_create:?}"))?;
    assert!(metadata.contains("loom.todo_head"), "metadata: {metadata}");
    assert!(
        metadata.contains("loom.todo_fingerprint"),
        "metadata: {metadata}"
    );
    assert_eq!(
        field(&session.config.initial_prompt, "Work epic")?,
        "lm-work"
    );
    Ok(())
}

#[tokio::test]
async fn todo_preflight_rejects_unindexed_spec_file() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (_base, _head) = init_workspace(dir.path())?;
    std::fs::write(dir.path().join("specs/services.md"), "# Services\n")?;
    let mut ctrl = controller(dir.path(), CapturingRunner::new(Vec::<RunOutput>::new()))?;

    let err = match ctrl.build_session().await {
        Ok(_) => return Err(anyhow!("unindexed spec file was accepted")),
        Err(err) => err,
    };

    match err {
        TodoError::SpecIndex { detail } => {
            assert!(detail.contains("specs/services.md"), "detail: {detail}");
            assert!(detail.contains("docs/README.md"), "detail: {detail}");
        }
        other => return Err(anyhow!("expected SpecIndex, got {other:?}")),
    }
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
        Ok(_) => return Err(anyhow!("generic marker was accepted")),
        Err(err) => err,
    };

    assert!(matches!(err, TodoError::GenericTodoMarker));
    Ok(())
}

#[tokio::test]
async fn missing_todo_success_marker_fails_without_advancing() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (base, head) = init_workspace(dir.path())?;
    let runner = CapturingRunner::new(preflight_responses(&base, &head));
    let calls = runner.clone();
    let mut ctrl = controller(dir.path(), runner)?;
    let _session = ctrl.build_session().await?;

    let err = match ctrl
        .record_outcome(
            &SessionOutcome {
                exit_code: 0,
                cost_usd: None,
            },
            None,
            None,
        )
        .await
    {
        Ok(_) => return Err(anyhow!("missing LOOM_TODO marker was accepted")),
        Err(err) => err,
    };

    assert!(matches!(err, TodoError::MissingExitSignal));
    let updates = calls
        .calls()?
        .into_iter()
        .filter(|argv| argv.first().is_some_and(|arg| arg == "update"))
        .collect::<Vec<_>>();
    assert!(
        updates
            .iter()
            .all(|argv| !argv.iter().any(|arg| arg == "--set-metadata")),
        "no cursor writes on missing marker: {updates:?}",
    );
    assert!(
        updates
            .iter()
            .all(|argv| !argv.iter().any(|arg| arg == "loom:active")),
        "active state unchanged on missing marker: {updates:?}",
    );
    assert!(
        updates
            .iter()
            .any(|argv| argv.iter().any(|arg| arg.contains("no terminal signal"))),
        "missing marker diagnostic is recorded on the work epic: {updates:?}",
    );
    Ok(())
}

async fn assert_todo_validation_failure_leaves_pending_without_advancing() -> Result<()> {
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
        Ok(_) => return Err(anyhow!("omitted spec payload was accepted")),
        Err(err) => err,
    };

    assert!(matches!(err, TodoError::TodoValidation { .. }));
    let updates = calls
        .calls()?
        .into_iter()
        .filter(|argv| argv.first().is_some_and(|arg| arg == "update"))
        .collect::<Vec<_>>();
    assert!(
        updates.iter().any(|argv| argv
            .iter()
            .any(|arg| arg.contains("LOOM_TODO validation failed"))),
        "diagnostic update expected: {updates:?}",
    );
    assert!(
        updates
            .iter()
            .all(|argv| !argv.iter().any(|arg| arg == "--set-metadata")),
        "no cursor writes on validation failure: {updates:?}",
    );
    assert!(
        updates
            .iter()
            .all(|argv| !argv.iter().any(|arg| arg == "loom:active")),
        "active state unchanged on validation failure: {updates:?}",
    );
    Ok(())
}

#[tokio::test]
async fn todo_validation_failure_leaves_pending_without_advancing() -> Result<()> {
    assert_todo_validation_failure_leaves_pending_without_advancing().await
}

#[tokio::test]
async fn todo_success_missing_changed_spec_fails_without_advancing() -> Result<()> {
    assert_todo_validation_failure_leaves_pending_without_advancing().await
}

#[derive(Debug, Clone, Copy)]
enum ValidationCase {
    MissingSpec,
    ExtraSpec,
    MissingBead,
    MisparentedBead,
}

async fn assert_todo_validation_case(case: ValidationCase) -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (base, head) = init_workspace(dir.path())?;
    let mut responses = preflight_responses(&base, &head);
    if matches!(case, ValidationCase::MisparentedBead) {
        responses.push(child_bead("lm-child", "lm-other", None));
    }
    let runner = CapturingRunner::new(responses);
    let calls = runner.clone();
    let mut ctrl = controller(dir.path(), runner)?;
    let session = ctrl.build_session().await?;
    let success = match case {
        ValidationCase::MissingSpec => todo_success(&session.config.initial_prompt, &["alpha"])?,
        ValidationCase::ExtraSpec => {
            todo_success(&session.config.initial_prompt, &["alpha", "beta", "gamma"])?
        }
        ValidationCase::MissingBead => todo_success_with_specs(
            &session.config.initial_prompt,
            r#"{"label":"alpha","outcome":"decomposed","beads":["lm-missing"]},{"label":"gamma","outcome":"no-work","reason":"audited"}"#,
        )?,
        ValidationCase::MisparentedBead => todo_success_with_specs(
            &session.config.initial_prompt,
            r#"{"label":"alpha","outcome":"decomposed","beads":["lm-child"]},{"label":"gamma","outcome":"no-work","reason":"audited"}"#,
        )?,
    };

    let error = match ctrl
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
        Ok(_) => return Err(anyhow!("invalid todo success was accepted")),
        Err(error) => error,
    };
    assert!(
        matches!(error, TodoError::TodoValidation { .. }),
        "case {case:?}: {error:?}"
    );
    let updates = calls
        .calls()?
        .into_iter()
        .filter(|argv| argv.first().is_some_and(|arg| arg == "update"))
        .collect::<Vec<_>>();
    assert!(
        updates
            .iter()
            .all(|argv| !argv.iter().any(|arg| arg == "--set-metadata")),
        "case {case:?} advanced a cursor: {updates:?}"
    );
    assert!(
        updates
            .iter()
            .all(|argv| !argv.iter().any(|arg| arg == "loom:active")),
        "case {case:?} changed the active bookmark: {updates:?}"
    );
    Ok(())
}

#[tokio::test]
async fn todo_success_marker_must_cover_exact_changed_spec_set() -> Result<()> {
    assert_todo_validation_case(ValidationCase::MissingSpec).await?;
    assert_todo_validation_case(ValidationCase::ExtraSpec).await
}

#[tokio::test]
async fn todo_success_validation_rejects_missing_extra_or_misparented_beads() -> Result<()> {
    assert_todo_validation_case(ValidationCase::MissingBead).await?;
    assert_todo_validation_case(ValidationCase::ExtraSpec).await?;
    assert_todo_validation_case(ValidationCase::MisparentedBead).await
}

#[tokio::test]
async fn todo_finalization_sets_active_and_advances_all_cursors() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (base, head) = init_workspace(dir.path())?;
    let mut responses = preflight_responses(&base, &head);
    responses.push(active_work_epic("lm-oldactive"));
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
        calls.iter().any(|argv| {
            argv.iter().any(|arg| arg == "--title")
                && argv.iter().any(|arg| arg == "Pin changed spec follow-ups")
        }),
        "finalization must apply the LOOM_TODO title: {calls:?}"
    );
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
    assert!(calls.iter().any(|argv| {
        argv.get(1).is_some_and(|arg| arg == "lm-oldactive")
            && argv.iter().any(|arg| arg == "--remove-label")
            && argv.iter().any(|arg| arg == "loom:active")
    }));
    Ok(())
}

async fn assert_atomic_finalization_failure(
    fail_update: Option<usize>,
    fail_cache: bool,
) -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (base, _head) = init_two_changed_workspace(dir.path())?;
    let runner = StatefulRunner::new(&base, fail_update);
    let state = Arc::new(CacheDb::open(dir.path().join(".loom/cache.db"))?);
    state.upsert_work_epic(&WorkEpicRow {
        epic_id: MoleculeId::new("lm-oldactive"),
        todo_head: Some(base.clone()),
        todo_fingerprint: None,
        is_active: true,
        iteration_count: 3,
    })?;
    let mut ctrl = ProductionTodoController::for_workspace(
        dir.path().to_path_buf(),
        Arc::clone(&state),
        manifest(dir.path())?,
        ProfileName::new("base"),
        Arc::new(GitClient::open(dir.path())?),
        Arc::new(BdClient::with_runner(runner.clone())),
    );
    let session = ctrl.build_session().await?;
    let success = todo_success(&session.config.initial_prompt, &["alpha", "beta"])?;
    if fail_cache {
        state.inject_todo_finalization_failure(1)?;
    }

    let error = match ctrl
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
        Ok(_) => return Err(anyhow!("injected finalization failure was ignored")),
        Err(error) => error,
    };
    assert!(
        matches!(error, TodoError::Bd(_) | TodoError::State(_)),
        "compensation should restore state and preserve the source error: {error:?}"
    );

    for id in ["lm-alpha", "lm-beta"] {
        let bead = runner.bead(id)?;
        assert_eq!(
            bead.metadata
                .get("loom.todo_cursor")
                .and_then(serde_json::Value::as_str),
            Some(base.as_str()),
            "{id} cursor changed after failure"
        );
    }
    let old_active = runner.bead("lm-oldactive")?;
    assert!(
        old_active.labels.iter().any(|label| label == "loom:active"),
        "previous active bookmark was not restored: {old_active:?}"
    );
    let pending = runner.bead("lm-work")?;
    assert!(
        pending.labels.iter().any(|label| label == "loom:todo"),
        "pending label was not restored: {pending:?}"
    );
    assert!(
        pending.labels.iter().all(|label| label != "loom:active"),
        "failed work epic remained active: {pending:?}"
    );
    assert_eq!(pending.title, "Pending todo decomposition");

    for label in [SpecLabel::new("alpha"), SpecLabel::new("beta")] {
        let row = state
            .spec_epic(&label)?
            .ok_or_else(|| anyhow!("cache row missing for {label}"))?;
        assert_eq!(row.todo_cursor.as_deref(), Some(base.as_str()));
    }
    let old_cached = state
        .work_epic(&MoleculeId::new("lm-oldactive"))?
        .ok_or_else(|| anyhow!("old active cache row missing"))?;
    let pending_cached = state
        .work_epic(&MoleculeId::new("lm-work"))?
        .ok_or_else(|| anyhow!("pending cache row missing"))?;
    assert!(old_cached.is_active);
    assert!(!pending_cached.is_active);
    Ok(())
}

#[tokio::test]
async fn todo_finalization_advances_cursors_and_active_epic_atomically() -> Result<()> {
    for fail_update in 1..=4 {
        assert_atomic_finalization_failure(Some(fail_update), false).await?;
    }
    assert_atomic_finalization_failure(None, true).await
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

#[tokio::test]
async fn todo_missing_spec_epic_initializes_existing_missing_cursor_blocks() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (base, head) = init_workspace(dir.path())?;
    let runner = CapturingRunner::new(preflight_responses(&base, &head));
    let calls = runner.clone();
    let mut ctrl = controller(dir.path(), runner)?;
    let session = ctrl.build_session().await?;
    assert!(session.config.initial_prompt.contains("### gamma"));
    assert!(
        calls
            .calls()?
            .iter()
            .any(|argv| argv.iter().any(|arg| arg == "loom:spec,spec:gamma"))
    );

    let blocked_dir = tempfile::tempdir()?;
    let (_blocked_base, _blocked_head) = init_workspace(blocked_dir.path())?;
    let responses = vec![spec_epic_without_cursor("lm-alpha", "alpha")];
    let mut ctrl = controller(blocked_dir.path(), CapturingRunner::new(responses))?;
    let err = match ctrl.build_session().await {
        Ok(_) => return Err(anyhow!("missing existing cursor was accepted")),
        Err(err) => err,
    };
    match err {
        TodoError::MissingSpecCursor { label, epic_id } => {
            assert_eq!(label, "alpha");
            assert_eq!(epic_id, "lm-alpha");
        }
        other => return Err(anyhow!("expected MissingSpecCursor, got {other:?}")),
    }
    Ok(())
}

#[tokio::test]
async fn todo_invalid_spec_cursor_blocks_loudly() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (_base, _head) = init_workspace(dir.path())?;
    let responses = vec![spec_epic("lm-alpha", "alpha", "not-a-sha")];
    let mut ctrl = controller(dir.path(), CapturingRunner::new(responses))?;

    let err = match ctrl.build_session().await {
        Ok(_) => return Err(anyhow!("invalid cursor was accepted")),
        Err(err) => err,
    };

    match err {
        TodoError::InvalidSpecCursor {
            label,
            epic_id,
            cursor,
            reason,
        } => {
            assert_eq!(label, "alpha");
            assert_eq!(epic_id, "lm-alpha");
            assert_eq!(cursor, "not-a-sha");
            assert!(reason.contains("full git SHA"));
        }
        other => return Err(anyhow!("expected InvalidSpecCursor, got {other:?}")),
    }
    Ok(())
}

#[tokio::test]
async fn todo_no_work_outcome_advances_cursor_with_reason() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (base, head) = init_workspace(dir.path())?;
    let runner = CapturingRunner::new(preflight_responses(&base, &head));
    let calls = runner.clone();
    let mut ctrl = controller(dir.path(), runner)?;
    let session = ctrl.build_session().await?;
    let success = todo_success(&session.config.initial_prompt, &["alpha", "gamma"])?;

    let record = ctrl
        .record_outcome(
            &SessionOutcome {
                exit_code: 0,
                cost_usd: None,
            },
            None,
            Some(&success),
        )
        .await?;

    assert!(
        record
            .spec_outcomes
            .iter()
            .any(|row| row.label == SpecLabel::new("alpha") && row.outcome == "no-work: audited")
    );
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
    Ok(())
}

#[tokio::test]
async fn todo_output_summarizes_every_changed_spec_outcome() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (base, head) = init_workspace(dir.path())?;
    let runner = CapturingRunner::new(preflight_responses(&base, &head));
    let mut ctrl = controller(dir.path(), runner)?;

    let summary = run_todo_workflow(&mut ctrl, |cfg| async move {
        let success = todo_success(&cfg.initial_prompt, &["alpha", "gamma"]).map_err(|err| {
            loom_driver::agent::ProtocolError::Io(std::io::Error::other(err.to_string()))
        })?;
        Ok((
            SessionOutcome {
                exit_code: 0,
                cost_usd: None,
            },
            None,
            Some(success),
        ))
    })
    .await?;

    let labels = summary
        .spec_outcomes
        .iter()
        .map(|row| row.label.to_string())
        .collect::<Vec<_>>();
    assert_eq!(labels, vec!["alpha", "gamma"]);
    assert!(
        summary
            .spec_outcomes
            .iter()
            .all(|row| row.outcome == "no-work: audited")
    );
    Ok(())
}

#[tokio::test]
async fn todo_clarify_marks_work_epic() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (base, head) = init_workspace(dir.path())?;
    let mut responses = preflight_responses(&base, &head);
    responses.push(work_epic_with_notes(
        "lm-work",
        "## Options — choose decomposition\n\n### Option 1 — proceed\nCost: churn.",
    ));
    responses.push(empty_json());
    let runner = CapturingRunner::new(responses);
    let calls = runner.clone();
    let mut ctrl = controller(dir.path(), runner)?;
    let _session = ctrl.build_session().await?;

    let record = ctrl
        .record_outcome(
            &SessionOutcome {
                exit_code: 0,
                cost_usd: None,
            },
            Some(&ExitSignal::Clarify {
                question: "which decomposition?".to_string(),
            }),
            None,
        )
        .await?;

    assert!(record.spec_outcomes.is_empty());
    let calls = calls.calls()?;
    assert!(
        calls
            .iter()
            .any(|argv| argv.first().is_some_and(|arg| arg == "show")
                && argv.get(1).is_some_and(|arg| arg == "lm-work"))
    );
    assert!(
        calls
            .iter()
            .any(|argv| argv.iter().any(|arg| arg == "loom:clarify"))
    );
    Ok(())
}

#[tokio::test]
async fn clarify_downgrade_emits_driver_events_and_bd_breadcrumb() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (base, head) = init_workspace(dir.path())?;
    let mut responses = preflight_responses(&base, &head);
    responses.push(work_epic_with_notes("lm-work", "clarify without options"));
    responses.push(empty_json());
    let runner = CapturingRunner::new(responses);
    let calls = runner.clone();
    let logs_root = dir.path().join(".loom/logs");
    let when = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut ctrl = controller(dir.path(), runner)?.with_phase_log(logs_root.clone(), when);
    let _session = ctrl.build_session().await?;

    ctrl.record_outcome(
        &SessionOutcome {
            exit_code: 0,
            cost_usd: None,
        },
        Some(&ExitSignal::Clarify {
            question: "which decomposition?".to_string(),
        }),
        None,
    )
    .await?;

    let log_path = phase_log_path(&logs_root, "todo", when);
    let events = std::fs::read_to_string(&log_path)?
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let kinds = events
        .iter()
        .map(|event| event["driver_kind"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec!["marker_routed", "clarify_downgraded", "bd_state_transition"]
    );
    let downgrade = &events[1];
    assert_eq!(downgrade["payload"]["cause"], "clarify-without-options");
    assert_eq!(downgrade["payload"]["event_sequence"], downgrade["seq"]);
    assert_eq!(
        downgrade["payload"]["gate_log_path"],
        log_path.to_string_lossy().as_ref(),
    );
    assert!(calls.calls()?.iter().any(|argv| {
        argv.first().is_some_and(|arg| arg == "update")
            && argv.iter().any(|arg| arg == "loom:blocked")
            && argv
                .iter()
                .any(|arg| arg.contains("clarify-without-options"))
    }));
    Ok(())
}

#[tokio::test]
async fn todo_consumes_notes_only_after_validated_finalization() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (base, head) = init_workspace(dir.path())?;
    let state = Arc::new(CacheDb::open(dir.path().join(".loom/cache.db"))?);
    state.notes_add(
        &SpecLabel::new("alpha"),
        "implementation",
        "carry this hint",
        1,
    )?;
    let mut responses = preflight_responses(&base, &head);
    responses.push(child_bead("lm-child", "lm-work", None));
    responses.push(child_bead("lm-child", "lm-work", Some("existing note")));
    responses.push(empty_json());
    let runner = CapturingRunner::new(responses);
    let calls = runner.clone();
    let mut ctrl = ProductionTodoController::for_workspace(
        dir.path().to_path_buf(),
        Arc::clone(&state),
        manifest(dir.path())?,
        ProfileName::new("base"),
        Arc::new(GitClient::open(dir.path())?),
        Arc::new(BdClient::with_runner(runner)),
    );
    let session = ctrl.build_session().await?;
    let success = todo_success_with_specs(
        &session.config.initial_prompt,
        r#"{"label":"alpha","outcome":"decomposed","beads":["lm-child"]},{"label":"gamma","outcome":"no-work","reason":"audited"}"#,
    )?;

    ctrl.record_outcome(
        &SessionOutcome {
            exit_code: 0,
            cost_usd: None,
        },
        None,
        Some(&success),
    )
    .await?;

    assert!(
        state
            .notes_list(Some(&SpecLabel::new("alpha")), Some("implementation"))?
            .is_empty()
    );
    let calls = calls.calls()?;
    assert!(calls.iter().any(|argv| {
        argv.iter()
            .any(|arg| arg.contains("existing note\n\nImplementation notes:\n\n- carry this hint"))
    }));

    let invalid_dir = tempfile::tempdir()?;
    let (invalid_base, invalid_head) = init_workspace(invalid_dir.path())?;
    let invalid_state = Arc::new(CacheDb::open(invalid_dir.path().join(".loom/cache.db"))?);
    invalid_state.notes_add(&SpecLabel::new("alpha"), "implementation", "preserve", 1)?;
    let invalid_runner = CapturingRunner::new(preflight_responses(&invalid_base, &invalid_head));
    let mut invalid_ctrl = ProductionTodoController::for_workspace(
        invalid_dir.path().to_path_buf(),
        Arc::clone(&invalid_state),
        manifest(invalid_dir.path())?,
        ProfileName::new("base"),
        Arc::new(GitClient::open(invalid_dir.path())?),
        Arc::new(BdClient::with_runner(invalid_runner)),
    );
    let invalid_session = invalid_ctrl.build_session().await?;
    let invalid_success = todo_success(&invalid_session.config.initial_prompt, &["alpha"])?;
    let result = invalid_ctrl
        .record_outcome(
            &SessionOutcome {
                exit_code: 0,
                cost_usd: None,
            },
            None,
            Some(&invalid_success),
        )
        .await;
    assert!(matches!(result, Err(TodoError::TodoValidation { .. })));
    assert_eq!(
        invalid_state.notes_list(Some(&SpecLabel::new("alpha")), Some("implementation"))?[0].text,
        "preserve",
    );
    Ok(())
}

//! Shared preparation for sequential and parallel loop workers.

use std::path::Path;

use loom_driver::agent::{AgentRuntime, SpawnConfig};
use loom_driver::bd::{BdClient, Bead, CommandRunner};
use loom_driver::config::{LoomTopConfig, Phase, SkillsConfig};
use loom_driver::git::{BeadCloneAlignment, GitClient, GitError};
use loom_driver::identifier::{ProfileName, SpecLabel};
use loom_driver::profile_manifest::{NativePreflight, ProfileError, ProfileImageManifest};
use loom_driver::scratch::{ScratchSession, resolve_scratch_key};
use loom_events::AgentStartMetadata;
use loom_protocol::oid::GitOid;
use loom_templates::run::{PreviousFailure, RecoveryStash, WorkspaceAlignment, WorkspaceRecovery};

use super::context::{LoopContextInputs, render_loop_prompt};
use super::profile::resolve_profile;
use super::spawn::{build_spawn_config_from_manifest, dolt_socket_mount, sccache_mount};
use super::{
    AgentOutcome, INVALID_SPAWN_CONFIG_CAUSE, LoopError, format_unknown_profile_error,
    format_unknown_runtime_for_profile_error,
};
use crate::skill::SkillPlan;
use crate::spawn::container_workspace_path;

pub(super) fn verification_failure(
    record: &loom_driver::workspace::VerificationContinuation,
) -> Option<PreviousFailure> {
    record
        .repair_failure
        .as_ref()
        .map(|failure| PreviousFailure::PostIntegrateFail {
            failures: vec![loom_templates::previous_failure::VerifierFailure::new(
                failure.target.clone(),
                failure.exit_code,
                failure.output.clone(),
            )],
            gate_log_path: failure.log_path.clone(),
        })
        .or_else(|| {
            record
                .repair_detail
                .clone()
                .map(PreviousFailure::from_agent_error)
        })
}

/// Inputs common to every worker, independent of the scheduling mode.
pub struct Request<'a> {
    pub bead: &'a Bead,
    pub workspace: &'a Path,
    pub loom_workspace: &'a Path,
    pub manifest: &'a ProfileImageManifest,
    pub cli_profile: Option<&'a ProfileName>,
    pub phase_default: &'a ProfileName,
    pub runtime: AgentRuntime,
    pub label: &'a SpecLabel,
    pub style_rules: &'a str,
    pub loom: &'a LoomTopConfig,
    pub skills: &'a SkillsConfig,
    pub launcher_env: Vec<(String, String)>,
    pub previous_failure: Option<PreviousFailure>,
    pub bead_base: GitOid,
    pub workspace_recovery: Option<WorkspaceRecovery>,
    pub attempt: u32,
}

/// Scratch ownership lasts until the dispatched worker exits or is cancelled.
pub struct Worker {
    pub spawn: SpawnConfig,
    pub scratch: ScratchSession,
    pub acceptance: super::acceptance::Resolved,
}

pub enum Preparation {
    Ready(Box<Worker>),
    Rejected(AgentOutcome),
}

/// Resolved dispatch base and any preserved dirty-workspace context.
#[derive(Debug, Clone)]
pub struct WorkspacePreparation {
    pub bead_base: GitOid,
    pub recovery: Option<WorkspaceRecovery>,
}

/// Preserve dirty work and resolve the base and recovery context before dispatch.
///
/// # Errors
/// Returns Git preparation failures without discarding the existing workspace.
pub async fn prepare_workspace(
    git: &GitClient,
    bead: &Bead,
    workspace: &Path,
) -> Result<WorkspacePreparation, GitError> {
    let preparation = git.prepare_bead_clone(workspace, &bead.id).await?;
    let bead_base = preparation.integration_tip;
    let alignment = match preparation.alignment {
        BeadCloneAlignment::Clean => WorkspaceAlignment::Clean,
        BeadCloneAlignment::Rebased {
            previous_head,
            current_head,
        } => WorkspaceAlignment::Rebased {
            previous_head,
            current_head,
        },
        BeadCloneAlignment::Conflict { files } => WorkspaceAlignment::Conflict {
            files: files
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
        },
    };
    let recovery = preparation.recovery.map(|recovery| WorkspaceRecovery {
        pre_stash_status: recovery.pre_stash_status,
        stash: RecoveryStash {
            selector: recovery.stash.selector,
            commit: recovery.stash.commit,
            message: recovery.stash.message,
        },
        integration_tip: bead_base.clone(),
        alignment,
    });
    Ok(WorkspacePreparation {
        bead_base,
        recovery,
    })
}

/// Resolve skills, prompt, profile image, mounts, and event metadata once per dispatch.
///
/// # Errors
/// Returns preparation failures; static profile faults are classified rejections.
pub async fn prepare(request: Request<'_>) -> Result<Preparation, LoopError> {
    prepare_with_bd(&BdClient::new(), request).await
}

/// Prepare workers using the owning driver's Beads transport and current finding context.
///
/// # Errors
/// Infrastructure setup errors propagate; invalid declared acceptance is a pre-spawn rejection.
pub async fn prepare_with_bd<R: CommandRunner>(
    bd: &BdClient<R>,
    request: Request<'_>,
) -> Result<Preparation, LoopError> {
    let acceptance = match super::acceptance::resolve(bd, request.bead, request.workspace).await {
        Ok(acceptance) => acceptance,
        Err(error) => {
            return Ok(Preparation::Rejected(AgentOutcome::Blocked {
                reason: format!(
                    "task acceptance rejected before spawn: {}",
                    crate::mint::mint_error_message(&error)
                ),
            }));
        }
    };
    let Request {
        bead,
        workspace,
        loom_workspace,
        manifest,
        cli_profile,
        phase_default,
        runtime,
        label,
        style_rules,
        loom,
        skills,
        launcher_env,
        previous_failure,
        bead_base,
        workspace_recovery,
        attempt,
    } = request;
    let labels = match &acceptance {
        super::acceptance::Resolved::Implementation(resolved) => resolved
            .obligations()
            .iter()
            .map(|obligation| obligation.reference().spec_label().clone())
            .collect::<std::collections::HashSet<_>>(),
        super::acceptance::Resolved::Remediation(resolved) => resolved
            .goals()
            .iter()
            .flat_map(|goal| goal.finding().bonds().iter().cloned())
            .collect(),
    }
    .into_iter()
    .collect::<Vec<_>>();
    let package_context = loom_driver::spec::context::load(
        workspace,
        loom_protocol::context::Purpose::Implementation,
        loom_driver::spec::context::Selection::Labels(&labels),
    )?;
    let spec_path = package_context
        .contract_path(label)
        .map_or_else(|| format!("specs/{label}.md"), str::to_owned);
    let association = loom_driver::workspace::Association::resolve(loom_workspace)?;
    let key = resolve_scratch_key(Phase::Loop, std::slice::from_ref(label), Some(&bead.id));
    let scratchpad_path = ScratchSession::scratchpad_path_for(workspace, &key);
    let scratch_dir = scratchpad_path.parent().ok_or_else(|| LoopError::Bug {
        context: "scratchpad path has no parent".into(),
    })?;
    let tracked_files = GitClient::open(workspace)?.tracked_files().await?;
    let profile = resolve_profile(&bead.labels, cli_profile, phase_default);
    let skill_plan = SkillPlan::resolve(
        workspace,
        &tracked_files,
        Phase::Loop.as_str(),
        &profile,
        runtime,
        skills,
    )?;
    let skill_session = skill_plan.materialize(scratch_dir, workspace)?;
    let package_context = package_context.with_snapshot_root(
        container_workspace_path(workspace, &scratch_dir.join("context"))
            .to_string_lossy()
            .into_owned(),
    );
    let initial_prompt = render_loop_prompt(LoopContextInputs {
        package_context: Some(package_context.clone()),
        acceptance: acceptance.clone(),
        acceptance_workspace: workspace.to_path_buf(),
        label: label.clone(),
        spec_path,
        pinned_context: String::new(),
        companion_paths: vec![],
        molecule_id: None,
        issue_id: bead.id.clone(),
        bead_base,
        title: bead.title.clone(),
        description: bead.description.clone(),
        previous_failure,
        workspace_recovery,
        attempt,
        scratchpad_path: container_workspace_path(workspace, &scratchpad_path)
            .to_string_lossy()
            .into_owned(),
        style_rules: style_rules.to_owned(),
        skill_index: skill_session.skill_index,
    })?;
    let scratch = ScratchSession::open(
        workspace,
        &key,
        &initial_prompt,
        &format!("loom loop @ {}", bead.id),
    )?;
    loom_driver::spec::context::write(&package_context, scratch.path())?;
    let mut mounts: Vec<_> = dolt_socket_mount(loom_workspace).into_iter().collect();
    if let Some(cache) = &loom.sccache_dir {
        association.admit_mount(&loom_driver::agent::MountSpec {
            host_path: cache.clone(),
            container_path: loom.sccache_container_path.clone(),
            read_only: false,
        })?;
    }
    mounts.extend(sccache_mount(loom)?);
    let mut spawn = match build_spawn_config_from_manifest(
        manifest,
        bead,
        cli_profile,
        phase_default,
        runtime,
        workspace.to_path_buf(),
        initial_prompt,
        scratch.path().to_path_buf(),
        loom.container_sccache_env(),
        vec![],
        mounts,
        launcher_env,
    ) {
        Ok(spawn) => spawn,
        Err(error) => return profile_rejection(error, manifest).map(Preparation::Rejected),
    };
    let native_feedback =
        manifest.lookup(&profile, runtime)?.native_preflight == Some(NativePreflight::Rust);
    if native_feedback
        || tracked_files
            .iter()
            .any(|path| path == Path::new("Cargo.toml"))
    {
        if !native_feedback {
            return Ok(Preparation::Rejected(AgentOutcome::StaticInfra {
                cause: super::runner::VERIFICATION_CAPABILITY_UNAVAILABLE_CAUSE.to_owned(),
                error: format!(
                    "profile {profile} runtime {runtime} has no provisioned native-Rust preflight; required source feedback remains unverified"
                ),
            }));
        }
        spawn
            .env
            .push(("LOOM_NATIVE_PREFLIGHT".to_owned(), "rust".to_owned()));
        spawn.handshake_timeout = Some(
            spawn
                .handshake_timeout
                .unwrap_or_default()
                .max(std::time::Duration::from_secs(370)),
        );
    }
    association.validate_worker(workspace, &spawn.mounts, spawn.profile_config.as_deref())?;
    let skill_session = skill_plan.materialize(scratch.path(), workspace)?;
    spawn.skills = Some(skill_session.registered);
    spawn.event_metadata = Some(AgentStartMetadata {
        title: bead.title.clone(),
        profile,
        spec_label: label.clone(),
        parent_tool_call_id: None,
    });
    Ok(Preparation::Ready(Box::new(Worker {
        spawn,
        scratch,
        acceptance,
    })))
}

/// Recovery metadata shared by sequential and parallel event streams.
pub fn recovery_event(
    bead_id: &loom_driver::identifier::BeadId,
    recovery: &WorkspaceRecovery,
) -> loom_events::DriverEventPayload {
    let (alignment_outcome, previous_head, current_head) = match &recovery.alignment {
        WorkspaceAlignment::Clean => ("clean", None, None),
        WorkspaceAlignment::Rebased {
            previous_head,
            current_head,
        } => (
            "rebased",
            Some(previous_head.as_str()),
            Some(current_head.as_str()),
        ),
        WorkspaceAlignment::Conflict { .. } => ("conflict", None, None),
    };
    loom_events::DriverEventPayload {
        driver_kind: loom_events::DriverKind::WorkspaceRecovery,
        summary: format!("workspace recovery stash preserved for bead {bead_id}"),
        payload: serde_json::json!({
            "bead_id": bead_id.to_string(),
            "pre_stash_status": recovery.pre_stash_status.as_str(),
            "stash_selector": recovery.stash.selector.as_str(),
            "stash_message": recovery.stash.message.as_str(),
            "stash_commit": recovery.stash.commit.as_str(),
            "integration_tip": recovery.integration_tip.as_str(),
            "alignment_outcome": alignment_outcome,
            "alignment_previous_head": previous_head,
            "alignment_current_head": current_head,
            "conflict_files": recovery.alignment.conflict_files(),
        }),
    }
}

fn profile_rejection(
    error: ProfileError,
    manifest: &ProfileImageManifest,
) -> Result<AgentOutcome, LoopError> {
    Ok(match error {
        ProfileError::UnknownProfile { name, .. } => AgentOutcome::UnknownProfile {
            error: format_unknown_profile_error(&name, manifest),
        },
        ProfileError::UnknownRuntimeForProfile {
            profile,
            runtime,
            declared_runtimes,
            ..
        } => AgentOutcome::UnknownRuntimeForProfile {
            error: format_unknown_runtime_for_profile_error(&profile, runtime, &declared_runtimes),
        },
        error @ (ProfileError::InvalidSpawnConfig { .. }
        | ProfileError::RuntimeMetadataMismatch { .. }) => AgentOutcome::StaticInfra {
            cause: INVALID_SPAWN_CONFIG_CAUSE.to_string(),
            error: error.to_string(),
        },
        other => return Err(LoopError::Profile(other)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::agent::AgentBackend;

    async fn run_prepared_backend<B: AgentBackend>(
        spawn: &SpawnConfig,
    ) -> super::super::SessionResult {
        crate::agent::run_agent_classified::<B>(spawn, None, None, None, None).await
    }

    #[tokio::test]
    async fn native_worker_preparation_probes_before_agent_and_routes_missing_capability() {
        use std::os::unix::fs::PermissionsExt as _;
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let git = loom_driver::git::init_test_repo_with_integration(dir.path()).unwrap();
        let bead_id = loom_driver::identifier::BeadId::new("lm-nworker").unwrap();
        let label = SpecLabel::new("harness").unwrap();
        let workspace = git.create_worktree(&label, &bead_id).await.unwrap().path;
        std::fs::write(
            workspace.join("Cargo.toml"),
            "broken Cargo project awaiting source repair\n",
        )
        .unwrap();
        let bead_git = GitClient::open(&workspace).unwrap();
        bead_git
            .commit_all_allow_empty("Prepare tracked Rust repair fixture")
            .await
            .unwrap();
        let bead_base = bead_git.head_commit_sha().await.unwrap();
        let profile_config = dir.path().join("profile.json");
        std::fs::write(&profile_config, r#"{"profile":{"mounts":[]}}"#).unwrap();
        let preflight = dir.path().join("preflight.sh");
        std::fs::write(
            &preflight,
            include_str!("../../../../scripts/worker-native-preflight.sh"),
        )
        .unwrap();
        let empty_path = dir.path().join("empty-path");
        std::fs::create_dir(&empty_path).unwrap();
        let bash = Command::new("bash")
            .args(["-c", "printf '%s' \"$BASH\""])
            .output()
            .unwrap();
        assert!(bash.status.success());
        let bash = String::from_utf8(bash.stdout).unwrap();
        let launcher = dir.path().join("wrix");
        std::fs::write(&launcher, format!("#!{bash}\n{}", r#"set -euo pipefail
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --spawn-config) config="$2"; shift 2 ;;
    *) shift ;;
  esac
done
if ! jq -e '.env | any(. == ["LOOM_NATIVE_PREFLIGHT", "rust"])' "$config" >/dev/null; then
  jq '.env' "$config" >&2
  exit 1
fi
printf '%s\n' '[wrix] Starting container' >&2
IFS= read -r _
if [[ "$LOOM_PREFLIGHT_STALL" == 1 ]]; then
  exec timeout --kill-after=0.1s 0.1s sleep 30
fi
export PATH="$LOOM_PREFLIGHT_EMPTY_PATH"
exec "$LOOM_PREFLIGHT_BASH" -euo pipefail -c 'source "$1"; require_tool nix' -- "$LOOM_PREFLIGHT_SCRIPT"
"#)).unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700)).unwrap();
        crate::test_support::acceptance::write_contract(
            &workspace,
            &label,
            "Worker reports unavailable native capabilities before starting the agent.",
        );
        let mut bead: Bead = serde_json::from_value(serde_json::json!({
            "id": "lm-nworker", "title": "native worker", "status": "open"
        }))
        .unwrap();
        crate::test_support::acceptance::bind(&workspace, &label, &mut bead);
        for (runtime, capability, stall, native_profile) in
            [AgentRuntime::Pi, AgentRuntime::Claude, AgentRuntime::Direct]
                .into_iter()
                .flat_map(|runtime| {
                    [
                        (runtime, None, false, true),
                        (runtime, Some(NativePreflight::Rust), false, true),
                        (runtime, Some(NativePreflight::Rust), true, true),
                        (runtime, None, false, false),
                    ]
                })
        {
            let profile = ProfileName::new(if native_profile { "rust" } else { "base" }).unwrap();
            let manifest_path = dir.path().join("images.json");
            std::fs::write(&manifest_path, serde_json::to_vec(&serde_json::json!({
                (profile.to_string()): {(runtime.to_string()): {"ref":"fixture", "source":"/fixture/image", "source_kind":"nix-descriptor",
                    "launcher":launcher, "profile_config":profile_config, "native_preflight":capability}}
            })).unwrap()).unwrap();
            let manifest = ProfileImageManifest::from_path(&manifest_path).unwrap();
            let prepared = prepare(Request {
                bead: &bead,
                workspace: &workspace,
                loom_workspace: dir.path(),
                manifest: &manifest,
                cli_profile: (!native_profile).then_some(&profile),
                phase_default: &profile,
                runtime,
                label: &label,
                style_rules: "docs/style-rules.md",
                loom: &LoomTopConfig::default(),
                skills: &SkillsConfig::default(),
                launcher_env: vec![
                    (
                        "LOOM_PREFLIGHT_STALL".into(),
                        if stall { "1" } else { "0" }.into(),
                    ),
                    (
                        "LOOM_PREFLIGHT_SCRIPT".into(),
                        preflight.display().to_string(),
                    ),
                    (
                        "LOOM_PREFLIGHT_EMPTY_PATH".into(),
                        empty_path.display().to_string(),
                    ),
                    ("LOOM_PREFLIGHT_BASH".into(), bash.clone()),
                ],
                previous_failure: None,
                bead_base: bead_base.clone(),
                workspace_recovery: None,
                attempt: 1,
            })
            .await
            .unwrap();
            if capability.is_none() {
                assert!(
                    matches!(prepared, Preparation::Rejected(AgentOutcome::StaticInfra { cause, .. })
                    if cause == super::super::VERIFICATION_CAPABILITY_UNAVAILABLE_CAUSE)
                );
                continue;
            }
            let Preparation::Ready(worker) = prepared else {
                panic!("provisioned worker rejected")
            };
            let result = match runtime {
                AgentRuntime::Pi => {
                    run_prepared_backend::<loom_agent::PiBackend>(&worker.spawn).await
                }
                AgentRuntime::Claude => {
                    run_prepared_backend::<loom_agent::ClaudeBackend>(&worker.spawn).await
                }
                AgentRuntime::Direct => {
                    run_prepared_backend::<loom_agent::DirectBackend>(&worker.spawn).await
                }
            };
            if stall {
                assert!(
                    matches!(&result, super::super::SessionResult::PreflightFailed { error }
                    if error.contains("124")),
                    "{result:?}"
                );
                continue;
            }
            assert!(
                matches!(&result, super::super::SessionResult::StaticInfra { cause, error }
                if cause == super::super::VERIFICATION_CAPABILITY_UNAVAILABLE_CAUSE && error.contains("78")),
                "{result:?}"
            );
        }
    }

    #[tokio::test]
    async fn shared_worker_preparation_threads_context_and_owns_scratch_for_every_backend() {
        let dir = tempfile::tempdir().unwrap();
        let git = loom_driver::git::init_test_repo_with_integration(dir.path()).unwrap();
        let bead_id = loom_driver::identifier::BeadId::new("lm-worker").unwrap();
        let label = SpecLabel::new("harness").unwrap();
        let workspace = git.create_worktree(&label, &bead_id).await.unwrap().path;
        let profile_config = dir.path().join("profile.json");
        std::fs::write(&profile_config, r#"{"profile":{"mounts":[]}}"#).unwrap();
        let mut entries = serde_json::Map::new();
        for runtime in [AgentRuntime::Pi, AgentRuntime::Claude, AgentRuntime::Direct] {
            entries.insert(runtime.to_string(), serde_json::json!({
                "ref": format!("image-{runtime}"), "source": "/fixture/image", "source_kind": "nix-descriptor",
                "launcher": "/fixture/wrix", "profile_config": profile_config
            }));
        }
        let manifest_path = dir.path().join("images.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_vec(&serde_json::json!({"base": entries})).unwrap(),
        )
        .unwrap();
        let manifest = ProfileImageManifest::from_path(&manifest_path).unwrap();
        crate::test_support::acceptance::write_contract(
            &workspace,
            &label,
            "Worker preserves resolved instructions, retry context and scratch ownership through compaction.",
        );
        let mut bead: Bead = serde_json::from_value(serde_json::json!({
            "id": "lm-worker", "title": "worker title", "description": "worker body", "status": "open"
        })).unwrap();
        crate::test_support::acceptance::bind(&workspace, &label, &mut bead);
        let profile = ProfileName::base();
        for runtime in [AgentRuntime::Pi, AgentRuntime::Claude, AgentRuntime::Direct] {
            let prepared = prepare(Request {
                bead: &bead,
                workspace: &workspace,
                loom_workspace: dir.path(),
                manifest: &manifest,
                cli_profile: None,
                phase_default: &profile,
                runtime,
                label: &label,
                style_rules: "docs/style-rules.md",
                loom: &LoomTopConfig::default(),
                skills: &SkillsConfig::default(),
                launcher_env: vec![],
                previous_failure: Some(PreviousFailure::AgentRetry {
                    reason: "retry-context".into(),
                }),
                bead_base: GitOid::new("0123456789abcdef0123456789abcdef01234567").unwrap(),
                workspace_recovery: None,
                attempt: 2,
            })
            .await
            .unwrap();
            let Preparation::Ready(worker) = prepared else {
                panic!("valid worker rejected");
            };
            assert_eq!(worker.spawn.image_ref, format!("image-{runtime}"));
            assert_eq!(
                worker.spawn.event_metadata.as_ref().unwrap().title,
                bead.title
            );
            assert!(worker.spawn.skills.is_some());
            assert!(worker.spawn.initial_prompt.contains("retry-context"));
            assert!(
                worker
                    .spawn
                    .initial_prompt
                    .contains("/workspace/.loom/scratch/")
            );
            assert!(
                !worker
                    .spawn
                    .initial_prompt
                    .contains(&dir.path().display().to_string())
            );
            let scratch = worker.scratch.path().to_path_buf();
            let prompt = &worker.spawn.initial_prompt;
            assert!(prompt.contains(
                "loom gate verify --diff 0123456789abcdef0123456789abcdef01234567..HEAD"
            ));
            assert_eq!(
                std::fs::read_to_string(scratch.join("prompt.txt")).unwrap(),
                *prompt
            );
            std::fs::write(scratch.join("scratch.md"), "live recovery notes").unwrap();
            let pi_pin = loom_agent::pi::backend::PiBackend::compaction_repin(&worker.spawn)
                .unwrap()
                .unwrap();
            assert!(pi_pin.contains(prompt));
            assert!(pi_pin.contains("live recovery notes"));
            let output = std::process::Command::new("bash")
                .arg(worker.scratch.repin_script())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let hook: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            let claude_pin = hook["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap();
            assert!(claude_pin.contains(prompt));
            assert!(claude_pin.contains("live recovery notes"));
            drop(worker);
            assert!(
                !scratch.exists(),
                "scratch ownership includes cancellation cleanup"
            );
        }
    }
}

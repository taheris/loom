//! Shared preparation for sequential and parallel loop workers.
use std::path::Path;

use loom_driver::agent::{AgentRuntime, SpawnConfig};
use loom_driver::bd::Bead;
use loom_driver::config::{LoomTopConfig, Phase, SkillsConfig};
use loom_driver::git::{BeadCloneAlignment, GitClient, GitError};
use loom_driver::identifier::{ProfileName, SpecLabel};
use loom_driver::profile_manifest::{ProfileError, ProfileImageManifest};
use loom_driver::scratch::{ScratchSession, resolve_scratch_key};
use loom_events::AgentStartMetadata;
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
    pub workspace_recovery: Option<WorkspaceRecovery>,
    pub attempt: u32,
}

/// Scratch ownership lasts until the dispatched worker exits or is cancelled.
pub struct Worker {
    pub spawn: SpawnConfig,
    pub scratch: ScratchSession,
}

pub enum Preparation {
    Ready(Box<Worker>),
    Rejected(AgentOutcome),
}

/// Preserve dirty work and derive the recovery context before dispatch.
///
/// # Errors
/// Returns Git preparation failures without discarding the existing workspace.
pub async fn prepare_workspace(
    git: &GitClient,
    bead: &Bead,
    workspace: &Path,
) -> Result<Option<WorkspaceRecovery>, GitError> {
    let preparation = git.prepare_bead_clone(workspace, &bead.id).await?;
    let Some(recovery) = preparation.recovery else {
        return Ok(None);
    };
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
    Ok(Some(WorkspaceRecovery {
        pre_stash_status: recovery.pre_stash_status,
        stash: RecoveryStash {
            selector: recovery.stash.selector,
            commit: recovery.stash.commit,
            message: recovery.stash.message,
        },
        integration_tip: preparation.integration_tip,
        alignment,
    }))
}

/// Resolve skills, prompt, profile image, mounts, and event metadata once per dispatch.
///
/// # Errors
/// Returns preparation failures; static profile faults are classified rejections.
pub async fn prepare(request: Request<'_>) -> Result<Preparation, LoopError> {
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
        workspace_recovery,
        attempt,
    } = request;
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
    let initial_prompt = render_loop_prompt(LoopContextInputs {
        label: label.clone(),
        spec_path: format!("specs/{label}.md"),
        pinned_context: String::new(),
        companion_paths: vec![],
        molecule_id: None,
        issue_id: bead.id.clone(),
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
    let mut mounts: Vec<_> = dolt_socket_mount(loom_workspace).into_iter().collect();
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
    let skill_session = skill_plan.materialize(scratch.path(), workspace)?;
    spawn.skills = Some(skill_session.registered);
    spawn.event_metadata = Some(AgentStartMetadata {
        title: bead.title.clone(),
        profile,
        spec_label: label.clone(),
        parent_tool_call_id: None,
    });
    Ok(Preparation::Ready(Box::new(Worker { spawn, scratch })))
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

    #[tokio::test]
    async fn shared_worker_preparation_threads_context_and_owns_scratch_for_every_backend() {
        let dir = tempfile::tempdir().unwrap();
        loom_driver::git::init_test_repo_with_integration(dir.path()).unwrap();
        let mut entries = serde_json::Map::new();
        for runtime in [AgentRuntime::Pi, AgentRuntime::Claude, AgentRuntime::Direct] {
            entries.insert(runtime.to_string(), serde_json::json!({
                "ref": format!("image-{runtime}"), "source": "/fixture/image", "source_kind": "nix-descriptor"
            }));
        }
        let manifest_path = dir.path().join("images.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_vec(&serde_json::json!({"base": entries})).unwrap(),
        )
        .unwrap();
        let manifest = ProfileImageManifest::from_path(&manifest_path).unwrap();
        let bead: Bead = serde_json::from_value(serde_json::json!({
            "id": "lm-worker", "title": "worker title", "description": "worker body", "status": "open"
        })).unwrap();
        let profile = ProfileName::base();
        let label = SpecLabel::new("harness").unwrap();
        for runtime in [AgentRuntime::Pi, AgentRuntime::Claude, AgentRuntime::Direct] {
            let prepared = prepare(Request {
                bead: &bead,
                workspace: dir.path(),
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
            assert!(scratch.join("prompt.txt").is_file());
            assert!(scratch.join("scratch.md").is_file());
            drop(worker);
            assert!(
                !scratch.exists(),
                "scratch ownership includes cancellation cleanup"
            );
        }
    }
}

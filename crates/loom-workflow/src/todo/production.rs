//! Production [`TodoController`] used by the `loom todo` binary.
//!
//! Resolves the per-bead [`SpawnConfig`] by running the single-query
//! resolver against a real [`BdClient`] + working-tree-diff touched-set
//! discovery against a real [`GitClient`], then renders `todo_new.md` /
//! `todo_update.md` from `templates`.
//!
//! Agent dispatch happens in [`super::runner::run`] via a caller-provided
//! closure, so this controller does not own the spawn surface.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use askama::Template;
use loom_driver::agent::{RePinContent, SessionOutcome, SpawnConfig, set_loom_inside};
use loom_driver::bd::{
    BdClient, BdError, CommandRunner, CreateOpts, ListOpts, TokioRunner, UpdateOpts,
};
use loom_driver::config::Phase;
use loom_driver::git::GitClient;
use loom_driver::identifier::{BeadId, MoleculeId, ProfileName, SpecLabel};
use loom_driver::profile_manifest::ProfileImageManifest;
use loom_driver::scratch::resolve_scratch_key;
use loom_driver::state::{BdUpdateFn, StateDb};
use tracing::{debug, info, warn};

use super::ExitSignal;
use super::context::{TemplateBaseFields, TodoTemplateContext, build_template_context};
use super::criterion_status::build_criterion_status;
use super::error::TodoError;
use super::fanout::{FanoutOutcome, classify_touched_set, render_collision_options};
use super::resolve::{ResolverOutcome, resolve_molecule};
use super::runner::{TodoController, TodoSession};
use super::touched::touched_specs;

const BASE_COMMIT_METADATA_KEY: &str = "loom.base_commit";

pub struct ProductionTodoController<R: CommandRunner = TokioRunner> {
    label: SpecLabel,
    workspace: PathBuf,
    state: Arc<StateDb>,
    manifest: Arc<ProfileImageManifest>,
    phase_default: ProfileName,
    git: Arc<GitClient>,
    bd: Arc<BdClient<R>>,
    #[expect(
        dead_code,
        reason = "CLI flag retained pending broader --since/--spec pruning; the tier-1 base override it fed has been removed"
    )]
    since: Option<String>,
    /// Pre-session task-bead snapshot for the productive-completion
    /// fan-out guard. `None` means [`Self::build_session`] has not run
    /// yet; the guard is skipped in that branch.
    pre_snapshot: Option<HashSet<BeadId>>,
}

impl<R: CommandRunner> ProductionTodoController<R> {
    #[expect(clippy::too_many_arguments, reason = "controller construction surface")]
    pub fn new(
        label: SpecLabel,
        workspace: PathBuf,
        state: Arc<StateDb>,
        manifest: Arc<ProfileImageManifest>,
        phase_default: ProfileName,
        git: Arc<GitClient>,
        bd: Arc<BdClient<R>>,
        since: Option<String>,
    ) -> Self {
        Self {
            label,
            workspace,
            state,
            manifest,
            phase_default,
            git,
            bd,
            since,
            pre_snapshot: None,
        }
    }

    /// Snapshot every task bead carrying `spec:<label>` across the three
    /// statuses an in-flight session can move beads through (open,
    /// in_progress, closed). Used by the productive-completion guard to
    /// detect zero fan-out under non-empty implementation notes — the
    /// failure shape the driver must refuse to consume per
    /// `specs/harness.md` *Productive-completion gate*.
    async fn snapshot_task_beads(&self) -> Result<HashSet<BeadId>, TodoError> {
        let mut snapshot = HashSet::new();
        for status in ["open", "in_progress", "closed"] {
            let beads = self
                .bd
                .list(ListOpts {
                    issue_type: Some("task".to_string()),
                    label: Some(format!("spec:{}", self.label.as_str())),
                    status: Some(status.to_string()),
                    ..Default::default()
                })
                .await?;
            for bead in beads {
                snapshot.insert(bead.id);
            }
        }
        Ok(snapshot)
    }

    async fn build_prompt(&self) -> Result<String, TodoError> {
        let spec_path = PathBuf::from("specs").join(format!("{}.md", self.label.as_str()));
        let touched = touched_specs(self.git.as_ref()).await?;

        // Multi-spec collision check runs before any prompt rendering. The
        // touched-set classifier walks every spec whose markdown differs
        // from HEAD; if it spans multiple molecules — or mixes
        // has-open-epic with no-open-epic — Loom mints nothing and emits
        // a `loom:clarify` bead per gate.md's *Options Format Contract*.
        // The classifier and clarify mint live in [`super::fanout`].
        //
        // Empty touched-set is a degenerate case (working tree matches
        // `HEAD`): the classifier returns `MintAll` vacuously, but we
        // still need the anchor's existing molecule for template
        // selection. Fall back to single-spec resolution against the
        // anchor when nothing is touched.
        let molecule_id = if touched.is_empty() {
            match resolve_molecule(&self.bd, &self.label).await? {
                ResolverOutcome::Existing(id) => Some(id),
                ResolverOutcome::None => None,
                ResolverOutcome::InvariantViolation(ids) => {
                    let joined = ids
                        .iter()
                        .map(MoleculeId::as_str)
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(TodoError::InvariantViolation {
                        label: self.label.to_string(),
                        ids: joined,
                    });
                }
            }
        } else {
            match classify_touched_set(&self.bd, &touched).await? {
                FanoutOutcome::MintAll => None,
                FanoutOutcome::Bond(id) => Some(id),
                FanoutOutcome::Collision { resolutions } => {
                    let body = render_collision_options(&resolutions);
                    let labels = resolutions
                        .iter()
                        .map(|r| r.label.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let title = format!("loom todo: multi-spec collision across {labels}");
                    let clarify_id = self
                        .bd
                        .create(CreateOpts {
                            title,
                            description: body,
                            issue_type: Some("task".to_string()),
                            priority: Some(2),
                            labels: vec!["loom:clarify".to_string()],
                            ..CreateOpts::default()
                        })
                        .await?;
                    info!(
                        label = %self.label,
                        clarify_id = %clarify_id,
                        "loom todo: multi-spec collision → clarify bead minted",
                    );
                    return Err(TodoError::MultiSpecCollision {
                        clarify_id: clarify_id.as_str().to_owned(),
                    });
                }
            }
        };
        debug!(label = %self.label, ?molecule_id, "multi-spec fan-out classification");

        match self.state.spec(&self.label) {
            Ok(_) => (),
            Err(loom_driver::state::StateError::SpecNotFound { .. }) => (),
            Err(e) => return Err(TodoError::State(e)),
        }

        let implementation_notes = self
            .state
            .notes_list(Some(&self.label), Some("implementation"))?
            .into_iter()
            .map(|row| row.text)
            .collect::<Vec<_>>();

        let key = resolve_scratch_key(Phase::Todo, &self.label, None);
        let scratchpad_path =
            loom_driver::scratch::ScratchSession::scratchpad_path_for(&self.workspace, &key)
                .to_string_lossy()
                .into_owned();
        let base = TemplateBaseFields {
            label: self.label.clone(),
            spec_path: spec_path.to_string_lossy().into_owned(),
            pinned_context: String::new(),
            companion_paths: vec![],
            implementation_notes,
            scratchpad_path,
        };
        let cache_path = self.workspace.join(".wrapix/loom/gate-cache.sqlite");
        let criterion_status = build_criterion_status(
            &self.workspace,
            &cache_path,
            &self.label,
            &spec_path,
            self.git.as_ref(),
        )
        .await;
        let ctx = build_template_context(molecule_id, &touched, base, criterion_status);
        let body = match ctx {
            TodoTemplateContext::New(c) => c.render()?,
            TodoTemplateContext::Update(c) => c.render()?,
        };
        Ok(body)
    }
}

impl<R: CommandRunner> TodoController for ProductionTodoController<R> {
    async fn build_session(&mut self) -> Result<TodoSession, TodoError> {
        let prompt = self.build_prompt().await?;
        // Pre-session task-bead snapshot for the productive-completion
        // fan-out guard. Captured before the agent spawns so a post-
        // session re-snapshot in `record_outcome` can detect new mints.
        self.pre_snapshot = Some(self.snapshot_task_beads().await?);
        let entry = self.manifest.lookup(&self.phase_default)?;
        let banner = format!("loom todo @ {}", self.label);
        let key = resolve_scratch_key(Phase::Todo, &self.label, None);
        let scratch =
            loom_driver::scratch::ScratchSession::open(&self.workspace, &key, &prompt, &banner)
                .map_err(|source| {
                    TodoError::Protocol(loom_driver::agent::ProtocolError::Io(source))
                })?;
        info!(
            label = %self.label,
            workspace = %self.workspace.display(),
            image_ref = %entry.r#ref,
            scratch_dir = %scratch.path().display(),
            "loom todo: building spawn config",
        );
        let scratch_dir = scratch.path().to_path_buf();
        let mut env = Vec::new();
        set_loom_inside(&mut env);
        Ok(TodoSession {
            config: SpawnConfig {
                image_ref: entry.r#ref.clone(),
                image_source: entry.source.clone(),
                workspace: self.workspace.clone(),
                env,
                mounts: vec![],
                initial_prompt: prompt,
                agent_args: vec![],
                repin: RePinContent {
                    orientation: String::new(),
                    pinned_context: String::new(),
                    partial_bodies: vec![],
                },
                scratch_dir,
                model: None,
                thinking_level: None,
                shutdown_grace: None,
                handshake_timeout: None,
                stall_warn_interval: None,
            },
            scratch,
        })
    }

    async fn record_outcome(
        &mut self,
        outcome: &SessionOutcome,
        marker: Option<&ExitSignal>,
    ) -> Result<(), TodoError> {
        // Decomposition Discipline (`specs/templates.md`): LOOM_CLARIFY
        // from a todo session targets the **molecule epic**, not a leaf
        // bead. The agent has already persisted its `## Options — …`
        // block to the epic notes per gate.md's Options Format Contract
        // before emitting the marker; here we stamp the `loom:clarify`
        // label + status=blocked transition so `bd ready` excludes the
        // epic until a human resolves via `loom msg`.
        if matches!(marker, Some(ExitSignal::Clarify { .. }))
            && let Some(mol_id) = crate::resolve::resolve_open_epic(&self.bd, &self.label).await?
        {
            let bead_id = BeadId::new(mol_id.as_str()).map_err(BdError::CreateInvalidId)?;
            self.bd
                .update(
                    &bead_id,
                    UpdateOpts {
                        status: Some("blocked".to_string()),
                        add_labels: vec!["loom:clarify".to_string()],
                        ..UpdateOpts::default()
                    },
                )
                .await?;
            info!(
                label = %self.label,
                epic = %mol_id,
                "loom todo: LOOM_CLARIFY routed to molecule epic",
            );
        }
        if !base_commit_should_advance(outcome.exit_code, marker) {
            info!(
                label = %self.label,
                exit_code = outcome.exit_code,
                marker = ?marker,
                "loom todo: base_commit not advanced — gate requires exit_code==0 AND LOOM_COMPLETE/LOOM_NOOP",
            );
            return Ok(());
        }
        // Productive-completion fan-out guard per `specs/harness.md`
        // *Productive-completion gate*: refuse to consume notes when
        // the agent narrated success without minting any task beads.
        if let Some(pre) = self.pre_snapshot.take() {
            let notes_remaining = self
                .state
                .notes_list(Some(&self.label), Some("implementation"))?
                .len();
            if notes_remaining > 0 {
                let post = self.snapshot_task_beads().await?;
                let beads_minted: HashSet<&BeadId> = post.difference(&pre).collect();
                if beads_minted.is_empty() {
                    warn!(
                        label = %self.label,
                        notes_remaining,
                        "loom todo: productive completion with non-empty notes minted zero task beads — refusing to advance base_commit",
                    );
                    return Err(TodoError::ProductiveCompletionWithoutFanout {
                        label: self.label.to_string(),
                        notes_remaining,
                    });
                }
            }
        }
        let Some(mol_id) = crate::resolve::resolve_open_epic(&self.bd, &self.label).await? else {
            // No active molecule + non-empty notes is the same
            // malformed exit the fan-out guard catches above; empty
            // notes is the legitimate audit-only path.
            let notes_remaining = self
                .state
                .notes_list(Some(&self.label), Some("implementation"))?
                .len();
            if notes_remaining > 0 {
                warn!(
                    label = %self.label,
                    notes_remaining,
                    "loom todo: productive completion with non-empty notes but no active molecule — refusing to advance base_commit",
                );
                return Err(TodoError::ProductiveCompletionWithoutFanout {
                    label: self.label.to_string(),
                    notes_remaining,
                });
            }
            warn!(
                label = %self.label,
                "loom todo: productive completion observed but no active molecule — base_commit and notes unchanged",
            );
            return Ok(());
        };
        let head = self
            .git
            .head_commit_sha()
            .await
            .map_err(|e| TodoError::Io(std::io::Error::other(e.to_string())))?;
        let bd = Arc::clone(&self.bd);
        let bd_update: BdUpdateFn = Box::new(move |mol_id, new_base_commit| {
            let bd = Arc::clone(&bd);
            let mol_id_str = mol_id.as_str().to_owned();
            let new_base_commit = new_base_commit.to_owned();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async move {
                    let bead_id = BeadId::new(&mol_id_str).map_err(BdError::CreateInvalidId)?;
                    bd.update(
                        &bead_id,
                        UpdateOpts {
                            set_metadata: vec![(
                                BASE_COMMIT_METADATA_KEY.to_owned(),
                                new_base_commit,
                            )],
                            ..UpdateOpts::default()
                        },
                    )
                    .await
                })
            })
        });
        self.state
            .consume_notes_and_refresh_base_commit(&self.label, &mol_id, &head, bd_update)?;
        info!(
            label = %self.label,
            head = %head,
            mol_id = %mol_id,
            marker = ?marker,
            "loom todo: implementation notes consumed and base_commit refreshed atomically",
        );
        Ok(())
    }
}

/// Productive-completion gate: a `loom todo` session advances
/// `loom.base_commit` only when the marker is `LOOM_COMPLETE` /
/// `LOOM_NOOP` and the agent process exited zero — backend errors,
/// network drops, and swallowed-marker turns must not skip the diff.
fn base_commit_should_advance(exit_code: i32, marker: Option<&ExitSignal>) -> bool {
    exit_code == 0 && matches!(marker, Some(ExitSignal::Complete | ExitSignal::Noop))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Five terminal marker shapes × two exit codes — ten rows, two
    /// truths: only `LOOM_COMPLETE`/`LOOM_NOOP` paired with `exit_code==0`
    /// advances `loom.base_commit` (per `specs/harness.md`
    /// *Productive-completion gate*).
    #[test]
    fn base_commit_should_advance_only_on_complete_or_noop_with_clean_exit() {
        let blocked = ExitSignal::Blocked {
            reason: "missing schema".into(),
        };
        let clarify = ExitSignal::Clarify {
            question: "additive only?".into(),
        };
        let cases: &[(Option<&ExitSignal>, i32, bool, &str)] = &[
            (Some(&ExitSignal::Complete), 0, true, "complete + exit 0"),
            (Some(&ExitSignal::Noop), 0, true, "noop + exit 0"),
            (Some(&blocked), 0, false, "blocked + exit 0"),
            (Some(&clarify), 0, false, "clarify + exit 0"),
            (None, 0, false, "no marker + exit 0"),
            (Some(&ExitSignal::Complete), 1, false, "complete + exit 1"),
            (Some(&ExitSignal::Noop), 1, false, "noop + exit 1"),
            (Some(&blocked), 1, false, "blocked + exit 1"),
            (Some(&clarify), 1, false, "clarify + exit 1"),
            (None, 1, false, "no marker + exit 1"),
        ];
        for (marker, exit_code, expected, label) in cases {
            assert_eq!(
                base_commit_should_advance(*exit_code, *marker),
                *expected,
                "case `{label}`: expected advance={expected}",
            );
        }
    }
}

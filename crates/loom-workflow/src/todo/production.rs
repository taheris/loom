//! Production [`TodoController`] used by the `loom todo` binary.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use askama::Template;
use loom_driver::agent::{AgentRuntime, SessionOutcome};
use loom_driver::bd::{
    BdClient, CommandRunner, CreateOpts, Label, ListOpts, TokioRunner, UpdateOpts,
};
use loom_driver::config::LoomTopConfig;
use loom_driver::git::GitClient;
use loom_driver::identifier::{BeadId, MoleculeId, ProfileName, SpecLabel};
use loom_driver::profile_manifest::ProfileImageManifest;
use loom_driver::state::{CacheDb, SpecEpicRow, WorkEpicRow};
use loom_protocol::todo::{GitSha, TodoSpecOutcome, TodoSuccess};
use tracing::{debug, info};

use super::ExitSignal;
use super::context::{
    FingerprintSpecInput, TemplateBaseFields, build_template_context, changed_spec_context,
    implementation_notes_context, spec_epic_context, todo_fingerprint,
};
use super::criterion_status::build_criterion_status;
use super::error::TodoError;
use super::runner::{TodoController, TodoRecord, TodoSession, TodoSpecSummary};

const TODO_HEAD_METADATA_KEY: &str = "loom.todo_head";
const TODO_FINGERPRINT_METADATA_KEY: &str = "loom.todo_fingerprint";
const TODO_CURSOR_METADATA_KEY: &str = "loom.todo_cursor";
const TODO_SPECS_METADATA_KEY: &str = "loom.todo_specs";

pub struct ProductionTodoController<R: CommandRunner = TokioRunner> {
    workspace: PathBuf,
    state: Arc<CacheDb>,
    manifest: Arc<ProfileImageManifest>,
    phase_default: ProfileName,
    runtime: AgentRuntime,
    git: Arc<GitClient>,
    bd: Arc<BdClient<R>>,
    #[expect(
        dead_code,
        reason = "CLI flag retained until the deterministic todo surface removes --since"
    )]
    since: Option<String>,
    preflight: Option<Preflight>,
    loom_cfg: LoomTopConfig,
}

#[derive(Debug, Clone)]
struct Preflight {
    head: GitSha,
    fingerprint: loom_protocol::todo::TodoFingerprint,
    changed_specs: Vec<ChangedSpec>,
    work_epic: BeadId,
}

#[derive(Debug, Clone)]
struct ChangedSpec {
    label: SpecLabel,
    spec_path: String,
    spec_epic: BeadId,
    todo_cursor: Option<String>,
    initialized: bool,
}

#[derive(Debug, Clone)]
struct IndexedSpec {
    label: SpecLabel,
    spec_path: String,
}

impl<R: CommandRunner> ProductionTodoController<R> {
    #[expect(clippy::too_many_arguments, reason = "controller construction surface")]
    pub fn new(
        _label: SpecLabel,
        workspace: PathBuf,
        state: Arc<CacheDb>,
        manifest: Arc<ProfileImageManifest>,
        phase_default: ProfileName,
        git: Arc<GitClient>,
        bd: Arc<BdClient<R>>,
        since: Option<String>,
    ) -> Self {
        Self::for_workspace(workspace, state, manifest, phase_default, git, bd, since)
    }

    pub fn for_workspace(
        workspace: PathBuf,
        state: Arc<CacheDb>,
        manifest: Arc<ProfileImageManifest>,
        phase_default: ProfileName,
        git: Arc<GitClient>,
        bd: Arc<BdClient<R>>,
        since: Option<String>,
    ) -> Self {
        Self {
            workspace,
            state,
            manifest,
            phase_default,
            runtime: AgentRuntime::Pi,
            git,
            bd,
            since,
            preflight: None,
            loom_cfg: LoomTopConfig::default(),
        }
    }

    pub fn with_loom_config(mut self, cfg: LoomTopConfig) -> Self {
        self.loom_cfg = cfg;
        self
    }

    pub fn with_agent_runtime(mut self, runtime: AgentRuntime) -> Self {
        self.runtime = runtime;
        self
    }

    async fn preflight(&self) -> Result<Option<Preflight>, TodoError> {
        let indexed = parse_spec_index(&self.workspace)?;
        let mut changed = Vec::new();
        for spec in &indexed {
            self.state.upsert_spec(&spec.label, &spec.spec_path)?;
            let epic = self.ensure_spec_epic(spec).await?;
            if epic.initialized {
                changed.push(epic);
                continue;
            }
            if self
                .spec_changed_since_cursor(spec, epic.todo_cursor.as_deref())
                .await?
            {
                changed.push(epic);
            }
        }
        if changed.is_empty() {
            return Ok(None);
        }
        changed.sort_by(|left, right| left.label.as_str().cmp(right.label.as_str()));
        let head = self.git.head_commit_sha().await?;
        let docs_blob = self.git.head_blob_sha(Path::new("docs/README.md")).await?;
        let mut fingerprint_specs = Vec::with_capacity(changed.len());
        for spec in &changed {
            let blob = self.git.head_blob_sha(Path::new(&spec.spec_path)).await?;
            fingerprint_specs.push(FingerprintSpecInput {
                label: spec.label.clone(),
                spec_path: spec.spec_path.clone(),
                spec_blob_sha: blob.to_string(),
                spec_epic_id: spec.spec_epic.clone(),
                todo_cursor: spec.todo_cursor.clone(),
                initialized: spec.initialized,
            });
        }
        let fingerprint = todo_fingerprint(&head, docs_blob.as_str(), &fingerprint_specs);
        let work_epic = self.ensure_work_epic(&head, &fingerprint, &changed).await?;
        Ok(Some(Preflight {
            head,
            fingerprint,
            changed_specs: changed,
            work_epic,
        }))
    }

    async fn ensure_spec_epic(&self, spec: &IndexedSpec) -> Result<ChangedSpec, TodoError> {
        let epics = self.spec_epics_for(&spec.label).await?;
        if epics.len() > 1 {
            let ids = epics
                .iter()
                .map(|bead| bead.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(TodoError::DuplicateSpecEpics {
                label: spec.label.to_string(),
                ids,
            });
        }
        let (spec_epic, todo_cursor, initialized) = match epics.first() {
            Some(bead) => {
                let cursor = metadata_string(bead, TODO_CURSOR_METADATA_KEY).ok_or_else(|| {
                    TodoError::MissingSpecCursor {
                        label: spec.label.to_string(),
                        epic_id: bead.id.to_string(),
                    }
                })?;
                self.validate_cursor(&spec.label, &bead.id, &cursor).await?;
                (bead.id.clone(), Some(cursor), false)
            }
            None => {
                let id = self
                    .bd
                    .create(CreateOpts {
                        title: format!("loom spec: {}", spec.label),
                        description: format!("Spec metadata epic for `{}`.", spec.label),
                        issue_type: Some("epic".to_string()),
                        priority: Some(2),
                        labels: vec!["loom:spec".to_string(), format!("spec:{}", spec.label)],
                        ..CreateOpts::default()
                    })
                    .await?;
                (id, None, true)
            }
        };
        let molecule_id = MoleculeId::new(spec_epic.as_str().to_owned());
        self.state.upsert_spec_epic(&SpecEpicRow {
            spec_label: spec.label.clone(),
            epic_id: molecule_id,
            todo_cursor: todo_cursor.clone(),
        })?;
        Ok(ChangedSpec {
            label: spec.label.clone(),
            spec_path: spec.spec_path.clone(),
            spec_epic,
            todo_cursor,
            initialized,
        })
    }

    async fn spec_epics_for(
        &self,
        label: &SpecLabel,
    ) -> Result<Vec<loom_driver::bd::Bead>, TodoError> {
        let mut out = Vec::new();
        for status in ["open", "in_progress", "closed"] {
            let beads = self
                .bd
                .list(ListOpts {
                    issue_type: Some("epic".to_string()),
                    label: Some(format!("spec:{label}")),
                    status: Some(status.to_string()),
                    ..Default::default()
                })
                .await?;
            out.extend(beads.into_iter().filter(has_label(Label::is_spec_epic)));
        }
        Ok(out)
    }

    async fn validate_cursor(
        &self,
        label: &SpecLabel,
        epic_id: &BeadId,
        cursor: &str,
    ) -> Result<(), TodoError> {
        if GitSha::new(cursor).is_err() {
            return Err(TodoError::InvalidSpecCursor {
                label: label.to_string(),
                epic_id: epic_id.to_string(),
                cursor: cursor.to_string(),
                reason: "not a full git SHA".to_string(),
            });
        }
        if !self.git.rev_exists(cursor).await? {
            return Err(TodoError::InvalidSpecCursor {
                label: label.to_string(),
                epic_id: epic_id.to_string(),
                cursor: cursor.to_string(),
                reason: "commit does not exist".to_string(),
            });
        }
        if !self.git.is_ancestor_of_head(cursor).await? {
            return Err(TodoError::InvalidSpecCursor {
                label: label.to_string(),
                epic_id: epic_id.to_string(),
                cursor: cursor.to_string(),
                reason: "commit is not an ancestor of HEAD".to_string(),
            });
        }
        Ok(())
    }

    async fn spec_changed_since_cursor(
        &self,
        spec: &IndexedSpec,
        cursor: Option<&str>,
    ) -> Result<bool, TodoError> {
        let Some(cursor) = cursor else {
            return Ok(true);
        };
        let spec_path = Path::new(&spec.spec_path);
        if self.git.path_changed_since(cursor, spec_path).await? {
            return Ok(true);
        }
        let old_index = self
            .git
            .file_at_revision(cursor, Path::new("docs/README.md"))
            .await?
            .unwrap_or_default();
        let old_rows = parse_spec_index_content(&old_index)?;
        let old_path = old_rows
            .iter()
            .find(|row| row.label == spec.label)
            .map(|row| row.spec_path.as_str());
        Ok(old_path != Some(spec.spec_path.as_str()))
    }

    async fn ensure_work_epic(
        &self,
        head: &GitSha,
        fingerprint: &loom_protocol::todo::TodoFingerprint,
        changed_specs: &[ChangedSpec],
    ) -> Result<BeadId, TodoError> {
        let pending = self.pending_todo_epics().await?;
        let (matching, nonmatching): (Vec<_>, Vec<_>) = pending.into_iter().partition(|bead| {
            metadata_string(bead, TODO_HEAD_METADATA_KEY).as_deref() == Some(head.as_str())
                && metadata_string(bead, TODO_FINGERPRINT_METADATA_KEY).as_deref()
                    == Some(fingerprint.as_str())
        });
        if matching.len() == 1 && nonmatching.is_empty() {
            return Ok(matching[0].id.clone());
        }
        if matching.len() > 1 || !nonmatching.is_empty() {
            let ids = matching
                .iter()
                .chain(nonmatching.iter())
                .map(|bead| bead.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(TodoError::PendingTodoEpicConflict {
                ids,
                diagnostic: pending_todo_options(head, fingerprint),
            });
        }
        let specs = changed_specs
            .iter()
            .map(|spec| spec.label.to_string())
            .collect::<Vec<_>>();
        let metadata = serde_json::json!({
            TODO_HEAD_METADATA_KEY: head.as_str(),
            TODO_FINGERPRINT_METADATA_KEY: fingerprint.as_str(),
            TODO_SPECS_METADATA_KEY: specs,
        })
        .to_string();
        let mut labels = vec!["loom:todo".to_string()];
        labels.extend(
            changed_specs
                .iter()
                .map(|spec| format!("spec:{}", spec.label)),
        );
        let work_epic = self
            .bd
            .create(CreateOpts {
                title: format!("loom todo work: {}", specs.join(", ")),
                description: "Driver-created work epic for deterministic loom todo decomposition."
                    .to_string(),
                issue_type: Some("epic".to_string()),
                priority: Some(2),
                labels,
                metadata: Some(metadata),
                ..CreateOpts::default()
            })
            .await?;
        self.state.upsert_work_epic(&WorkEpicRow {
            epic_id: MoleculeId::new(work_epic.as_str().to_owned()),
            todo_head: Some(head.to_string()),
            todo_fingerprint: Some(fingerprint.to_string()),
            is_active: false,
            iteration_count: 0,
        })?;
        Ok(work_epic)
    }

    async fn pending_todo_epics(&self) -> Result<Vec<loom_driver::bd::Bead>, TodoError> {
        let beads = self
            .bd
            .list(ListOpts {
                issue_type: Some("epic".to_string()),
                label: Some("loom:todo".to_string()),
                status: Some("open".to_string()),
                ..Default::default()
            })
            .await?;
        Ok(beads
            .into_iter()
            .filter(has_label(Label::is_todo_stage))
            .collect())
    }

    async fn build_prompt(&mut self) -> Result<Option<String>, TodoError> {
        let Some(preflight) = self.preflight().await? else {
            self.preflight = None;
            return Ok(None);
        };
        let mut implementation_notes = Vec::new();
        let mut companion_paths = Vec::new();
        let mut spec_epics = Vec::new();
        let mut changed_specs = Vec::new();
        let mut criterion_status = Vec::new();
        for spec in &preflight.changed_specs {
            let notes = self
                .state
                .notes_list(Some(&spec.label), Some("implementation"))?
                .into_iter()
                .map(|row| row.text)
                .collect::<Vec<_>>();
            implementation_notes.push(implementation_notes_context(spec.label.clone(), notes));
            companion_paths.extend(self.state.companions(&spec.label)?);
            spec_epics.push(spec_epic_context(
                spec.label.clone(),
                Some(MoleculeId::new(spec.spec_epic.as_str().to_owned())),
                spec.todo_cursor.clone(),
            ));
            let diff = match spec.todo_cursor.as_deref() {
                Some(cursor) => Some(
                    self.git
                        .diff_spec(cursor, Path::new(&spec.spec_path))
                        .await?,
                ),
                None => None,
            };
            changed_specs.push(changed_spec_context(
                spec.label.clone(),
                spec.spec_path.clone(),
                diff,
            ));
            let cache_path = self.workspace.join(".loom/cache.db");
            criterion_status.extend(
                build_criterion_status(
                    &self.workspace,
                    &cache_path,
                    &spec.label,
                    Path::new(&spec.spec_path),
                    self.git.as_ref(),
                )
                .await,
            );
        }
        companion_paths.sort();
        companion_paths.dedup();
        let key = preflight.work_epic.as_str();
        let scratchpad_path =
            loom_driver::scratch::ScratchSession::scratchpad_path_for(&self.workspace, key)
                .to_string_lossy()
                .into_owned();
        let spec_index = std::fs::read_to_string(self.workspace.join("docs/README.md"))?;
        let base = TemplateBaseFields {
            pinned_context: String::new(),
            spec_index,
            changed_specs,
            work_epic: preflight.work_epic.clone(),
            todo_head: preflight.head.clone(),
            todo_fingerprint: preflight.fingerprint.clone(),
            spec_epics,
            companion_paths,
            implementation_notes,
            scratchpad_path,
        };
        let ctx = build_template_context(base, criterion_status);
        self.preflight = Some(preflight);
        Ok(Some(ctx.render()?))
    }

    async fn validate_success(&self, success: &TodoSuccess) -> Result<(), TodoError> {
        let preflight = self
            .preflight
            .as_ref()
            .ok_or(TodoError::TodoSuccessWithoutPreflight)?;
        if success.head != preflight.head {
            return Err(TodoError::TodoValidation {
                detail: "LOOM_TODO head did not match preflight HEAD".to_string(),
            });
        }
        if success.fingerprint != preflight.fingerprint {
            return Err(TodoError::TodoValidation {
                detail: "LOOM_TODO fingerprint did not match preflight fingerprint".to_string(),
            });
        }
        if success.work_epic != preflight.work_epic {
            return Err(TodoError::TodoValidation {
                detail: "LOOM_TODO work_epic did not match driver-created work epic".to_string(),
            });
        }
        let expected = preflight
            .changed_specs
            .iter()
            .map(|spec| spec.label.to_string())
            .collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        for spec in success.specs.as_slice() {
            let label = spec.label.to_string();
            if !seen.insert(label.clone()) {
                return Err(TodoError::TodoValidation {
                    detail: format!("LOOM_TODO reported duplicate spec `{}`", spec.label),
                });
            }
            if !expected.contains(&label) {
                return Err(TodoError::TodoValidation {
                    detail: format!("LOOM_TODO reported unexpected spec `{}`", spec.label),
                });
            }
            if let TodoSpecOutcome::Decomposed { beads } = &spec.outcome {
                for bead_id in beads.as_slice() {
                    let bead = self.bd.show(bead_id).await?;
                    if bead.parent.as_ref() != Some(&preflight.work_epic) {
                        return Err(TodoError::TodoValidation {
                            detail: format!(
                                "bead `{bead_id}` is not parented under work epic `{}`",
                                preflight.work_epic
                            ),
                        });
                    }
                }
            }
        }
        if seen != expected {
            let missing = expected
                .difference(&seen)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(TodoError::TodoValidation {
                detail: format!("LOOM_TODO omitted changed spec(s): {missing}"),
            });
        }
        Ok(())
    }

    async fn record_validation_failure(&self, detail: &str) -> Result<(), TodoError> {
        let Some(preflight) = self.preflight.as_ref() else {
            return Ok(());
        };
        self.bd
            .update(
                &preflight.work_epic,
                UpdateOpts {
                    notes: Some(format!("LOOM_TODO validation failed: {detail}")),
                    ..UpdateOpts::default()
                },
            )
            .await?;
        Ok(())
    }

    async fn render_notes_into_beads(&self, success: &TodoSuccess) -> Result<(), TodoError> {
        for spec in success.specs.as_slice() {
            let TodoSpecOutcome::Decomposed { beads } = &spec.outcome else {
                continue;
            };
            let notes = self
                .state
                .notes_list(Some(&spec.label), Some("implementation"))?
                .into_iter()
                .map(|row| row.text)
                .collect::<Vec<_>>();
            if notes.is_empty() {
                continue;
            }
            let rendered = render_implementation_notes(&notes);
            for bead_id in beads.as_slice() {
                let bead = self.bd.show(bead_id).await?;
                let notes = append_note_block(bead.notes.as_deref(), &rendered);
                self.bd
                    .update(
                        bead_id,
                        UpdateOpts {
                            notes: Some(notes),
                            ..UpdateOpts::default()
                        },
                    )
                    .await?;
            }
        }
        Ok(())
    }

    async fn finalize_success(&self, success: &TodoSuccess) -> Result<TodoRecord, TodoError> {
        let preflight = self
            .preflight
            .as_ref()
            .ok_or(TodoError::TodoSuccessWithoutPreflight)?;
        self.render_notes_into_beads(success).await?;
        for active in self.active_work_epics().await? {
            if active.id != preflight.work_epic {
                self.bd
                    .update(
                        &active.id,
                        UpdateOpts {
                            remove_labels: vec!["loom:active".to_string()],
                            ..UpdateOpts::default()
                        },
                    )
                    .await?;
            }
        }
        for spec in &preflight.changed_specs {
            self.bd
                .update(
                    &spec.spec_epic,
                    UpdateOpts {
                        set_metadata: vec![(
                            TODO_CURSOR_METADATA_KEY.to_string(),
                            preflight.head.to_string(),
                        )],
                        ..UpdateOpts::default()
                    },
                )
                .await?;
            self.state.upsert_spec_epic(&SpecEpicRow {
                spec_label: spec.label.clone(),
                epic_id: MoleculeId::new(spec.spec_epic.as_str().to_owned()),
                todo_cursor: Some(preflight.head.to_string()),
            })?;
            self.state
                .notes_clear(&spec.label, Some("implementation"))?;
        }
        self.bd
            .update(
                &preflight.work_epic,
                UpdateOpts {
                    add_labels: vec!["loom:active".to_string()],
                    remove_labels: vec!["loom:todo".to_string()],
                    ..UpdateOpts::default()
                },
            )
            .await?;
        self.state.upsert_work_epic(&WorkEpicRow {
            epic_id: MoleculeId::new(preflight.work_epic.as_str().to_owned()),
            todo_head: Some(preflight.head.to_string()),
            todo_fingerprint: Some(preflight.fingerprint.to_string()),
            is_active: true,
            iteration_count: 0,
        })?;
        Ok(TodoRecord {
            spec_outcomes: summarize_success(success),
        })
    }

    async fn record_blocked(&self, reason: &str) -> Result<TodoRecord, TodoError> {
        let Some(preflight) = self.preflight.as_ref() else {
            return Ok(TodoRecord::default());
        };
        self.bd
            .update(
                &preflight.work_epic,
                UpdateOpts {
                    status: Some("blocked".to_string()),
                    add_labels: vec!["loom:blocked".to_string()],
                    notes: Some(reason.to_string()),
                    ..UpdateOpts::default()
                },
            )
            .await?;
        Ok(TodoRecord {
            spec_outcomes: preflight
                .changed_specs
                .iter()
                .map(|spec| TodoSpecSummary {
                    label: spec.label.clone(),
                    outcome: format!("blocked: {reason}"),
                })
                .collect(),
        })
    }

    async fn active_work_epics(&self) -> Result<Vec<loom_driver::bd::Bead>, TodoError> {
        let beads = self
            .bd
            .list(ListOpts {
                issue_type: Some("epic".to_string()),
                label: Some("loom:active".to_string()),
                status: Some("open".to_string()),
                ..Default::default()
            })
            .await?;
        Ok(beads
            .into_iter()
            .filter(has_label(Label::is_active))
            .collect())
    }
}

impl<R: CommandRunner> TodoController for ProductionTodoController<R> {
    async fn build_session(&mut self) -> Result<TodoSession, TodoError> {
        let prompt = self
            .build_prompt()
            .await?
            .ok_or(TodoError::NoChangedSpecs)?;
        let entry = self.manifest.lookup(&self.phase_default, self.runtime)?;
        let preflight = self
            .preflight
            .as_ref()
            .ok_or(TodoError::TodoSuccessWithoutPreflight)?;
        let banner = format!("loom todo @ {}", preflight.work_epic);
        let key = preflight.work_epic.as_str();
        let scratch =
            loom_driver::scratch::ScratchSession::open(&self.workspace, key, &prompt, &banner)
                .map_err(|source| {
                    TodoError::Protocol(loom_driver::agent::ProtocolError::Io(source))
                })?;
        info!(
            work_epic = %preflight.work_epic,
            workspace = %self.workspace.display(),
            image_ref = %entry.r#ref,
            scratch_dir = %scratch.path().display(),
            "loom todo: building spawn config",
        );
        let scratch_dir = scratch.path().to_path_buf();
        let mounts = crate::r#loop::sccache_mount(&self.loom_cfg)
            .map_err(|source| TodoError::Protocol(loom_driver::agent::ProtocolError::Io(source)))?
            .into_iter()
            .collect();
        let config = crate::spawn::build_spawn_config(
            entry,
            self.runtime,
            self.workspace.clone(),
            prompt,
            scratch_dir,
            self.loom_cfg.container_sccache_env(),
            vec![],
            mounts,
            vec![],
        );
        Ok(TodoSession { config, scratch })
    }

    async fn record_outcome(
        &mut self,
        outcome: &SessionOutcome,
        marker: Option<&ExitSignal>,
        todo_success: Option<&TodoSuccess>,
    ) -> Result<TodoRecord, TodoError> {
        if let Some(ExitSignal::Clarify { .. }) = marker {
            if let Some(preflight) = self.preflight.as_ref() {
                let applied =
                    crate::gate_clarify::apply_clarify_or_blocked(&self.bd, &preflight.work_epic)
                        .await?;
                info!(work_epic = %preflight.work_epic, outcome = ?applied, "loom todo: LOOM_CLARIFY routed to work epic");
            }
            return Ok(TodoRecord::default());
        }
        if let Some(ExitSignal::Blocked { reason }) = marker {
            return self.record_blocked(reason).await;
        }
        if matches!(marker, Some(ExitSignal::Complete | ExitSignal::Noop)) {
            return Err(TodoError::GenericTodoMarker);
        }
        let Some(success) = todo_success else {
            debug!(exit_code = outcome.exit_code, marker = ?marker, "loom todo: no success payload to finalize");
            return Ok(TodoRecord::default());
        };
        if outcome.exit_code != 0 {
            return Ok(TodoRecord::default());
        }
        if let Err(err) = self.validate_success(success).await {
            if let TodoError::TodoValidation { detail } = &err {
                self.record_validation_failure(detail).await?;
            }
            return Err(err);
        }
        self.finalize_success(success).await
    }
}

fn parse_spec_index(workspace: &Path) -> Result<Vec<IndexedSpec>, TodoError> {
    let content = std::fs::read_to_string(workspace.join("docs/README.md"))?;
    parse_spec_index_content(&content)
}

fn parse_spec_index_content(content: &str) -> Result<Vec<IndexedSpec>, TodoError> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for line in content.lines() {
        let Some(start) = line.find("](../specs/") else {
            continue;
        };
        let path_start = start + "](../".len();
        let Some(rest) = line.get(path_start..) else {
            continue;
        };
        let Some(end) = rest.find(')') else {
            continue;
        };
        let spec_path = &rest[..end];
        let Some(label) = spec_path
            .strip_prefix("specs/")
            .and_then(|path| path.strip_suffix(".md"))
        else {
            continue;
        };
        if !seen.insert(label.to_string()) {
            return Err(TodoError::SpecIndex {
                detail: format!("duplicate index row for spec `{label}`"),
            });
        }
        out.push(IndexedSpec {
            label: label.parse().map_err(|_| TodoError::SpecIndex {
                detail: format!("invalid spec label `{label}`"),
            })?,
            spec_path: spec_path.to_string(),
        });
    }
    if out.is_empty() {
        return Err(TodoError::SpecIndex {
            detail: "no specs indexed in docs/README.md".to_string(),
        });
    }
    Ok(out)
}

fn metadata_string(bead: &loom_driver::bd::Bead, key: &str) -> Option<String> {
    bead.metadata
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

fn has_label(pred: fn(&Label) -> bool) -> impl Fn(&loom_driver::bd::Bead) -> bool {
    move |bead| bead.labels.iter().any(pred)
}

fn pending_todo_options(
    head: &GitSha,
    fingerprint: &loom_protocol::todo::TodoFingerprint,
) -> String {
    format!(
        "## Options — Resolve pending todo epic\n\n### Option 1 — Continue matching batch\nClose or relabel non-matching `loom:todo` epics, leaving exactly one open epic with `{TODO_HEAD_METADATA_KEY}={head}` and `{TODO_FINGERPRINT_METADATA_KEY}={fingerprint}`. Cost: manual bd cleanup.\n\n### Option 2 — Restart decomposition\nRemove `loom:todo` from all pending todo epics, then rerun `loom todo` so the driver creates a fresh batch. Cost: any useful draft decomposition must be copied manually."
    )
}

fn render_implementation_notes(notes: &[String]) -> String {
    let mut out = String::from("Implementation notes:\n");
    for note in notes {
        out.push_str("\n- ");
        out.push_str(note);
    }
    out
}

fn append_note_block(existing: Option<&str>, block: &str) -> String {
    let Some(existing) = existing.filter(|value| !value.trim().is_empty()) else {
        return block.to_string();
    };
    format!("{}\n\n{}", existing.trim_end(), block)
}

fn summarize_success(success: &TodoSuccess) -> Vec<TodoSpecSummary> {
    success
        .specs
        .as_slice()
        .iter()
        .map(|spec| {
            let outcome = match &spec.outcome {
                TodoSpecOutcome::Decomposed { beads } => {
                    let bead_list = beads
                        .as_slice()
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("decomposed: {bead_list}")
                }
                TodoSpecOutcome::NoWork { reason } => format!("no-work: {reason}"),
            };
            TodoSpecSummary {
                label: spec.label.clone(),
                outcome,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_spec_index_reads_docs_rows() {
        let rows = parse_spec_index_content(
            "- [Harness](../specs/harness.md)\n- [Templates](../specs/templates.md)\n",
        )
        .expect("index parses");
        assert_eq!(rows[0].label, SpecLabel::new("harness"));
        assert_eq!(rows[1].spec_path, "specs/templates.md");
    }

    #[test]
    fn parse_spec_index_rejects_duplicate_labels() {
        let err = parse_spec_index_content(
            "- [Harness](../specs/harness.md)\n- [Harness again](../specs/harness.md)\n",
        )
        .expect_err("duplicate rejected");
        assert!(matches!(err, TodoError::SpecIndex { .. }));
    }
}

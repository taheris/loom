use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use displaydoc::Display;
use loom_driver::bd::{BdClient, CreateOpts, UpdateOpts};
use loom_driver::clock::SystemClock;
use loom_driver::config::{LoomConfig, LoomConfigError};
use loom_driver::git::{GitClient, GitError};
use loom_driver::identifier::BeadId;
use loom_driver::lock::{LockError, LockManager, PhaseLock};
use loom_skills::builtin::{self, CatalogError};
use loom_skills::discovery::{DiscoveryError, load_workspace};
use loom_skills::identity::{PhaseName, SkillName};
use loom_skills::registry::{NamedSkill, RegistryError, SkillRegistry};
use loom_skills::source::SkillSource;
use loom_tune::case::{Document, LoadContext, LoadError, LoadedCases, load_documents};
use loom_tune::checker::{
    Level, Registry as CheckerRegistry, RegistryError as CheckerRegistryError,
};
use loom_tune::config::{FileConfig as TuneFileConfig, TuneConfig};
use loom_tune::evidence::{
    Item, ItemId, RootReport, Snapshot as EvidenceSnapshot, SplitError, Splitter,
};
use loom_tune::executor::{self as tune_executor, Artifact as TuneArtifact};
use loom_tune::gate::{self as tune_gate, Outcome as GateOutcome, State as GateState};
use loom_tune::plan::{self, FrozenPlan, PlanError};
use loom_tune::proposal::{
    Caps, CaseCounts, LocalPaths, ManifestInput, OutcomeCounts, ProposalManifest, State,
    ValidationRow, ValidationStatus,
};
use loom_tune::target::{Catalog as TargetCatalog, PartialName, Target};
use thiserror::Error;
use tracing::warn;

/// Tune command requested by the CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    List(ListSurface),
    Propose(ProposeRequest),
}

/// Read-only tune listing surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListSurface {
    Skill,
    Phase,
    Partial,
    Checker,
    All,
}

/// Artifact surface being tuned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Skill,
    Phase,
    Partial,
    All,
}

impl fmt::Display for Surface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Skill => f.write_str("skill"),
            Self::Phase => f.write_str("phase"),
            Self::Partial => f.write_str("partial"),
            Self::All => f.write_str("all"),
        }
    }
}

/// Proposal-creating tune request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposeRequest {
    pub surface: Surface,
    pub level: Level,
    pub targets: Vec<String>,
    pub dry_run: bool,
    pub seed: Option<u64>,
}

/// Renderable tune command response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Listing(String),
    DryRun(String),
    Proposal(ProposalReport),
}

impl Response {
    pub fn render(&self) -> String {
        match self {
            Self::Listing(text) | Self::DryRun(text) => text.clone(),
            Self::Proposal(report) => report.render(),
        }
    }
}

/// Created proposal summary printed to the operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalReport {
    pub proposal_id: BeadId,
    pub state: State,
    pub envelope: PathBuf,
    pub repo: PathBuf,
    pub branch: String,
    pub base_commit: String,
    pub proposal_head: String,
}

impl ProposalReport {
    fn render(&self) -> String {
        format!(
            "loom tune proposal created: {id}\nstate: {state}\nenvelope: {envelope}\nrepo: {repo}\nbranch: {branch}\nbase: {base}\nhead: {head}\n",
            id = self.proposal_id,
            state = state_name(self.state),
            envelope = self.envelope.display(),
            repo = self.repo.display(),
            branch = self.branch,
            base = self.base_commit,
            head = self.proposal_head,
        )
    }
}

/// Run a tune command against `workspace`.
pub async fn run(workspace: &Path, request: Request) -> Result<Response, TuneError> {
    let context = Context::load(workspace).await?;
    match request {
        Request::List(surface) => Ok(Response::Listing(context.render_listing(surface)?)),
        Request::Propose(proposal) if proposal.dry_run => {
            let plan = context.plan(&proposal)?;
            Ok(Response::DryRun(render_dry_run(&context, &plan)))
        }
        Request::Propose(proposal) => create_proposal(context, &proposal)
            .await
            .map(Response::Proposal),
    }
}

#[derive(Debug)]
struct Context {
    workspace: PathBuf,
    tracked_files: BTreeSet<PathBuf>,
    tune_config: TuneConfig,
    skills: Vec<SkillEntry>,
    phases: Vec<TemplateEntry>,
    partials: Vec<TemplateEntry>,
    checker_registry: CheckerRegistry,
    disabled_checkers: BTreeSet<loom_tune::CheckerId>,
    target_catalog: TargetCatalog,
    root_report: RootReport,
    evidence: EvidenceSnapshot,
    base_commit: String,
}

impl Context {
    async fn load(workspace: &Path) -> Result<Self, TuneError> {
        let workspace =
            fs::canonicalize(workspace).map_err(|source| TuneError::CanonicalizeWorkspace {
                path: workspace.to_path_buf(),
                source,
            })?;
        let config_path = LoomConfig::resolve_path(&workspace);
        let loom_config = LoomConfig::load(&config_path)?;
        let tune_config = load_tune_config(&config_path)?;
        let git = GitClient::open(&workspace)?;
        let tracked_files = git
            .tracked_files()
            .await?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let base_commit = git.head_commit_sha().await?.to_string();
        let skills = load_skill_entries(&workspace, &tracked_files, &loom_config)?;
        let phases = load_phase_entries(&tracked_files);
        let partials = load_partial_entries(&tracked_files);
        let checker_registry = CheckerRegistry::builtin()?;
        let target_catalog = target_catalog(&skills, &phases, &partials);
        let disabled_checkers = tune_config.disabled_checkers(&checker_registry)?;
        let root_report = RootReport::from_config(&workspace, &tune_config.evidence);
        let evidence = build_evidence(&workspace, &tune_config, target_catalog.targets().cloned())?;
        Ok(Self {
            workspace,
            tracked_files,
            tune_config,
            skills,
            phases,
            partials,
            checker_registry,
            disabled_checkers,
            target_catalog,
            root_report,
            evidence,
            base_commit,
        })
    }

    fn render_listing(&self, surface: ListSurface) -> Result<String, TuneError> {
        Ok(match surface {
            ListSurface::Skill => render_skill_listing(&self.skills),
            ListSurface::Phase => render_template_listing("phase", &self.phases),
            ListSurface::Partial => render_template_listing("partial", &self.partials),
            ListSurface::Checker => render_checker_listing(&self.checker_registry),
            ListSurface::All => render_all_listing(self),
        })
    }

    fn plan(&self, proposal: &ProposeRequest) -> Result<PreparedPlan, TuneError> {
        let targets = self.resolve_targets(proposal)?;
        let seed = proposal
            .seed
            .unwrap_or_else(|| generated_seed(&targets, proposal.level, &self.base_commit));
        let loaded_tuning = load_tuning_cases(
            &self.workspace,
            &self.tracked_files,
            &self.skills,
            &self.target_catalog,
            &targets,
            &self.checker_registry,
            &self.disabled_checkers,
        )?;
        let frozen = plan::build(plan::Request {
            targets: targets.clone(),
            level: proposal.level,
            cases: &loaded_tuning.cases,
            evidence: &self.evidence,
            config: &self.tune_config,
            registry: &self.checker_registry,
            seed,
        })?;
        let case_counts = CaseCounts {
            declared: loaded_tuning.cases.cases().len(),
            mined_train: self.evidence.train.len(),
            mined_selection: self.evidence.selection.len(),
            selected: frozen.selected_cases.len(),
            skipped: frozen.skipped_cases.len(),
        };
        Ok(PreparedPlan {
            targets,
            frozen,
            case_counts,
            loaded_cases: loaded_tuning.cases,
            tuning_guidance: loaded_tuning.guidance,
        })
    }

    fn resolve_targets(&self, proposal: &ProposeRequest) -> Result<Vec<Target>, TuneError> {
        match proposal.surface {
            Surface::Skill => self.resolve_skill_targets(&proposal.targets),
            Surface::Phase => self.resolve_phase_targets(&proposal.targets),
            Surface::Partial => self.resolve_partial_targets(&proposal.targets),
            Surface::All => {
                if !proposal.targets.is_empty() {
                    return Err(TuneError::AllRejectsTargets);
                }
                let targets = self
                    .skills
                    .iter()
                    .map(|entry| entry.target.clone())
                    .chain(self.phases.iter().map(|entry| entry.target.clone()))
                    .chain(self.partials.iter().map(|entry| entry.target.clone()))
                    .collect::<Vec<_>>();
                require_non_empty(targets, proposal.surface)
            }
        }
    }

    fn resolve_skill_targets(&self, names: &[String]) -> Result<Vec<Target>, TuneError> {
        if names.is_empty() {
            return require_non_empty(
                self.skills
                    .iter()
                    .map(|entry| entry.target.clone())
                    .collect(),
                Surface::Skill,
            );
        }
        names
            .iter()
            .map(|name| {
                let parsed =
                    SkillName::new(name.clone()).map_err(|source| TuneError::SkillName {
                        name: name.clone(),
                        source,
                    })?;
                let target = Target::Skill { name: parsed };
                if self.skills.iter().any(|entry| entry.target == target) {
                    Ok(target)
                } else {
                    Err(TuneError::UnknownTarget { target })
                }
            })
            .collect()
    }

    fn resolve_phase_targets(&self, names: &[String]) -> Result<Vec<Target>, TuneError> {
        if names.is_empty() {
            return require_non_empty(
                self.phases
                    .iter()
                    .map(|entry| entry.target.clone())
                    .collect(),
                Surface::Phase,
            );
        }
        names
            .iter()
            .map(|name| {
                let parsed =
                    PhaseName::new(name.clone()).map_err(|source| TuneError::PhaseName {
                        name: name.clone(),
                        source,
                    })?;
                let target = Target::Phase { name: parsed };
                if self.phases.iter().any(|entry| entry.target == target) {
                    Ok(target)
                } else {
                    Err(TuneError::UnknownTarget { target })
                }
            })
            .collect()
    }

    fn resolve_partial_targets(&self, names: &[String]) -> Result<Vec<Target>, TuneError> {
        if names.is_empty() {
            return require_non_empty(
                self.partials
                    .iter()
                    .map(|entry| entry.target.clone())
                    .collect(),
                Surface::Partial,
            );
        }
        names
            .iter()
            .map(|name| {
                let parsed =
                    PartialName::new(name.clone()).map_err(|source| TuneError::PartialName {
                        name: name.clone(),
                        source,
                    })?;
                let target = Target::Partial { name: parsed };
                if self.partials.iter().any(|entry| entry.target == target) {
                    Ok(target)
                } else {
                    Err(TuneError::UnknownTarget { target })
                }
            })
            .collect()
    }

    fn candidate_path(&self, target: &Target, repo: &Path) -> Result<PathBuf, TuneError> {
        match target {
            Target::Skill { .. } => self.skill_candidate_path(target, repo),
            Target::Phase { .. } => self.template_candidate_path(target, repo, &self.phases),
            Target::Partial { .. } => self.template_candidate_path(target, repo, &self.partials),
        }
    }

    fn skill_candidate_path(&self, target: &Target, repo: &Path) -> Result<PathBuf, TuneError> {
        let entry = self
            .skills
            .iter()
            .find(|entry| &entry.target == target)
            .ok_or_else(|| TuneError::UnknownTarget {
                target: target.clone(),
            })?;
        if entry.source == SkillSource::BuiltIn
            && !entry.source_path.starts_with(&self.workspace)
            && let Target::Skill { name } = target
        {
            return Ok(repo
                .join(".loom-override/skills")
                .join(name.as_str())
                .join("skill.md"));
        }
        let relative = entry
            .source_path
            .strip_prefix(&self.workspace)
            .map_err(|source| TuneError::TargetOutsideWorkspace {
                path: entry.source_path.clone(),
                source,
            })?;
        Ok(repo.join(relative))
    }

    fn template_candidate_path(
        &self,
        target: &Target,
        repo: &Path,
        entries: &[TemplateEntry],
    ) -> Result<PathBuf, TuneError> {
        let entry = entries
            .iter()
            .find(|entry| &entry.target == target)
            .ok_or_else(|| TuneError::UnknownTarget {
                target: target.clone(),
            })?;
        Ok(repo.join(&entry.relative_path))
    }
}

#[derive(Debug, Clone)]
struct SkillEntry {
    target: Target,
    description: String,
    source: SkillSource,
    source_path: PathBuf,
    applicability: String,
    markdown: String,
    tuning_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct TemplateEntry {
    target: Target,
    relative_path: PathBuf,
}

#[derive(Debug)]
struct PreparedPlan {
    targets: Vec<Target>,
    frozen: FrozenPlan,
    case_counts: CaseCounts,
    loaded_cases: LoadedCases,
    tuning_guidance: Vec<TuningDocumentGuidance>,
}

#[derive(Debug, Clone)]
struct TuningDocumentGuidance {
    path: PathBuf,
    lines: Vec<String>,
}

struct LoadedTuning {
    cases: LoadedCases,
    guidance: Vec<TuningDocumentGuidance>,
}

async fn create_proposal(
    context: Context,
    proposal: &ProposeRequest,
) -> Result<ProposalReport, TuneError> {
    let prepared = context.plan(proposal)?;
    let lock_manager = LockManager::new(&context.workspace)?;
    let clock = SystemClock::new();
    let _guard = lock_manager
        .acquire_phase_async(PhaseLock::Tune, &clock)
        .await?;
    let labels = labels_for_targets(&prepared.targets);
    let title = format!(
        "Tune {} targets at {} level",
        proposal.surface,
        level_name(proposal.level)
    );
    let bead_id = BdClient::new()
        .create(CreateOpts {
            title,
            description: "Tune proposal is being prepared.".to_owned(),
            issue_type: Some("task".to_owned()),
            priority: Some(2),
            labels,
            parent: None,
            metadata: None,
        })
        .await?;
    let envelope = context.workspace.join(".loom/tune").join(bead_id.as_str());
    let repo = envelope.join("repo");
    let local_paths = local_paths(&envelope);
    create_envelope_dirs(&local_paths)?;
    let branch = format!("loom/tune/{bead_id}");
    clone_repo(&context.workspace, &repo, &context.base_commit, &branch).await?;
    let touched = write_candidate_files(&context, proposal, &prepared, &repo)?;
    let artifacts = target_artifacts(&context, &prepared.targets, &repo)?;
    let candidate_validation = validate_candidate(
        &repo,
        &prepared.frozen,
        &prepared.loaded_cases,
        &context.checker_registry,
        &prepared.targets,
        &touched,
        &artifacts,
    )?;
    commit_candidate(&repo, proposal.surface, proposal.level, &bead_id).await?;
    let proposal_head = GitClient::open(&repo)?.head_commit_sha().await?.to_string();
    let state = proposal_state(&candidate_validation.rows);
    let outcome_counts = candidate_validation.outcome_counts.clone();
    let manifest = ProposalManifest::from_plan(ManifestInput {
        proposal_id: bead_id.clone(),
        workspace_path: context.workspace.clone(),
        plan: &prepared.frozen,
        state,
        target_files: touched
            .iter()
            .map(|path| relative_or_original(path, &repo))
            .collect(),
        base_commit: context.base_commit.clone(),
        proposal_branch: branch.clone(),
        proposal_head: proposal_head.clone(),
        case_counts: prepared.case_counts.clone(),
        outcome_counts: outcome_counts.clone(),
        validation: candidate_validation.rows.clone(),
        caps: Caps::from(&context.tune_config.checks),
        local_paths: relative_local_paths(&local_paths, &context.workspace),
    });
    write_manifest(&local_paths.manifest, &manifest)?;
    write_evidence(&local_paths.evidence, &context, &prepared)?;
    update_tune_bead(BeadUpdate {
        bead_id: &bead_id,
        context: &context,
        plan: &prepared,
        state,
        branch: &branch,
        proposal_head: &proposal_head,
        outcome_counts: &outcome_counts,
        validation: &candidate_validation.rows,
        local_paths: &local_paths,
    })
    .await?;
    Ok(ProposalReport {
        proposal_id: bead_id,
        state,
        envelope,
        repo,
        branch,
        base_commit: context.base_commit,
        proposal_head,
    })
}

fn load_tune_config(path: &Path) -> Result<TuneConfig, TuneError> {
    match fs::read_to_string(path) {
        Ok(raw) => Ok(toml::from_str::<TuneFileConfig>(&raw)?.tune),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(TuneConfig::default()),
        Err(source) => Err(TuneError::ReadConfig {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn load_skill_entries(
    workspace: &Path,
    tracked_files: &BTreeSet<PathBuf>,
    config: &LoomConfig,
) -> Result<Vec<SkillEntry>, TuneError> {
    let mut set = builtin::catalog()?;
    let tracked = tracked_files.iter().cloned().collect::<Vec<_>>();
    let report = load_workspace(workspace, &tracked, &config.skills.paths)?;
    for warning in report.warnings() {
        warn!(
            path = %warning.path.display(),
            message = %warning.message,
            "workspace skill skipped during tune discovery",
        );
    }
    set.extend(report.into_set());
    let registry = SkillRegistry::from_set(set)?;
    let mut entries = registry
        .skills()
        .iter()
        .map(skill_entry)
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.target.cmp(&right.target));
    Ok(entries)
}

fn skill_entry(skill: &NamedSkill) -> SkillEntry {
    SkillEntry {
        target: Target::Skill {
            name: skill.name().clone(),
        },
        description: skill.description().as_str().to_owned(),
        source: skill.source(),
        source_path: skill.provenance().document_path.clone(),
        applicability: applicability(skill),
        markdown: skill.document().markdown().to_owned(),
        tuning_path: skill.provenance().tuning_path.clone(),
    }
}

fn applicability(skill: &NamedSkill) -> String {
    let Some(metadata) = skill
        .frontmatter()
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.loom.as_ref())
    else {
        return "all phases/profiles".to_owned();
    };
    let phases = metadata
        .phases
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let profiles = metadata
        .profiles
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    match (phases.is_empty(), profiles.is_empty()) {
        (true, true) => "all phases/profiles".to_owned(),
        (false, true) => format!("phases={}", phases.join(",")),
        (true, false) => format!("profiles={}", profiles.join(",")),
        (false, false) => format!(
            "phases={} profiles={}",
            phases.join(","),
            profiles.join(",")
        ),
    }
}

fn load_phase_entries(tracked_files: &BTreeSet<PathBuf>) -> Vec<TemplateEntry> {
    let mut entries = tracked_files
        .iter()
        .filter_map(|path| phase_entry(path))
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.target.cmp(&right.target));
    entries
}

fn phase_entry(path: &Path) -> Option<TemplateEntry> {
    let prefix = Path::new("crates/loom-templates/templates");
    let relative = path.strip_prefix(prefix).ok()?;
    if relative.components().count() != 1
        || path.extension().and_then(|ext| ext.to_str()) != Some("md")
    {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let name = PhaseName::new(stem.to_owned()).ok()?;
    Some(TemplateEntry {
        target: Target::Phase { name },
        relative_path: path.to_path_buf(),
    })
}

fn load_partial_entries(tracked_files: &BTreeSet<PathBuf>) -> Vec<TemplateEntry> {
    let mut entries = tracked_files
        .iter()
        .filter_map(|path| partial_entry(path))
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.target.cmp(&right.target));
    entries
}

fn partial_entry(path: &Path) -> Option<TemplateEntry> {
    let prefix = Path::new("crates/loom-templates/templates/partial");
    let relative = path.strip_prefix(prefix).ok()?;
    if relative.components().count() != 1
        || path.extension().and_then(|ext| ext.to_str()) != Some("md")
    {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let name = PartialName::new(stem.to_owned()).ok()?;
    Some(TemplateEntry {
        target: Target::Partial { name },
        relative_path: path.to_path_buf(),
    })
}

fn target_catalog(
    skills: &[SkillEntry],
    phases: &[TemplateEntry],
    partials: &[TemplateEntry],
) -> TargetCatalog {
    TargetCatalog::new(
        skills
            .iter()
            .map(|entry| entry.target.clone())
            .chain(phases.iter().map(|entry| entry.target.clone()))
            .chain(partials.iter().map(|entry| entry.target.clone())),
    )
}

fn load_tuning_cases(
    workspace: &Path,
    tracked_files: &BTreeSet<PathBuf>,
    skills: &[SkillEntry],
    known_targets: &TargetCatalog,
    tune_targets: &[Target],
    registry: &CheckerRegistry,
    disabled: &BTreeSet<loom_tune::CheckerId>,
) -> Result<LoadedTuning, TuneError> {
    let tuning = tuning_documents(workspace, tracked_files, skills, tune_targets)?;
    let cases = load_documents(
        &tuning.documents,
        &LoadContext {
            repo_root: workspace,
            tracked_files,
            targets: known_targets,
            registry,
            disabled_checkers: disabled,
        },
    )
    .map_err(TuneError::CaseLoad)?;
    Ok(LoadedTuning {
        cases,
        guidance: tuning.guidance,
    })
}

struct TuningDocuments {
    documents: Vec<Document>,
    guidance: Vec<TuningDocumentGuidance>,
}

fn tuning_documents(
    workspace: &Path,
    tracked_files: &BTreeSet<PathBuf>,
    skills: &[SkillEntry],
    tune_targets: &[Target],
) -> Result<TuningDocuments, TuneError> {
    let mut documents = Vec::new();
    let mut guidance = Vec::new();
    let repo_tuning = PathBuf::from("docs/tuning.md");
    if tracked_files.contains(&repo_tuning) {
        let path = workspace.join(&repo_tuning);
        let markdown = read_to_string(&path)?;
        guidance.push(TuningDocumentGuidance {
            path: path.clone(),
            lines: tuning_guidance_lines(&markdown),
        });
        documents.push(Document::repo(path, markdown));
    }
    for skill in skills {
        if !tune_targets.contains(&skill.target) {
            continue;
        }
        let Some(path) = &skill.tuning_path else {
            continue;
        };
        let Ok(relative) = path.strip_prefix(workspace) else {
            continue;
        };
        if !tracked_files.contains(relative) {
            continue;
        }
        if let Target::Skill { name } = &skill.target {
            let markdown = read_to_string(path)?;
            guidance.push(TuningDocumentGuidance {
                path: path.clone(),
                lines: tuning_guidance_lines(&markdown),
            });
            documents.push(Document::package(path.clone(), name.clone(), markdown));
        }
    }
    Ok(TuningDocuments {
        documents,
        guidance,
    })
}

fn tuning_guidance_lines(markdown: &str) -> Vec<String> {
    let mut in_case = false;
    let mut lines = Vec::new();
    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```loom-case") {
            in_case = true;
            continue;
        }
        if in_case {
            if trimmed.starts_with("```") {
                in_case = false;
            }
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        lines.push(trimmed.chars().take(160).collect::<String>());
        if lines.len() == 3 {
            break;
        }
    }
    lines
}

fn build_evidence(
    workspace: &Path,
    config: &TuneConfig,
    targets: impl IntoIterator<Item = Target>,
) -> Result<EvidenceSnapshot, TuneError> {
    let splitter = Splitter::new(
        "workspace",
        workspace.display().to_string(),
        config.evidence.selection_fraction,
    )?;
    let checker = loom_tune::CheckerId::new("behavior.review.finding-recall")?;
    let mut items = Vec::new();
    for target in targets {
        if matches!(target, Target::Skill { .. } | Target::Phase { .. }) {
            let id = ItemId::new(format!("{}", target))?;
            items.push(Item::new(id, checker.clone(), vec![target]));
        }
    }
    Ok(splitter.snapshot(items))
}

fn render_skill_listing(skills: &[SkillEntry]) -> String {
    let mut out = String::from("tuneable skills\n");
    if skills.is_empty() {
        out.push_str("(none)\n");
        return out;
    }
    for skill in skills {
        let source = source_name(skill.source);
        let path = skill.source_path.display();
        out.push_str(&format!(
            "- {} | source={source} | applicability={} | path={} | {}\n",
            skill.target, skill.applicability, path, skill.description,
        ));
    }
    out
}

fn render_template_listing(kind: &str, entries: &[TemplateEntry]) -> String {
    let mut out = format!("tuneable {kind} templates\n");
    if entries.is_empty() {
        out.push_str("(none)\n");
        return out;
    }
    for entry in entries {
        out.push_str(&format!(
            "- {} | path={}\n",
            entry.target,
            entry.relative_path.display()
        ));
    }
    out
}

fn render_checker_listing(registry: &CheckerRegistry) -> String {
    let mut out = String::from("registered tune checkers\n");
    for checker in registry.metadata_snapshot() {
        let target_kinds = checker
            .target_kinds
            .iter()
            .map(|kind| format!("{kind:?}").to_lowercase())
            .collect::<Vec<_>>()
            .join(",");
        let levels = checker
            .levels
            .iter()
            .map(|level| level_name(*level))
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&format!(
            "- {} | status={:?} | targets={} | levels={} | cost={:?} | mandatory={} | {}\n",
            checker.id,
            checker.status,
            target_kinds,
            levels,
            checker.cost,
            checker.mandatory,
            checker.summary,
        ));
    }
    out
}

fn render_all_listing(context: &Context) -> String {
    format!(
        "tuneable surfaces\nskills: {}\nphase templates: {}\npartials: {}\ncheckers: {}\n",
        context.skills.len(),
        context.phases.len(),
        context.partials.len(),
        context.checker_registry.metadata_snapshot().len(),
    )
}

fn render_dry_run(context: &Context, plan: &PreparedPlan) -> String {
    let mut out = String::new();
    out.push_str("loom tune dry-run\n");
    out.push_str(&format!("workspace: {}\n", context.workspace.display()));
    out.push_str("loaded tuning docs:\n");
    if plan.loaded_cases.documents().is_empty() {
        out.push_str("- (none)\n");
    } else {
        for document in plan.loaded_cases.documents() {
            out.push_str(&format!(
                "- {} ({} case(s))\n",
                document.path.display(),
                document.case_count
            ));
        }
    }
    out.push_str("evidence roots:\n");
    for line in context.root_report.lines() {
        out.push_str("- ");
        out.push_str(&line);
        out.push('\n');
    }
    out.push_str(&format!("seed: {}\n", plan.frozen.seed));
    out.push_str("case pool:\n");
    out.push_str(&format!("- declared: {}\n", plan.case_counts.declared));
    out.push_str(&format!(
        "- mined train: {}\n",
        plan.case_counts.mined_train
    ));
    out.push_str(&format!(
        "- mined selection: {}\n",
        plan.case_counts.mined_selection
    ));
    out.push_str("selected cases:\n");
    if plan.frozen.selected_cases.is_empty() {
        out.push_str("- (none)\n");
    } else {
        for case in &plan.frozen.selected_cases {
            out.push_str(&format!(
                "- {} via {} ({:?})\n",
                case.case_id, case.checker, case.pool
            ));
        }
    }
    out.push_str("skipped cases:\n");
    if plan.frozen.skipped_cases.is_empty() {
        out.push_str("- (none)\n");
    } else {
        for case in &plan.frozen.skipped_cases {
            out.push_str(&format!(
                "- {} via {} ({:?}: {:?})\n",
                case.case_id, case.checker, case.pool, case.reason
            ));
        }
    }
    out.push_str("frozen checker plan:\n");
    for checker in &plan.frozen.checker_plan {
        out.push_str(&format!("- {checker}\n"));
    }
    out.push_str(&format!("plan hash: {}\n", plan.frozen.plan_hash));
    out.push_str("candidate generation: skipped (dry-run)\n");
    out
}

fn require_non_empty(targets: Vec<Target>, surface: Surface) -> Result<Vec<Target>, TuneError> {
    if targets.is_empty() {
        Err(TuneError::NoTargets { surface })
    } else {
        Ok(targets)
    }
}

fn generated_seed(targets: &[Target], level: Level, base_commit: &str) -> u64 {
    let mut input = String::new();
    input.push_str(base_commit);
    input.push('|');
    input.push_str(level_name(level));
    for target in targets {
        input.push('|');
        input.push_str(&target.to_string());
    }
    let hash = blake3::hash(input.as_bytes());
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&hash.as_bytes()[..8]);
    u64::from_be_bytes(bytes)
}

fn create_envelope_dirs(paths: &LocalPaths) -> Result<(), TuneError> {
    for path in [&paths.logs, &paths.evidence_dir] {
        fs::create_dir_all(path).map_err(|source| TuneError::CreateDir {
            path: path.clone(),
            source,
        })?;
    }
    Ok(())
}

fn local_paths(envelope: &Path) -> LocalPaths {
    LocalPaths {
        repo: envelope.join("repo"),
        manifest: envelope.join("manifest.json"),
        evidence: envelope.join("evidence.md"),
        logs: envelope.join("logs"),
        evidence_dir: envelope.join("evidence"),
    }
}

fn relative_local_paths(paths: &LocalPaths, workspace: &Path) -> LocalPaths {
    LocalPaths {
        repo: relative_or_original(&paths.repo, workspace),
        manifest: relative_or_original(&paths.manifest, workspace),
        evidence: relative_or_original(&paths.evidence, workspace),
        logs: relative_or_original(&paths.logs, workspace),
        evidence_dir: relative_or_original(&paths.evidence_dir, workspace),
    }
}

async fn clone_repo(
    workspace: &Path,
    repo: &Path,
    base: &str,
    branch: &str,
) -> Result<(), TuneError> {
    GitClient::open(workspace)?
        .create_tune_checkout(repo, base, branch)
        .await?;
    Ok(())
}

fn write_candidate_files(
    context: &Context,
    proposal: &ProposeRequest,
    plan: &PreparedPlan,
    repo: &Path,
) -> Result<Vec<PathBuf>, TuneError> {
    let mut touched = Vec::new();
    for target in &plan.targets {
        let path = context.candidate_path(target, repo)?;
        if let Target::Skill { .. } = target
            && let Some(entry) = context.skills.iter().find(|entry| &entry.target == target)
            && entry.source == SkillSource::BuiltIn
            && !entry.source_path.starts_with(&context.workspace)
        {
            write_parented(&path, &entry.markdown)?;
        }
        append_candidate_note(&path, proposal.level, target, context, plan)?;
        touched.push(path);
    }
    touched.sort();
    touched.dedup();
    Ok(touched)
}

fn append_candidate_note(
    path: &Path,
    level: Level,
    target: &Target,
    context: &Context,
    plan: &PreparedPlan,
) -> Result<(), TuneError> {
    let mut body = fs::read_to_string(path).map_err(|source| TuneError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str("\n## Tuning Candidate Notes\n\n");
    body.push_str(&candidate_note(level, target, context, plan));
    write_parented(path, &body)
}

fn candidate_note(level: Level, target: &Target, context: &Context, plan: &PreparedPlan) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "- Candidate level: `{}` for `{}`.\n",
        level_name(level),
        target
    ));
    out.push_str(
        "- Edit budget: one bounded guidance section derived from loaded tuning context.\n",
    );
    out.push_str("- Evidence roots considered:\n");
    for line in context.root_report.lines() {
        out.push_str(&format!("  - {line}\n"));
    }
    out.push_str("- Tuning guidance considered:\n");
    if plan.tuning_guidance.is_empty() {
        out.push_str("  - (none)\n");
    } else {
        for guidance in &plan.tuning_guidance {
            let path = relative_or_original(&guidance.path, &context.workspace);
            let summary = if guidance.lines.is_empty() {
                "case declarations only".to_owned()
            } else {
                guidance.lines.join(" / ")
            };
            out.push_str(&format!("  - {}: {summary}\n", path.display()));
        }
    }
    out.push_str("- Selected behavioral cases for this target:\n");
    let case_lines = selected_case_guidance(plan, target);
    if case_lines.is_empty() {
        out.push_str("  - (none)\n");
    } else {
        for line in case_lines {
            out.push_str(&format!("  - {line}\n"));
        }
    }
    out
}

fn selected_case_guidance(plan: &PreparedPlan, target: &Target) -> Vec<String> {
    let mut lines = Vec::new();
    for selected in &plan.frozen.selected_cases {
        match &selected.case_id {
            loom_tune::plan::PlannedCaseId::Declared(id) => {
                let Some(case) = plan.loaded_cases.cases().iter().find(|case| &case.id == id)
                else {
                    continue;
                };
                if !target.intersects_any(&case.targets) {
                    continue;
                }
                let terms = tune_executor::expected_terms(&case.expected);
                lines.push(format!(
                    "{} via {}: {}",
                    selected.case_id,
                    selected.checker,
                    format_terms(&terms)
                ));
            }
            loom_tune::plan::PlannedCaseId::Mined(_) => lines.push(format!(
                "{} via {}: mined selection evidence",
                selected.case_id, selected.checker
            )),
        }
    }
    lines
}

fn format_terms(terms: &[String]) -> String {
    if terms.is_empty() {
        "no explicit terms".to_owned()
    } else {
        terms.join("; ")
    }
}

#[derive(Debug, Clone)]
struct CandidateValidation {
    rows: Vec<ValidationRow>,
    outcome_counts: OutcomeCounts,
}

fn target_artifacts(
    context: &Context,
    targets: &[Target],
    repo: &Path,
) -> Result<Vec<TuneArtifact>, TuneError> {
    targets
        .iter()
        .map(|target| {
            let candidate_path = context.candidate_path(target, repo)?;
            Ok(TuneArtifact::new(
                target.clone(),
                current_artifact_text(context, target)?,
                read_to_string(&candidate_path)?,
            ))
        })
        .collect()
}

fn current_artifact_text(context: &Context, target: &Target) -> Result<String, TuneError> {
    match target {
        Target::Skill { .. } => {
            let entry = context
                .skills
                .iter()
                .find(|entry| &entry.target == target)
                .ok_or_else(|| TuneError::UnknownTarget {
                    target: target.clone(),
                })?;
            Ok(entry.markdown.clone())
        }
        Target::Phase { .. } => current_template_text(context, target, &context.phases),
        Target::Partial { .. } => current_template_text(context, target, &context.partials),
    }
}

fn current_template_text(
    context: &Context,
    target: &Target,
    entries: &[TemplateEntry],
) -> Result<String, TuneError> {
    let entry = entries
        .iter()
        .find(|entry| &entry.target == target)
        .ok_or_else(|| TuneError::UnknownTarget {
            target: target.clone(),
        })?;
    read_to_string(&context.workspace.join(&entry.relative_path))
}

fn validate_candidate(
    repo: &Path,
    plan: &FrozenPlan,
    loaded_cases: &LoadedCases,
    registry: &CheckerRegistry,
    targets: &[Target],
    touched: &[PathBuf],
    artifacts: &[TuneArtifact],
) -> Result<CandidateValidation, TuneError> {
    let mut rows = vec![ValidationRow {
        check: "candidate-files".to_owned(),
        status: ValidationStatus::Passed,
        detail: format!("{} target file(s) updated", touched.len()),
    }];
    if targets
        .iter()
        .any(|target| matches!(target, Target::Phase { .. } | Target::Partial { .. }))
    {
        rows.extend(validate_templates(repo));
    }
    let behavior = validate_behavioral_cases(plan, loaded_cases, registry, artifacts);
    rows.extend(behavior.rows);
    Ok(CandidateValidation {
        rows,
        outcome_counts: behavior.outcome_counts,
    })
}

fn validate_behavioral_cases(
    plan: &FrozenPlan,
    loaded_cases: &LoadedCases,
    registry: &CheckerRegistry,
    artifacts: &[TuneArtifact],
) -> CandidateValidation {
    if plan.selected_cases.is_empty() {
        return CandidateValidation {
            rows: Vec::new(),
            outcome_counts: OutcomeCounts::pending(0),
        };
    }
    let results = match tune_executor::run(plan, loaded_cases, artifacts) {
        Ok(results) => results,
        Err(source) => {
            return CandidateValidation {
                rows: vec![ValidationRow {
                    check: "behavioral-cases".to_owned(),
                    status: ValidationStatus::Failed,
                    detail: format!("checker execution failed: {source}"),
                }],
                outcome_counts: OutcomeCounts {
                    pending: 0,
                    passed: 0,
                    failed: 0,
                    blocked: plan.selected_cases.len(),
                },
            };
        }
    };
    match tune_gate::evaluate(plan, results, registry) {
        Ok(report) => CandidateValidation {
            rows: vec![ValidationRow {
                check: "behavioral-cases".to_owned(),
                status: gate_validation_status(report.state),
                detail: format!(
                    "{} selected behavioral case(s) evaluated",
                    report.cases.len()
                ),
            }],
            outcome_counts: outcome_counts_from_gate(&report),
        },
        Err(source) => CandidateValidation {
            rows: vec![ValidationRow {
                check: "behavioral-cases".to_owned(),
                status: ValidationStatus::Failed,
                detail: source.to_string(),
            }],
            outcome_counts: OutcomeCounts {
                pending: 0,
                passed: 0,
                failed: 0,
                blocked: plan.selected_cases.len(),
            },
        },
    }
}

fn gate_validation_status(state: GateState) -> ValidationStatus {
    match state {
        GateState::Passed => ValidationStatus::Passed,
        GateState::Blocked => ValidationStatus::Failed,
    }
}

fn outcome_counts_from_gate(report: &tune_gate::Report) -> OutcomeCounts {
    let mut counts = OutcomeCounts {
        pending: 0,
        passed: 0,
        failed: 0,
        blocked: 0,
    };
    for case in &report.cases {
        match case.outcome {
            GateOutcome::Improved | GateOutcome::StableSuccess => counts.passed += 1,
            GateOutcome::Regressed | GateOutcome::PersistentFail => counts.failed += 1,
        }
    }
    counts
}

fn validate_templates(repo: &Path) -> Vec<ValidationRow> {
    let commands = [
        (
            "askama-compile",
            vec!["check", "-p", "loom-templates", "--quiet"],
        ),
        (
            "representative-renders",
            vec![
                "test",
                "-p",
                "loom-templates",
                "--test",
                "render",
                "template_renders_are_byte_stable_across_runs",
                "--quiet",
            ],
        ),
        (
            "template-conformance",
            vec![
                "run",
                "-p",
                "loom-walk",
                "--quiet",
                "--",
                "template_pinning_matrix",
            ],
        ),
    ];
    if !repo.join("crates/loom-templates/Cargo.toml").exists() {
        return commands
            .iter()
            .map(|(name, _)| ValidationRow {
                check: (*name).to_owned(),
                status: ValidationStatus::Skipped,
                detail: "loom-templates crate is not present in this proposal repo".to_owned(),
            })
            .collect();
    }
    commands
        .iter()
        .map(|(name, args)| validation_command(repo, name, args))
        .collect()
}

fn validation_command(repo: &Path, name: &str, args: &[&str]) -> ValidationRow {
    match Command::new("cargo").args(args).current_dir(repo).output() {
        Ok(output) if output.status.success() => ValidationRow {
            check: name.to_owned(),
            status: ValidationStatus::Passed,
            detail: "command passed".to_owned(),
        },
        Ok(output) => ValidationRow {
            check: name.to_owned(),
            status: ValidationStatus::Failed,
            detail: command_failure_detail(&output),
        },
        Err(source) => ValidationRow {
            check: name.to_owned(),
            status: ValidationStatus::Failed,
            detail: source.to_string(),
        },
    }
}

fn command_failure_detail(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    if detail.is_empty() {
        format!("command exited with {}", output.status)
    } else {
        detail.chars().take(500).collect()
    }
}

fn proposal_state(validation: &[ValidationRow]) -> State {
    if validation
        .iter()
        .any(|row| row.status == ValidationStatus::Failed)
    {
        State::Blocked
    } else {
        State::Pending
    }
}

async fn commit_candidate(
    repo: &Path,
    surface: Surface,
    level: Level,
    bead_id: &BeadId,
) -> Result<(), TuneError> {
    let message = format!(
        "Tune {surface} targets for {bead_id} at {} level",
        level_name(level)
    );
    GitClient::open(repo)?
        .commit_all_allow_empty(&message)
        .await?;
    Ok(())
}

fn write_manifest(path: &Path, manifest: &ProposalManifest) -> Result<(), TuneError> {
    let body = serde_json::to_string_pretty(manifest)?;
    write_parented(path, &(body + "\n"))
}

fn write_evidence(path: &Path, context: &Context, plan: &PreparedPlan) -> Result<(), TuneError> {
    let mut body = String::new();
    body.push_str("# Tune Evidence Appendix\n\n");
    body.push_str("## Evidence roots\n\n");
    for line in context.root_report.lines() {
        body.push_str("- ");
        body.push_str(&line);
        body.push('\n');
    }
    body.push_str("\n## Loaded tuning docs\n\n");
    if plan.loaded_cases.documents().is_empty() {
        body.push_str("- (none)\n");
    } else {
        for document in plan.loaded_cases.documents() {
            body.push_str(&format!(
                "- {} ({} case(s))\n",
                document.path.display(),
                document.case_count
            ));
        }
    }
    body.push_str("\n## Frozen checker plan\n\n");
    body.push_str(&format!("- seed: {}\n", plan.frozen.seed));
    body.push_str(&format!("- plan hash: {}\n", plan.frozen.plan_hash));
    for checker in &plan.frozen.checker_plan {
        body.push_str(&format!("- {checker}\n"));
    }
    write_parented(path, &body)
}

struct BeadUpdate<'a> {
    bead_id: &'a BeadId,
    context: &'a Context,
    plan: &'a PreparedPlan,
    state: State,
    branch: &'a str,
    proposal_head: &'a str,
    outcome_counts: &'a OutcomeCounts,
    validation: &'a [ValidationRow],
    local_paths: &'a LocalPaths,
}

async fn update_tune_bead(update: BeadUpdate<'_>) -> Result<(), TuneError> {
    let body = bead_body(&update);
    let metadata = tune_metadata(&update)?;
    BdClient::new()
        .update(
            update.bead_id,
            UpdateOpts {
                status: Some(update.state.bead_status().to_owned()),
                description: Some(body),
                set_metadata: metadata,
                ..UpdateOpts::default()
            },
        )
        .await?;
    Ok(())
}

fn bead_body(update: &BeadUpdate<'_>) -> String {
    let mut body = String::new();
    body.push_str(&format!("# Tune proposal {}\n\n", update.bead_id));
    body.push_str(&format!("State: `{}`\n\n", state_name(update.state)));
    body.push_str("## Targets\n\n");
    for target in &update.plan.targets {
        body.push_str(&format!("- `{target}`\n"));
    }
    body.push_str("\n## Proposal\n\n");
    body.push_str(&format!(
        "- Level: `{}`\n",
        level_name(update.plan.frozen.level)
    ));
    body.push_str(&format!("- Seed: `{}`\n", update.plan.frozen.seed));
    body.push_str(&format!(
        "- Base commit: `{}`\n",
        update.context.base_commit
    ));
    body.push_str(&format!("- Proposal branch: `{}`\n", update.branch));
    body.push_str(&format!("- Proposal head: `{}`\n", update.proposal_head));
    body.push_str(&format!(
        "- Checker plan hash: `{}`\n",
        update.plan.frozen.plan_hash
    ));
    body.push_str("\n## Case counts\n\n");
    body.push_str(&format!(
        "- Declared: {}\n",
        update.plan.case_counts.declared
    ));
    body.push_str(&format!(
        "- Mined train: {}\n",
        update.plan.case_counts.mined_train
    ));
    body.push_str(&format!(
        "- Mined selection: {}\n",
        update.plan.case_counts.mined_selection
    ));
    body.push_str(&format!(
        "- Selected: {}\n",
        update.plan.case_counts.selected
    ));
    body.push_str(&format!("- Skipped: {}\n", update.plan.case_counts.skipped));
    body.push_str("\n## Outcome counts\n\n");
    body.push_str(&format!("- Pending: {}\n", update.outcome_counts.pending));
    body.push_str(&format!("- Passed: {}\n", update.outcome_counts.passed));
    body.push_str(&format!("- Failed: {}\n", update.outcome_counts.failed));
    body.push_str(&format!("- Blocked: {}\n", update.outcome_counts.blocked));
    body.push_str("\n## Summary\n\n");
    body.push_str("- Candidate edits are isolated in the local proposal repo.\n");
    body.push_str("- No push was performed by `loom tune`.\n");
    body.push_str("\n## Validation\n\n");
    body.push_str("| Check | Status | Detail |\n|---|---|---|\n");
    for row in update.validation {
        body.push_str(&format!(
            "| {} | {:?} | {} |\n",
            row.check,
            row.status,
            row.detail.replace('|', "\\|")
        ));
    }
    body.push_str("\n## Risks\n\n");
    body.push_str(
        "- Human review must confirm the generated candidate improves the targeted artifact.\n",
    );
    body.push_str("- Template proposals must keep compiled phase protocol intact.\n");
    body.push_str("\n## Inbox context\n\n");
    body.push_str(&format!(
        "- View with `loom inbox view -p {}`.\n",
        update.bead_id
    ));
    body.push_str(&format!(
        "- Proposal repo: `{}`\n",
        update.local_paths.repo.display()
    ));
    body.push_str(&format!(
        "- Manifest: `{}`\n",
        update.local_paths.manifest.display()
    ));
    body.push_str(&format!(
        "- Evidence appendix: `{}`\n",
        update.local_paths.evidence.display()
    ));
    body
}

fn tune_metadata(update: &BeadUpdate<'_>) -> Result<Vec<(String, String)>, TuneError> {
    let specs = specs_for_targets(&update.plan.targets);
    Ok(vec![
        (
            "loom.tune.id".to_owned(),
            update.bead_id.as_str().to_owned(),
        ),
        (
            "loom.tune.state".to_owned(),
            state_name(update.state).to_owned(),
        ),
        (
            "loom.tune.targets".to_owned(),
            serde_json::to_string(&update.plan.targets)?,
        ),
        (
            "loom.tune.level".to_owned(),
            level_name(update.plan.frozen.level).to_owned(),
        ),
        (
            "loom.tune.seed".to_owned(),
            update.plan.frozen.seed.to_string(),
        ),
        (
            "loom.tune.base_commit".to_owned(),
            update.context.base_commit.to_owned(),
        ),
        (
            "loom.tune.proposal_branch".to_owned(),
            update.branch.to_owned(),
        ),
        (
            "loom.tune.proposal_head".to_owned(),
            update.proposal_head.to_owned(),
        ),
        (
            "loom.tune.plan_hash".to_owned(),
            update.plan.frozen.plan_hash.to_string(),
        ),
        (
            "loom.tune.case_counts".to_owned(),
            serde_json::to_string(&update.plan.case_counts)?,
        ),
        (
            "loom.tune.outcome_counts".to_owned(),
            serde_json::to_string(update.outcome_counts)?,
        ),
        (
            "loom.tune.summary".to_owned(),
            "isolated tune proposal".to_owned(),
        ),
        ("loom.tune.specs".to_owned(), serde_json::to_string(&specs)?),
        (
            "loom.tune.evidence_roots".to_owned(),
            serde_json::to_string(update.context.root_report.roots())?,
        ),
    ])
}

fn labels_for_targets(targets: &[Target]) -> Vec<String> {
    let mut labels = vec!["loom:tune".to_owned()];
    for spec in specs_for_targets(targets) {
        labels.push(format!("spec:{spec}"));
    }
    labels.sort();
    labels.dedup();
    labels
}

fn specs_for_targets(targets: &[Target]) -> Vec<&'static str> {
    let mut specs = Vec::new();
    if targets
        .iter()
        .any(|target| matches!(target, Target::Skill { .. }))
    {
        specs.push("skills");
    }
    if targets
        .iter()
        .any(|target| matches!(target, Target::Phase { .. } | Target::Partial { .. }))
    {
        specs.push("templates");
    }
    specs
}

fn source_name(source: SkillSource) -> &'static str {
    match source {
        SkillSource::BuiltIn => "built_in",
        SkillSource::Workspace => "workspace",
        SkillSource::Configured => "configured",
        SkillSource::Override => "override",
    }
}

fn state_name(state: State) -> &'static str {
    match state {
        State::Pending => "pending",
        State::Blocked => "blocked",
        State::Accepted => "accepted",
        State::Applied => "applied",
        State::Rejected => "rejected",
        State::ApplyFailed => "apply_failed",
    }
}

fn level_name(level: Level) -> &'static str {
    match level {
        Level::Fast => "fast",
        Level::Run => "run",
        Level::Full => "full",
    }
}

fn write_parented(path: &Path, body: &str) -> Result<(), TuneError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| TuneError::CreateDir {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::write(path, body).map_err(|source| TuneError::WriteFile {
        path: path.to_path_buf(),
        source,
    })
}

fn read_to_string(path: &Path) -> Result<String, TuneError> {
    fs::read_to_string(path).map_err(|source| TuneError::ReadFile {
        path: path.to_path_buf(),
        source,
    })
}

fn relative_or_original(path: &Path, root: &Path) -> PathBuf {
    match path.strip_prefix(root) {
        Ok(relative) => relative.to_path_buf(),
        Err(_) => path.to_path_buf(),
    }
}

/// Tune workflow failures.
#[derive(Debug, Display, Error)]
pub enum TuneError {
    /// failed to canonicalize workspace path {path}
    CanonicalizeWorkspace {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// failed to read tune config {path}
    ReadConfig {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// invalid tune config
    TuneConfig(#[from] toml::de::Error),
    /// loom config error
    LoomConfig(#[from] LoomConfigError),
    /// git error
    Git(#[from] GitError),
    /// built-in skill catalog error
    BuiltinCatalog(#[from] CatalogError),
    /// skill discovery error
    SkillDiscovery(#[from] DiscoveryError),
    /// skill registry error
    SkillRegistry(#[from] RegistryError),
    /// checker registry error
    CheckerRegistry(#[from] CheckerRegistryError),
    /// tuning case load error
    CaseLoad(#[from] LoadError),
    /// checker planning error
    Plan(#[from] PlanError),
    /// evidence split error
    Split(#[from] SplitError),
    /// evidence item id error
    EvidenceItemId(#[from] loom_tune::evidence::ParseItemIdError),
    /// checker id error
    CheckerId(#[from] loom_tune::checker::ParseCheckerIdError),
    /// invalid skill target name `{name}`
    SkillName {
        name: String,
        #[source]
        source: loom_skills::identity::ParseSkillNameError,
    },
    /// invalid phase target name `{name}`
    PhaseName {
        name: String,
        #[source]
        source: loom_skills::identity::ParsePhaseNameError,
    },
    /// invalid partial target name `{name}`
    PartialName {
        name: String,
        #[source]
        source: loom_tune::target::ParsePartialNameError,
    },
    /// tune target `{target}` is not known
    UnknownTarget { target: Target },
    /// no tune targets are available for `{surface}`
    NoTargets { surface: Surface },
    /// `loom tune all` does not accept target names
    AllRejectsTargets,
    /// target path {path} is outside the workspace
    TargetOutsideWorkspace {
        path: PathBuf,
        #[source]
        source: std::path::StripPrefixError,
    },
    /// failed to create directory at {path}
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// failed to read file {path}
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// failed to write file {path}
    WriteFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// failed to serialize tune artifact
    Serialize(#[from] serde_json::Error),
    /// tune lock error
    Lock(#[from] LockError),
    /// bd error
    Bd(#[from] loom_driver::bd::BdError),
}

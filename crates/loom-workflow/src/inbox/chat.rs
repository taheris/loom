//! `loom inbox chat` — interactive human-decision session.
//!
//! Mirrors `loom plan`'s runner shape: the driver renders the `inbox.md`
//! template against the visible inbox queue, builds the same `wrix run
//! <workspace> <agent command> ... <prompt>` argv plan uses, and shells out
//! with inherited stdio so the configured agent attaches to the user's terminal
//! as a real REPL.

use std::path::{Path, PathBuf};
use std::process::Command;

use askama::Template;
use loom_driver::agent::AgentKind;
use loom_driver::bd::{BdClient, ListOpts};
use loom_driver::config::{LoomConfig, Phase};
use loom_driver::git::{GitClient, GitError};
use loom_driver::identifier::{BeadId, ProfileName, SpecLabel};
use loom_driver::profile_manifest::{ImageEntry, ProfileError, ProfileImageManifest};
use loom_driver::scratch::{ScratchSession, resolve_scratch_key};
use loom_driver::state::CacheDb;
use thiserror::Error;
use tracing::info;

use crate::skill::{SkillError, SkillPlan};

use super::context::build_inbox_context;
use super::list::{
    InboxItem, InboxKind, build_queue, find_by_bead_id, find_by_index, find_by_proposal_id,
};

/// Default name of the wrix launcher binary on PATH.
pub const WRIX_BIN: &str = "wrix";

/// Env vars `wrix run` reads to pick the per-profile image when no
/// `--spawn-config` is supplied.
pub const WRIX_DEFAULT_IMAGE_REF: &str = "WRIX_DEFAULT_IMAGE_REF";
pub const WRIX_DEFAULT_IMAGE_SOURCE: &str = "WRIX_DEFAULT_IMAGE_SOURCE";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatTarget {
    Index(u32),
    Bead(String),
    Proposal(String),
}

/// Inputs to one [`run`] call.
#[derive(Debug)]
pub struct ChatOpts {
    /// Optional `-s <label>` filter.
    pub spec_filter: Option<SpecLabel>,
    /// Optional `-k <kind>` filter.
    pub kind_filter: Option<InboxKind>,
    /// Optional target. Absent means the session walks the visible queue.
    pub target: Option<ChatTarget>,
    /// Optional profile override.
    pub cli_profile: Option<ProfileName>,
    /// CLI `--agent` override.
    pub agent_override: Option<AgentKind>,
    /// Resolved profile-image manifest.
    pub manifest: ProfileImageManifest,
    /// Explicit path to the `wrix` launcher.
    pub wrix_bin: Option<PathBuf>,
}

/// Outcome of one `loom inbox chat` session.
#[derive(Debug, Clone)]
pub struct ChatReport {
    /// Number of inbox items surfaced into the rendered prompt at session start.
    pub items_surfaced: usize,
    /// Number of inbox items still visible after the session exited.
    pub items_remaining: usize,
}

#[derive(Debug, Error)]
pub enum ChatError {
    #[error("profile resolution failed")]
    Profile(#[from] ProfileError),
    #[error("config load failed: {0}")]
    Config(String),
    #[error("bd list failed: {0}")]
    BdList(String),
    #[error("render inbox.md template: {0}")]
    Render(String),
    #[error("cache db operation failed while running `loom inbox chat`")]
    State(#[from] loom_driver::state::CacheError),
    #[error("scratch session io failed")]
    Scratch(#[from] std::io::Error),
    #[error("wrix exited with status {status}")]
    WrixExit { status: String },
    #[error("agent selection: {0}")]
    AgentSelection(String),
    #[error("git step failed while running `loom inbox chat`")]
    Git(#[from] GitError),
    #[error("skill resolution failed while running `loom inbox chat`")]
    Skill(#[from] SkillError),
    #[error("no inbox item at index {index} ({total} outstanding)")]
    IndexOutOfRange { index: u32, total: u32 },
    #[error("no inbox item with bead id {id}")]
    BeadNotFound { id: String },
    #[error("no tune proposal with id {id}")]
    ProposalNotFound { id: String },
}

/// Run one `loom inbox chat` session against `workspace`.
pub fn run(workspace: &Path, opts: ChatOpts) -> Result<ChatReport, ChatError> {
    let cfg = LoomConfig::load(LoomConfig::resolve_path(workspace))
        .map_err(|e| ChatError::Config(e.to_string()))?;
    let (profile, agent_kind) =
        resolve_chat_selection(opts.cli_profile.as_ref(), opts.agent_override, &cfg)?;
    let image: &ImageEntry = opts.manifest.lookup(&profile, agent_kind)?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ChatError::Config(format!("tokio runtime: {e}")))?;
    let beads = runtime
        .block_on(async {
            let bd = BdClient::new();
            bd.list(ListOpts::default()).await
        })
        .map_err(|e| ChatError::BdList(e.to_string()))?;
    let queue = build_queue(&beads, opts.spec_filter.as_ref(), opts.kind_filter, false);
    let visible = select_visible(queue, opts.target.as_ref())?;
    let items_surfaced = visible.len();
    if visible.is_empty() {
        return Ok(ChatReport {
            items_surfaced: 0,
            items_remaining: 0,
        });
    }

    let scope_label = opts
        .spec_filter
        .clone()
        .or_else(|| visible.iter().find_map(|item| item.spec.clone()))
        .unwrap_or_else(|| SpecLabel::new("inbox-chat"));
    let key = resolve_scratch_key(
        Phase::Inbox,
        std::slice::from_ref(&scope_label),
        single_bead(&visible),
    );
    let scratchpad_path = ScratchSession::scratchpad_path_for(workspace, &key);
    let scratch_dir = scratchpad_path.parent().ok_or_else(|| {
        ChatError::Scratch(std::io::Error::other("scratchpad path has no parent"))
    })?;
    let tracked_files = if cfg.skills.paths.is_empty() {
        Vec::new()
    } else {
        let git = GitClient::open(workspace)?;
        git.tracked_files_sync()?
    };
    let skill_plan = SkillPlan::resolve(
        workspace,
        &tracked_files,
        Phase::Inbox.as_str(),
        &profile,
        agent_kind,
        &cfg.skills,
    )?;
    let skill_session = skill_plan.materialize(scratch_dir, workspace)?;
    let companion_paths = load_companion_paths(workspace, opts.spec_filter.as_ref(), &visible)?;
    let ctx = build_inbox_context(
        workspace,
        String::new(),
        companion_paths,
        &visible,
        scratchpad_path.to_string_lossy().into_owned(),
        skill_session.skill_index,
    );
    let prompt_body = ctx.render().map_err(|e| ChatError::Render(e.to_string()))?;

    let scratch = ScratchSession::open(workspace, &key, &prompt_body, "loom inbox chat")?;
    let _restored_skills = skill_plan.materialize(scratch.path(), workspace)?;
    let argv = build_wrix_argv(workspace, &prompt_body, agent_kind);
    let bin: PathBuf = opts
        .wrix_bin
        .or_else(|| std::env::var_os("LOOM_WRIX_BIN").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(WRIX_BIN));

    info!(
        wrix_bin = %bin.display(),
        items_surfaced,
        profile = %profile,
        agent = ?agent_kind,
        image_ref = %image.r#ref,
        scratch_dir = %scratch.path().display(),
        "loom inbox chat: shelling out to interactive wrix run",
    );

    let status = Command::new(&bin)
        .args(&argv)
        .env(WRIX_DEFAULT_IMAGE_REF, &image.r#ref)
        .env(WRIX_DEFAULT_IMAGE_SOURCE, &image.source)
        .env("WRIX_AGENT", agent_kind.as_str())
        .status()
        .map_err(ChatError::Scratch)?;
    drop(scratch);

    if !status.success() {
        return Err(ChatError::WrixExit {
            status: status.to_string(),
        });
    }

    let beads_after = runtime
        .block_on(async {
            let bd = BdClient::new();
            bd.list(ListOpts::default()).await
        })
        .map_err(|e| ChatError::BdList(e.to_string()))?;
    let remaining = build_queue(
        &beads_after,
        opts.spec_filter.as_ref(),
        opts.kind_filter,
        false,
    );
    Ok(ChatReport {
        items_surfaced,
        items_remaining: remaining.len(),
    })
}

pub fn build_wrix_argv(workspace: &Path, prompt_body: &str, agent_kind: AgentKind) -> Vec<String> {
    let mut argv = vec![
        "run".to_string(),
        workspace.to_string_lossy().into_owned(),
        agent_command(agent_kind).to_string(),
    ];
    if matches!(agent_kind, AgentKind::Claude) {
        argv.push("--dangerously-skip-permissions".to_string());
    }
    argv.push(prompt_body.to_string());
    argv
}

fn agent_command(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Claude => "claude",
        AgentKind::Pi => "pi",
        AgentKind::Direct => "loom-direct-runner",
    }
}

fn select_visible(
    queue: Vec<InboxItem>,
    target: Option<&ChatTarget>,
) -> Result<Vec<InboxItem>, ChatError> {
    let Some(target) = target else {
        return Ok(queue);
    };
    let total = u32::try_from(queue.len()).unwrap_or(u32::MAX);
    let item = match target {
        ChatTarget::Index(index) => {
            find_by_index(&queue, *index).ok_or(ChatError::IndexOutOfRange {
                index: *index,
                total,
            })?
        }
        ChatTarget::Bead(id) => {
            find_by_bead_id(&queue, id).ok_or_else(|| ChatError::BeadNotFound { id: id.clone() })?
        }
        ChatTarget::Proposal(id) => find_by_proposal_id(&queue, id)
            .ok_or_else(|| ChatError::ProposalNotFound { id: id.clone() })?,
    };
    Ok(vec![item.clone()])
}

fn single_bead(items: &[InboxItem]) -> Option<&BeadId> {
    if items.len() == 1 {
        Some(&items[0].bead.id)
    } else {
        None
    }
}

fn load_companion_paths(
    workspace: &Path,
    spec_filter: Option<&SpecLabel>,
    items: &[InboxItem],
) -> Result<Vec<String>, ChatError> {
    let db = CacheDb::open(workspace.join(".loom/cache.db"))?;
    let mut labels: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Some(label) = spec_filter {
        labels.insert(label.as_str().to_string());
    } else {
        for item in items {
            if let Some(label) = &item.spec {
                labels.insert(label.as_str().to_string());
            }
        }
    }
    let mut paths: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for label in &labels {
        let spec_label = SpecLabel::new(label);
        for path in db.companions(&spec_label)? {
            paths.insert(path);
        }
    }
    Ok(paths.into_iter().collect())
}

fn resolve_chat_selection(
    cli_profile: Option<&ProfileName>,
    agent_override: Option<AgentKind>,
    config: &LoomConfig,
) -> Result<(ProfileName, AgentKind), ChatError> {
    let mut selection = config
        .agent_for(Phase::Inbox)
        .map_err(|e| ChatError::AgentSelection(e.to_string()))?;
    if let Some(p) = cli_profile {
        selection.profile = p.clone();
    }
    if let Some(kind) = agent_override {
        selection.kind = kind;
    }
    if matches!(selection.kind, AgentKind::Direct) {
        return Err(ChatError::AgentSelection(
            "direct backend cannot run interactive `loom inbox chat`".to_string(),
        ));
    }
    Ok((selection.profile, selection.kind))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn argv_starts_with_wrix_run_and_workspace() {
        let argv = build_wrix_argv(&PathBuf::from("/work"), "PROMPT", AgentKind::Claude);
        assert_eq!(argv[0], "run");
        assert_eq!(argv[1], "/work");
        assert_eq!(argv[2], "claude");
    }

    #[test]
    fn argv_passes_prompt_to_claude_with_skip_permissions() {
        let argv = build_wrix_argv(&PathBuf::from("/work"), "PROMPT BODY", AgentKind::Claude);
        assert_eq!(argv[2], "claude");
        assert_eq!(argv[3], "--dangerously-skip-permissions");
        assert_eq!(argv[4], "PROMPT BODY");
    }

    #[test]
    fn argv_passes_prompt_to_pi_without_claude_flags() {
        let argv = build_wrix_argv(&PathBuf::from("/work"), "PROMPT BODY", AgentKind::Pi);
        assert_eq!(argv[2], "pi");
        assert_eq!(argv[3], "PROMPT BODY");
        assert!(!argv.iter().any(|a| a == "--dangerously-skip-permissions"));
    }

    #[test]
    fn argv_never_contains_profile_spawn_or_stdio_or_spawn_config() {
        let argv = build_wrix_argv(&PathBuf::from("/work"), "PROMPT", AgentKind::Claude);
        assert!(!argv.iter().any(|a| a == "--profile"));
        assert!(!argv.iter().any(|a| a == "spawn"));
        assert!(!argv.iter().any(|a| a == "--stdio"));
        assert!(!argv.iter().any(|a| a == "--spawn-config"));
    }
}

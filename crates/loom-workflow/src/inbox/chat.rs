//! `loom inbox chat` — interactive human-decision session.
//!
//! Mirrors `loom plan`'s Claude runner shape for Claude-backed chat, and
//! uses a controlled Pi RPC bridge for Pi-backed chat so compaction re-pins
//! are observable before post-compaction output is trusted.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;

use askama::Template;
use displaydoc::Display;
use loom_agent::PiBackend;
use loom_driver::agent::{
    Active, AgentBackend, AgentEvent, AgentKind, AgentSession, ModelSelection, ProtocolError,
    SpawnConfig,
};
use loom_driver::bd::{BdClient, ListOpts};
use loom_driver::clock::{Clock, SystemClock};
use loom_driver::config::{AgentSelection, LoomConfig, Phase};
use loom_driver::git::GitError;
use loom_driver::identifier::{BeadId, ProfileName, SpecLabel};
use loom_driver::profile_manifest::{ImageEntry, ProfileError, ProfileImageManifest};
use loom_driver::scratch::{ScratchSession, resolve_scratch_key};
use loom_driver::state::CacheDb;
use loom_events::{EnvelopeBuilder, Source};
use thiserror::Error;
use tracing::info;

use crate::r#loop::{dolt_socket_mount, sccache_mount};
use crate::skill::{SkillError, SkillPlan};
use crate::spawn::{container_workspace_path, launcher_key_env_for_checkout};

use super::context::build_inbox_context;
use super::list::{
    InboxItem, InboxKind, build_queue, find_by_bead_id, find_by_index, find_by_proposal_id,
};
use super::terminal::{TerminalMarker, TerminalMarkerError, parse as parse_terminal_marker};
use super::{ApplyError, apply_proposals, ensure_integration_clean_after_chat};

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
    /// Number of tune proposals applied by a trusted driver handoff.
    pub applied_proposals: usize,
}

#[derive(Debug, Display, Error)]
pub enum ChatError {
    /// profile resolution failed
    Profile(#[from] ProfileError),
    /// config load failed: {0}
    Config(String),
    /// bd list failed: {0}
    BdList(String),
    /// render inbox.md template: {0}
    Render(String),
    /// cache db operation failed while running `loom inbox chat`
    State(#[from] loom_driver::state::CacheError),
    /// scratch session io failed
    Scratch(#[from] std::io::Error),
    /// wrix exited with status {status}
    WrixExit { status: String },
    /// agent backend protocol failure during `loom inbox chat`
    Protocol(#[from] ProtocolError),
    /// agent selection: {0}
    AgentSelection(String),
    /// git step failed while running `loom inbox chat`
    Git(#[from] GitError),
    /// skill resolution failed while running `loom inbox chat`
    Skill(#[from] SkillError),
    /// inbox terminal marker error
    Terminal(#[from] TerminalMarkerError),
    /// tune proposal apply failed
    Apply(#[from] ApplyError),
    /// no inbox item at index {index} ({total} outstanding)
    IndexOutOfRange { index: u32, total: u32 },
    /// no inbox item with bead id {id}
    BeadNotFound { id: String },
    /// no tune proposal with id {id}
    ProposalNotFound { id: String },
}

/// Run one `loom inbox chat` session against `workspace`.
pub fn run(workspace: &Path, opts: ChatOpts) -> Result<ChatReport, ChatError> {
    let cfg = LoomConfig::load(LoomConfig::resolve_path(workspace))
        .map_err(|e| ChatError::Config(e.to_string()))?;
    let selection = resolve_chat_selection(opts.cli_profile.as_ref(), opts.agent_override, &cfg)?;
    let image: &ImageEntry = opts.manifest.lookup(&selection.profile, selection.kind)?;

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
            applied_proposals: 0,
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
    let skill_plan = SkillPlan::resolve_from_workspace_sync(
        workspace,
        Phase::Inbox.as_str(),
        &selection.profile,
        selection.kind,
        &cfg.skills,
    )?;
    let skill_session = skill_plan.materialize(scratch_dir, workspace)?;
    let companion_paths = load_companion_paths(workspace, opts.spec_filter.as_ref(), &visible)?;
    let prompt_scratchpad_path = container_workspace_path(workspace, &scratchpad_path);
    let ctx = build_inbox_context(
        workspace,
        String::new(),
        companion_paths,
        &visible,
        prompt_scratchpad_path.to_string_lossy().into_owned(),
        skill_session.skill_index,
    );
    let prompt_body = ctx.render().map_err(|e| ChatError::Render(e.to_string()))?;

    let scratch = ScratchSession::open(workspace, &key, &prompt_body, "loom inbox chat")?;
    let restored_skills = skill_plan.materialize(scratch.path(), workspace)?;
    let bin: PathBuf = opts
        .wrix_bin
        .or_else(|| std::env::var_os("LOOM_WRIX_BIN").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(WRIX_BIN));

    let stdout = match selection.kind {
        AgentKind::Claude => {
            let claude_settings_path =
                container_workspace_path(workspace, &scratch.claude_settings());
            let argv = build_wrix_argv(
                workspace,
                &prompt_body,
                selection.kind,
                Some(&claude_settings_path),
            );
            info!(
                wrix_bin = %bin.display(),
                items_surfaced,
                profile = %selection.profile,
                agent = ?selection.kind,
                image_ref = %image.r#ref,
                scratch_dir = %scratch.path().display(),
                "loom inbox chat: shelling out to interactive wrix run",
            );

            let mut command = Command::new(&bin);
            command
                .args(&argv)
                .env(WRIX_DEFAULT_IMAGE_REF, &image.r#ref)
                .env(WRIX_DEFAULT_IMAGE_SOURCE, &image.source)
                .env("WRIX_AGENT", selection.kind.as_str());
            let output = run_wrix_and_capture_stdout(command).map_err(ChatError::Scratch)?;
            if !output.status.success() {
                return Err(ChatError::WrixExit {
                    status: output.status.to_string(),
                });
            }
            output.stdout
        }
        AgentKind::Pi => {
            let mut spawn_config = build_pi_bridge_spawn_config(
                workspace,
                image,
                &selection,
                prompt_body.clone(),
                scratch.path().to_path_buf(),
                &cfg,
            )?;
            spawn_config.skills = Some(restored_skills.registered);
            info!(
                wrix_bin = %bin.display(),
                items_surfaced,
                profile = %selection.profile,
                agent = ?selection.kind,
                image_ref = %image.r#ref,
                scratch_dir = %scratch.path().display(),
                "loom inbox chat: starting controlled pi RPC bridge",
            );
            runtime.block_on(run_pi_bridge(spawn_config, &bin))?
        }
        AgentKind::Direct => {
            return Err(ChatError::AgentSelection(
                "direct backend cannot run interactive `loom inbox chat`".to_string(),
            ));
        }
    };
    drop(scratch);

    let marker = parse_terminal_marker(&stdout)?;
    ensure_integration_clean_after_chat(workspace)?;
    let applied_proposals = match marker {
        TerminalMarker::Complete => 0,
        TerminalMarker::Apply { proposals } => {
            apply_proposals(workspace, proposals)?.proposals.len()
        }
    };

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
        applied_proposals,
    })
}

fn build_pi_bridge_spawn_config(
    workspace: &Path,
    image: &ImageEntry,
    selection: &AgentSelection,
    prompt_body: String,
    scratch_dir: PathBuf,
    cfg: &LoomConfig,
) -> Result<SpawnConfig, ChatError> {
    let mut mounts: Vec<_> = dolt_socket_mount(workspace).into_iter().collect();
    if let Some(spec) = sccache_mount(&cfg.loom)? {
        mounts.push(spec);
    }
    let mut spawn_config = crate::spawn::build_spawn_config(
        image,
        AgentKind::Pi,
        workspace.to_path_buf(),
        prompt_body,
        scratch_dir,
        cfg.loom.container_sccache_env(),
        vec![],
        mounts,
        launcher_key_env_for_checkout(workspace)?,
    );
    if let (Some(provider), Some(model_id)) = (&selection.provider, &selection.model_id) {
        spawn_config.model = Some(ModelSelection {
            provider: provider.clone(),
            model_id: model_id.clone(),
        });
    }
    spawn_config.thinking_level = selection.thinking_level;
    Ok(spawn_config)
}

async fn run_pi_bridge(config: SpawnConfig, wrix_bin: &Path) -> Result<String, ChatError> {
    let session = PiBackend::spawn_with_wrix_bin(&config, wrix_bin.as_os_str()).await?;
    let mut session = session.prompt(&config.initial_prompt).await?;
    let mut output = String::new();
    let mut envelope_builder = pi_bridge_envelope_builder()?;
    loop {
        let parsed = session
            .next_event()
            .await?
            .ok_or(ProtocolError::UnexpectedEof)?;
        let event = AgentEvent::from_parsed(parsed, envelope_builder.build());
        render_pi_bridge_event(&event, &mut output)?;
        if matches!(event, AgentEvent::CompactionStart { .. }) {
            PiBackend::on_compaction_start(&mut session, &config).await?;
        }
        if let AgentEvent::SessionComplete { exit_code, .. } = event {
            if exit_code != 0 {
                return Err(ChatError::Protocol(ProtocolError::ProcessExit(exit_code)));
            }
            match parse_terminal_marker(&output) {
                Ok(_) => {
                    ensure_bridge_output_newline(&mut output)?;
                    return Ok(output);
                }
                Err(TerminalMarkerError::Missing) => {
                    if !read_pi_bridge_follow_up(&mut session).await? {
                        return Ok(output);
                    }
                }
                Err(err) => return Err(ChatError::Terminal(err)),
            }
        }
    }
}

fn pi_bridge_envelope_builder() -> Result<EnvelopeBuilder, ChatError> {
    let bead = BeadId::new("lm-phase").map_err(|e| ChatError::Config(e.to_string()))?;
    let clock = SystemClock::new();
    Ok(EnvelopeBuilder::new(
        bead,
        None,
        0,
        Source::Agent,
        move || {
            clock
                .wall_now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64
        },
    ))
}

fn render_pi_bridge_event(event: &AgentEvent, output: &mut String) -> Result<(), std::io::Error> {
    match event {
        AgentEvent::TextDelta { text, .. } => {
            output.push_str(text);
            print!("{text}");
            std::io::stdout().flush()?;
        }
        AgentEvent::ToolCall { tool, params, .. } => {
            println!("\n[tool] {tool} {params}");
        }
        AgentEvent::ToolResult {
            output: body,
            is_error,
            ..
        } => {
            let label = if *is_error {
                "tool error"
            } else {
                "tool result"
            };
            println!("\n[{label}] {body}");
        }
        AgentEvent::ToolProgress { text, .. } => {
            println!("\n[tool progress] {text}");
        }
        AgentEvent::Error { message, .. } => {
            eprintln!("\n[agent error] {message}");
        }
        _ => {}
    }
    Ok(())
}

async fn read_pi_bridge_follow_up(session: &mut AgentSession<Active>) -> Result<bool, ChatError> {
    print!("\nloom inbox pi> ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    let n = std::io::stdin().read_line(&mut line)?;
    if n == 0 {
        return Ok(false);
    }
    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }
    session.follow_up(&line).await?;
    Ok(true)
}

fn ensure_bridge_output_newline(output: &mut String) -> Result<(), std::io::Error> {
    if !output.ends_with('\n') {
        println!();
        std::io::stdout().flush()?;
        output.push('\n');
    }
    Ok(())
}

struct WrixOutput {
    status: ExitStatus,
    stdout: String,
}

fn run_wrix_and_capture_stdout(mut command: Command) -> std::io::Result<WrixOutput> {
    let mut child = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("wrix stdout pipe unavailable"))?;
    let reader = thread::spawn(move || -> std::io::Result<String> {
        let mut captured = Vec::new();
        let mut buffer = [0_u8; 8192];
        let mut terminal = std::io::stdout();
        loop {
            let n = stdout.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            terminal.write_all(&buffer[..n])?;
            terminal.flush()?;
            captured.extend_from_slice(&buffer[..n]);
        }
        Ok(String::from_utf8_lossy(&captured).into_owned())
    });
    let status = child.wait()?;
    let stdout = reader
        .join()
        .map_err(|_| std::io::Error::other("wrix stdout reader panicked"))??;
    Ok(WrixOutput { status, stdout })
}

pub fn build_wrix_argv(
    workspace: &Path,
    prompt_body: &str,
    agent_kind: AgentKind,
    claude_settings: Option<&Path>,
) -> Vec<String> {
    let mut argv = vec![
        "run".to_string(),
        workspace.to_string_lossy().into_owned(),
        agent_command(agent_kind).to_string(),
    ];
    if matches!(agent_kind, AgentKind::Claude) {
        if let Some(settings) = claude_settings {
            argv.push("--settings".to_string());
            argv.push(settings.to_string_lossy().into_owned());
        }
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
) -> Result<AgentSelection, ChatError> {
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
    Ok(selection)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn argv_starts_with_wrix_run_and_workspace() {
        let settings = PathBuf::from("/workspace/.loom/scratch/inbox/claude-settings.json");
        let argv = build_wrix_argv(
            &PathBuf::from("/work"),
            "PROMPT",
            AgentKind::Claude,
            Some(&settings),
        );
        assert_eq!(argv[0], "run");
        assert_eq!(argv[1], "/work");
        assert_eq!(argv[2], "claude");
    }

    #[test]
    fn argv_passes_prompt_to_claude_with_skip_permissions() {
        let settings = PathBuf::from("/workspace/.loom/scratch/inbox/claude-settings.json");
        let argv = build_wrix_argv(
            &PathBuf::from("/work"),
            "PROMPT BODY",
            AgentKind::Claude,
            Some(&settings),
        );
        assert_eq!(argv[2], "claude");
        assert_eq!(argv[3], "--settings");
        assert_eq!(argv[4], settings.to_string_lossy());
        assert_eq!(argv[5], "--dangerously-skip-permissions");
        assert_eq!(argv[6], "PROMPT BODY");
    }

    #[test]
    fn argv_passes_prompt_to_pi_without_claude_flags() {
        let argv = build_wrix_argv(&PathBuf::from("/work"), "PROMPT BODY", AgentKind::Pi, None);
        assert_eq!(argv[2], "pi");
        assert_eq!(argv[3], "PROMPT BODY");
        assert!(!argv.iter().any(|a| a == "--dangerously-skip-permissions"));
        assert!(!argv.iter().any(|a| a == "--settings"));
    }

    #[test]
    fn argv_never_contains_profile_spawn_or_stdio_or_spawn_config() {
        let settings = PathBuf::from("/workspace/.loom/scratch/inbox/claude-settings.json");
        let argv = build_wrix_argv(
            &PathBuf::from("/work"),
            "PROMPT",
            AgentKind::Claude,
            Some(&settings),
        );
        assert!(!argv.iter().any(|a| a == "--profile"));
        assert!(!argv.iter().any(|a| a == "spawn"));
        assert!(!argv.iter().any(|a| a == "--stdio"));
        assert!(!argv.iter().any(|a| a == "--spawn-config"));
    }
}

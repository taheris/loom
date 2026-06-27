//! `loom inbox chat` — interactive human-decision session.
//!
//! Mirrors `loom plan`'s Claude runner shape for Claude-backed chat. Pi-backed
//! chat prefers the native Pi TUI when attached to a real terminal, with a
//! controlled RPC bridge retained for non-TTY execution and tests.

use std::fs;
use std::io::{IsTerminal, Read, Write};
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
            if should_use_pi_tui_shell_out() {
                let launch =
                    prepare_pi_tui_launch(workspace, &selection, &prompt_body, scratch.path())?;
                info!(
                    wrix_bin = %bin.display(),
                    items_surfaced,
                    profile = %selection.profile,
                    agent = ?selection.kind,
                    image_ref = %image.r#ref,
                    scratch_dir = %scratch.path().display(),
                    "loom inbox chat: shelling out to native pi TUI",
                );
                let mut command = Command::new(&bin);
                command
                    .args(&launch.argv)
                    .env(WRIX_DEFAULT_IMAGE_REF, &image.r#ref)
                    .env(WRIX_DEFAULT_IMAGE_SOURCE, &image.source)
                    .env("WRIX_AGENT", selection.kind.as_str());
                run_pi_tui_shell_out(command, &launch.session_dir)?
            } else {
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
            if verbose_pi_bridge_tools() {
                println!("\n[tool] {tool} {params}");
            } else {
                println!("\n[tool] {tool}");
            }
        }
        AgentEvent::ToolResult {
            output: body,
            is_error,
            ..
        } => {
            if (*is_error || verbose_pi_bridge_tools())
                && let Some(body) = renderable_tool_body(body, *is_error)
            {
                let label = if *is_error {
                    "tool error"
                } else {
                    "tool result"
                };
                println!("\n[{label}] {body}");
            }
        }
        AgentEvent::ToolProgress { text, .. } => {
            if verbose_pi_bridge_tools()
                && let Some(body) = renderable_tool_body(text, false)
            {
                println!("\n[tool progress] {body}");
            }
        }
        AgentEvent::Error { message, .. } => {
            eprintln!("\n[agent error] {message}");
        }
        _ => {}
    }
    Ok(())
}

fn renderable_tool_body(body: &str, is_error: bool) -> Option<&str> {
    let trimmed = body.trim();
    if !is_error && (trimmed.is_empty() || trimmed == "(no output)") {
        return None;
    }
    Some(trimmed)
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

const LOOM_INBOX_PI_FORCE_TUI: &str = "LOOM_INBOX_PI_FORCE_TUI";
const LOOM_INBOX_PI_FORCE_BRIDGE: &str = "LOOM_INBOX_PI_FORCE_BRIDGE";
const LOOM_INBOX_PI_VERBOSE_TOOLS: &str = "LOOM_INBOX_PI_VERBOSE_TOOLS";

fn should_use_pi_tui_shell_out() -> bool {
    if truthy_env(LOOM_INBOX_PI_FORCE_BRIDGE) {
        return false;
    }
    truthy_env(LOOM_INBOX_PI_FORCE_TUI)
        || (std::io::stdin().is_terminal() && std::io::stdout().is_terminal())
}

fn truthy_env(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        let value = value.trim();
        value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
    })
}

fn verbose_pi_bridge_tools() -> bool {
    truthy_env(LOOM_INBOX_PI_VERBOSE_TOOLS)
}

struct PiTuiLaunch {
    argv: Vec<String>,
    session_dir: PathBuf,
}

fn prepare_pi_tui_launch(
    workspace: &Path,
    selection: &AgentSelection,
    prompt_body: &str,
    scratch_dir: &Path,
) -> Result<PiTuiLaunch, ChatError> {
    let session_dir = scratch_dir.join("pi-sessions");
    fs::create_dir_all(&session_dir)?;

    let extension_path = scratch_dir.join("loom-pi-repin-extension.js");
    let prompt_path = container_workspace_path(workspace, &scratch_dir.join("prompt.txt"));
    let scratchpad_path = container_workspace_path(workspace, &scratch_dir.join("scratch.md"));
    fs::write(
        &extension_path,
        pi_repin_extension_source(&prompt_path, &scratchpad_path)?,
    )?;

    let container_session_dir = container_workspace_path(workspace, &session_dir);
    let container_extension_path = container_workspace_path(workspace, &extension_path);
    let argv = build_pi_tui_wrix_argv(
        workspace,
        prompt_body,
        selection,
        &container_session_dir,
        &container_extension_path,
    );
    Ok(PiTuiLaunch { argv, session_dir })
}

fn pi_repin_extension_source(
    prompt_path: &Path,
    scratchpad_path: &Path,
) -> Result<String, ChatError> {
    let prompt_path = serde_json::to_string(&prompt_path.to_string_lossy())
        .map_err(|e| ChatError::Config(format!("quote pi prompt path: {e}")))?;
    let scratchpad_path = serde_json::to_string(&scratchpad_path.to_string_lossy())
        .map_err(|e| ChatError::Config(format!("quote pi scratchpad path: {e}")))?;
    Ok(format!(
        r###"import {{ readFileSync }} from "node:fs";

export default function(pi) {{
  const promptPath = {prompt_path};
  const scratchpadPath = {scratchpad_path};

  function readText(path) {{
    try {{
      return readFileSync(path, "utf8");
    }} catch (_err) {{
      return "";
    }}
  }}

  function contentText(content) {{
    if (typeof content === "string") return content;
    if (!Array.isArray(content)) return "";
    return content.map((block) => {{
      if (!block || typeof block !== "object") return "";
      if (block.type === "text") return block.text ?? block.content ?? "";
      if (block.type === "thinking") return block.thinking ?? "";
      if (block.type === "toolCall") return `${{block.name ?? "tool"}} ${{JSON.stringify(block.arguments ?? {{}})}}`;
      return "";
    }}).join("");
  }}

  function messageText(message) {{
    if (!message || typeof message !== "object") return "";
    return contentText(message.content);
  }}

  pi.on("context", async (event) => {{
    const prompt = readText(promptPath);
    if (!prompt || !Array.isArray(event.messages)) return;
    if (event.messages.some((message) => messageText(message).includes(prompt))) return;

    const scratchpad = readText(scratchpadPath).trimEnd();
    const pinned = [
      "Loom post-compaction pinned context. Continue following this phase prompt and scratchpad exactly.",
      "",
      "## Original Loom prompt",
      prompt,
      "",
      "## Loom scratchpad",
      scratchpad || "(empty)",
    ].join("\n");

    return {{
      messages: [{{
        role: "custom",
        customType: "loom-repin",
        content: pinned,
        display: false,
        timestamp: Date.now(),
      }}, ...event.messages],
    }};
  }});
}}
"###
    ))
}

fn run_pi_tui_shell_out(mut command: Command, session_dir: &Path) -> Result<String, ChatError> {
    let captured = if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        let status = command
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()?;
        WrixOutput {
            status,
            stdout: String::new(),
        }
    } else {
        run_wrix_and_capture_stdout(command)?
    };

    if !captured.status.success() {
        return Err(ChatError::WrixExit {
            status: captured.status.to_string(),
        });
    }
    if parse_terminal_marker(&captured.stdout).is_ok() {
        return Ok(captured.stdout);
    }

    let transcript = read_pi_session_transcript(session_dir)?;
    if transcript.trim().is_empty() {
        Ok(captured.stdout)
    } else {
        Ok(transcript)
    }
}

fn read_pi_session_transcript(session_dir: &Path) -> Result<String, ChatError> {
    let Some(path) = latest_pi_session_file(session_dir)? else {
        return Ok(String::new());
    };
    let raw = fs::read_to_string(path)?;
    Ok(pi_session_transcript_from_str(&raw))
}

fn latest_pi_session_file(session_dir: &Path) -> Result<Option<PathBuf>, ChatError> {
    let entries = match fs::read_dir(session_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(ChatError::Scratch(err)),
    };
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let modified = entry
            .metadata()?
            .modified()
            .unwrap_or(std::time::UNIX_EPOCH);
        if newest
            .as_ref()
            .is_none_or(|(newest_modified, _)| modified > *newest_modified)
        {
            newest = Some((modified, path));
        }
    }
    Ok(newest.map(|(_, path)| path))
}

fn pi_session_transcript_from_str(raw: &str) -> String {
    raw.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|entry| {
            let message = entry.get("message")?;
            if message.get("role").and_then(serde_json::Value::as_str) != Some("assistant") {
                return None;
            }
            let text = pi_content_text(message.get("content"));
            (!text.trim().is_empty()).then_some(text)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn pi_content_text(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(serde_json::Value::as_str) != Some("text") {
                    return None;
                }
                block
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| block.get("content").and_then(serde_json::Value::as_str))
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
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

fn build_pi_tui_wrix_argv(
    workspace: &Path,
    prompt_body: &str,
    selection: &AgentSelection,
    session_dir: &Path,
    extension_path: &Path,
) -> Vec<String> {
    let mut argv = vec![
        "run".to_string(),
        workspace.to_string_lossy().into_owned(),
        "pi".to_string(),
        "--session-dir".to_string(),
        session_dir.to_string_lossy().into_owned(),
        "-e".to_string(),
        extension_path.to_string_lossy().into_owned(),
    ];
    if let Some(provider) = &selection.provider {
        argv.push("--provider".to_string());
        argv.push(provider.clone());
    }
    if let Some(model_id) = &selection.model_id {
        argv.push("--model".to_string());
        argv.push(model_id.clone());
    }
    if let Some(level) = selection.thinking_level {
        argv.push("--thinking".to_string());
        argv.push(level.as_str().to_string());
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
    fn pi_bridge_suppresses_empty_success_tool_bodies() {
        assert_eq!(renderable_tool_body("", false), None);
        assert_eq!(renderable_tool_body("  (no output)\n", false), None);
        assert_eq!(renderable_tool_body("failure", true), Some("failure"));
    }

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
    fn pi_tui_argv_uses_wrix_run_with_session_extension_and_model_args() {
        let selection = AgentSelection {
            profile: ProfileName::new("base"),
            kind: AgentKind::Pi,
            provider: Some("openai".to_string()),
            model_id: Some("gpt-4o".to_string()),
            thinking_level: Some(loom_driver::agent::ThinkingLevel::High),
            claude_settings: None,
        };
        let argv = build_pi_tui_wrix_argv(
            &PathBuf::from("/work"),
            "PROMPT BODY",
            &selection,
            &PathBuf::from("/work/.loom/scratch/inbox/pi-sessions"),
            &PathBuf::from("/work/.loom/scratch/inbox/loom-pi-repin-extension.js"),
        );
        assert_eq!(argv[0], "run");
        assert_eq!(argv[1], "/work");
        assert_eq!(argv[2], "pi");
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--provider" && w[1] == "openai")
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--model" && w[1] == "gpt-4o")
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--thinking" && w[1] == "high")
        );
        assert!(argv.iter().any(|a| a == "-e"));
        assert!(argv.iter().any(|a| a == "--session-dir"));
        assert_eq!(argv.last().map(String::as_str), Some("PROMPT BODY"));
        assert!(!argv.iter().any(|a| a == "spawn"));
        assert!(!argv.iter().any(|a| a == "--stdio"));
    }

    #[test]
    fn pi_session_transcript_extracts_assistant_text_blocks_for_marker_parse() {
        let raw = [
            serde_json::json!({"type":"session","version":3,"id":"s","timestamp":"now","cwd":"/work"}).to_string(),
            serde_json::json!({"type":"message","id":"u","parentId":null,"timestamp":"now","message":{"role":"user","content":"hi"}}).to_string(),
            serde_json::json!({"type":"message","id":"a","parentId":"u","timestamp":"now","message":{"role":"assistant","content":[{"type":"text","text":"Done\n"},{"type":"text","text":"LOOM_COMPLETE"}]}}).to_string(),
        ]
        .join("\n");
        assert_eq!(pi_session_transcript_from_str(&raw), "Done\nLOOM_COMPLETE");
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

//! `loom msg --chat` — interactive Drafter session.
//!
//! Mirrors `loom plan`'s runner shape: the driver renders the `msg.md`
//! template against the outstanding clarify queue, builds the same
//! `wrix run <workspace> <agent command> ... <prompt>` argv plan uses, and
//! shells out with **inherited stdio** so the configured agent attaches
//! directly to the user's terminal as a real REPL.
//!
//! This deliberately bypasses the `dispatch` stream-json surface used by
//! `loom loop` / `loom gate` / `loom todo`. Those backends pipe stdio so the
//! driver can read events and write the JSONL log — fine for non-interactive
//! sessions, fatal for an interactive chat (no readline, no color, no real
//! REPL).
//!
//! Resolution itself is the agent's responsibility: the rendered prompt
//! tells the configured agent to call `bd update <id> --notes "…"`,
//! `bd update <id> --remove-label=loom:clarify`, and `bd close <id>` per
//! resolved bead. The driver only renders, shells out, and reports — it
//! does NOT reconcile bd state after the session, per the verdict-gate
//! split between worker and interactive phases (`specs/templates.md`
//! Implementation Note 5).

use std::path::{Path, PathBuf};
use std::process::Command;

use askama::Template;
use loom_driver::agent::AgentKind;
use loom_driver::bd::{BdClient, Bead, ListOpts};
use loom_driver::config::{LoomConfig, Phase};
use loom_driver::identifier::{BeadId, ProfileName, SpecLabel};
use loom_driver::profile_manifest::{ImageEntry, ProfileError, ProfileImageManifest};
use loom_driver::scratch::{ScratchSession, resolve_scratch_key};
use loom_driver::state::CacheDb;
use thiserror::Error;
use tracing::info;

use super::context::build_msg_context;
use super::list::{filter_msg_beads, spec_label_of};

/// Default name of the wrix launcher binary on PATH. Tests override
/// via the `LOOM_WRIX_BIN` env var resolved by the CLI caller.
pub const WRIX_BIN: &str = "wrix";

/// Env vars `wrix run` reads to pick the per-profile image when no
/// `--spawn-config` is supplied — the sole profile-selection contract
/// on the interactive shell-out path. Shared with `loom plan` so the
/// chat dispatch picks up the same per-phase profile resolution.
pub const WRIX_DEFAULT_IMAGE_REF: &str = "WRIX_DEFAULT_IMAGE_REF";
pub const WRIX_DEFAULT_IMAGE_SOURCE: &str = "WRIX_DEFAULT_IMAGE_SOURCE";

/// Inputs to one [`run`] call.
#[derive(Debug)]
pub struct ChatOpts {
    /// Optional `-s <label>` filter. When `Some`, only beads carrying
    /// `spec:<label>` are surfaced in the rendered prompt.
    pub spec_filter: Option<SpecLabel>,
    /// Optional `--profile <name>` override. Wins over per-phase
    /// config and the built-in `base` default.
    pub cli_profile: Option<ProfileName>,
    /// CLI `--agent` override. When absent, `[phase.msg].agent.backend` /
    /// `[phase.default].agent.backend` select the interactive command passed
    /// to `wrix run`.
    pub agent_override: Option<AgentKind>,
    /// Resolved profile-image manifest. The driver reads this via
    /// `LOOM_PROFILES_MANIFEST`.
    pub manifest: ProfileImageManifest,
    /// Explicit path to the `wrix` launcher. `None` falls back to
    /// the `LOOM_WRIX_BIN` env var, then to `wrix` on PATH.
    pub wrix_bin: Option<PathBuf>,
}

/// Outcome of one `loom msg --chat` session.
#[derive(Debug, Clone)]
pub struct ChatReport {
    /// Number of clarify/blocked beads surfaced into the rendered
    /// prompt at session start.
    pub beads_surfaced: usize,
    /// Number of clarify/blocked beads still open after the session
    /// exited — the difference is the agent's resolved count.
    pub beads_remaining: usize,
}

#[derive(Debug, Error)]
pub enum ChatError {
    #[error("profile resolution failed")]
    Profile(#[from] ProfileError),
    #[error("config load failed: {0}")]
    Config(String),
    #[error("bd list failed: {0}")]
    BdList(String),
    #[error("render msg.md template: {0}")]
    Render(String),
    #[error("cache db operation failed while running `loom msg --chat`")]
    State(#[from] loom_driver::state::CacheError),
    #[error("scratch session io failed")]
    Scratch(#[from] std::io::Error),
    #[error("wrix exited with status {status}")]
    WrixExit { status: String },
    #[error("agent selection: {0}")]
    AgentSelection(String),
    #[error("bead identifier: {0}")]
    Identifier(String),
}

/// Run one `loom msg --chat` session against `workspace`. Returns the
/// before/after clarify counts so the caller can surface a one-line
/// summary.
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
            bd.list(ListOpts {
                label_any: vec!["loom:clarify".to_string(), "loom:blocked".to_string()],
                ..ListOpts::default()
            })
            .await
        })
        .map_err(|e| ChatError::BdList(e.to_string()))?;
    let kept: Vec<&Bead> = filter_msg_beads(&beads, opts.spec_filter.as_ref()).to_vec();
    let beads_surfaced = kept.len();
    if kept.is_empty() {
        return Ok(ChatReport {
            beads_surfaced: 0,
            beads_remaining: 0,
        });
    }

    let scope_label = opts
        .spec_filter
        .clone()
        .unwrap_or_else(|| SpecLabel::new("msg-chat"));
    let key = resolve_scratch_key(Phase::Msg, &scope_label, None);
    let scratchpad_path = ScratchSession::scratchpad_path_for(workspace, &key)
        .to_string_lossy()
        .into_owned();
    let companion_paths = load_companion_paths(workspace, opts.spec_filter.as_ref(), &kept)?;
    let ctx = build_msg_context(String::new(), companion_paths, &kept, scratchpad_path);
    let prompt_body = ctx.render().map_err(|e| ChatError::Render(e.to_string()))?;

    let banner = "loom msg --chat".to_string();
    let scratch = ScratchSession::open(workspace, &key, &prompt_body, &banner)?;

    let argv = build_wrix_argv(workspace, &prompt_body, agent_kind);
    let bin: PathBuf = opts
        .wrix_bin
        .or_else(|| std::env::var_os("LOOM_WRIX_BIN").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(WRIX_BIN));

    info!(
        wrix_bin = %bin.display(),
        beads_surfaced,
        profile = %profile,
        agent = ?agent_kind,
        image_ref = %image.r#ref,
        scratch_dir = %scratch.path().display(),
        "loom msg --chat: shelling out to interactive wrix run",
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

    // Interactive sessions own their own bd state per `specs/templates.md`
    // Implementation Note 5: the driver runs no post-session
    // reconciliation, applies no labels, and does not reverse agent
    // writes. The remaining-bead count is read fresh so the caller can
    // print a one-line summary; whatever bd state the chat agent + human
    // established is the canonical state.
    let beads_after = runtime
        .block_on(async {
            let bd = BdClient::new();
            bd.list(ListOpts {
                label_any: vec!["loom:clarify".to_string(), "loom:blocked".to_string()],
                ..ListOpts::default()
            })
            .await
        })
        .map_err(|e| ChatError::BdList(e.to_string()))?;
    let remaining_kept = filter_msg_beads(&beads_after, opts.spec_filter.as_ref());
    Ok(ChatReport {
        beads_surfaced,
        beads_remaining: remaining_kept.len(),
    })
}

/// Build the argv passed to `wrix run` — the SAME shape `loom plan`
/// uses so both interactive sessions share one entry point. `wrix
/// run` (NOT `spawn`) keeps the TTY attached and inherits the user's
/// terminal. Profile selection flows through the
/// `WRIX_DEFAULT_IMAGE_REF` / `WRIX_DEFAULT_IMAGE_SOURCE` env vars
/// exported by [`run`]; `wrix run` has no `--profile` parser (any
/// trailing tokens after the workspace are forwarded into the container
/// as the command vector).
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

/// Aggregate companion paths from the cache DB across every spec label
/// represented in the surfaced clarify queue. `msg --chat` is cross-spec by
/// default, so the queue may carry beads from multiple specs; under a
/// `--spec <label>` filter the union collapses to that single spec. Returns
/// a sorted, deduplicated list.
fn load_companion_paths(
    workspace: &Path,
    spec_filter: Option<&SpecLabel>,
    beads: &[&Bead],
) -> Result<Vec<String>, ChatError> {
    let db = CacheDb::open(workspace.join(".loom/cache.db"))?;
    let mut labels: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Some(label) = spec_filter {
        labels.insert(label.as_str().to_string());
    } else {
        for bead in beads {
            if let Some(label) = spec_label_of(bead) {
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

/// Resolve the profile `loom msg --chat` should pass to the launcher.
/// Same precedence chain as `loom plan`: CLI override → per-phase
/// config → built-in `base`.
fn resolve_chat_selection(
    cli_profile: Option<&ProfileName>,
    agent_override: Option<AgentKind>,
    config: &LoomConfig,
) -> Result<(ProfileName, AgentKind), ChatError> {
    let mut selection = config
        .agent_for(Phase::Msg)
        .map_err(|e| ChatError::AgentSelection(e.to_string()))?;
    if let Some(p) = cli_profile {
        selection.profile = p.clone();
    }
    if let Some(kind) = agent_override {
        selection.kind = kind;
    }
    if matches!(selection.kind, AgentKind::Direct) {
        return Err(ChatError::AgentSelection(
            "direct backend cannot run interactive `loom msg --chat`".to_string(),
        ));
    }
    Ok((selection.profile, selection.kind))
}

// Unused type alias — `BeadId` import kept so callers needing it via
// `msg::chat::BeadId` don't fall through to `loom_driver::identifier`.
#[doc(hidden)]
pub type _BeadId = BeadId;

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

    /// The dispatch must NEVER include `--profile`, `--stdio`, or
    /// `--spawn-config`. `wrix run` has no `--profile` parser (the
    /// flag would be forwarded into the container as a command and
    /// crash the entrypoint); profile selection rides the
    /// `WRIX_DEFAULT_IMAGE_*` env vars instead. `--stdio` /
    /// `--spawn-config` are the non-interactive surfaces `loom loop` /
    /// `check` / `todo` use — msg --chat is interactive (`wrix run`,
    /// no pi-mono protocol).
    #[test]
    fn argv_never_contains_profile_spawn_or_stdio_or_spawn_config() {
        let argv = build_wrix_argv(&PathBuf::from("/work"), "PROMPT", AgentKind::Claude);
        assert!(
            !argv.iter().any(|a| a == "--profile"),
            "wrix run has no --profile parser; profile flows via WRIX_DEFAULT_IMAGE_* env vars"
        );
        assert!(!argv.iter().any(|a| a == "spawn"));
        assert!(!argv.iter().any(|a| a == "--stdio"));
        assert!(!argv.iter().any(|a| a == "--spawn-config"));
    }
}

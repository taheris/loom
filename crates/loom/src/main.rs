//! `loom` CLI binary entry point.
//!
//! Parses command-line arguments and dispatches to the workflow modules in
//! `loom-workflow`. The set of subcommands matches the harness specification:
//! `init`, `status`, `use`, `logs`, `spec`, `loop`, `gate`, `inbox`, and
//! `tune`. There is no `sync` — Askama compiled templates make per-project
//! sync unnecessary (see `specs/harness.md`).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use clap::{ArgGroup, CommandFactory, Parser, Subcommand, ValueEnum};

use loom_agent::{ClaudeBackend, DirectBackend, PiBackend};
use loom_driver::agent::{AgentKind, LOOM_INSIDE_ENV, ProtocolError, SessionOutcome, SpawnConfig};
use loom_driver::bd::{BdClient, Bead, CommandRunner, ListOpts, UpdateOpts};
use loom_driver::clock::{Clock, SystemClock};
use loom_driver::config::{AgentObserversConfig, LoomConfig, Phase};
use loom_driver::git::GitClient;
use loom_driver::identifier::{BeadId, MoleculeId, ProfileName, SpecLabel};
use loom_driver::lock::{LockGuard, LockManager};
use loom_driver::logging::{LogSink, sweep_retention_at};
use loom_driver::profile_manifest::{ProfileError, ProfileImageManifest};
use loom_driver::scratch::resolve_scratch_key;
use loom_driver::state::CacheDb;
use loom_gate::{
    self, CacheRow, CargoMetadataScope, DispatchOptions, DispatchPendingExecutor,
    FsCommandResolver, InputResolver, RunnerSpec, StatusCache, TestScope, Tier, TierCwds, Verdict,
    filter_by_files, is_missing_binary_target, render_report,
};
use loom_protocol::todo::parse_todo_success;
use loom_workflow::inbox::{
    InboxItem, InboxKind, build_queue, build_rows, find_by_bead_id, find_by_index,
    find_by_proposal_id, parse_options_in,
};
use loom_workflow::r#loop::{
    BatchInfraFailure, BatchResult, GateOutcome, InfraDiagnostic, InfraRetryPolicy, LoopOutcome,
    NoGateReason, Parallelism, ProductionAgentLoopController, REVIEW_EMIT_STDOUT_ENV,
    REVIEW_PHASE_WHEN_ENV, REVIEW_SPEC_LABEL_ENV, RetryPolicy, SessionResult, classify_session,
    format_unknown_profile_error, format_unknown_runtime_for_profile_error,
    run_loop_with_infra_policy,
};
use loom_workflow::mint::{BatchOutcome, FindingStatusAction, FindingStatusRecord, MintWalker};
use loom_workflow::review::{
    AcceptAllFindingValidator, DispatchScope, IterationCap, ProductionReviewController, ReviewLane,
    WalkOutput, WorkspaceFindingValidator, review_loop as run_review_loop,
};
use loom_workflow::run_agent_classified;
use loom_workflow::todo::{
    ProductionTodoController, TodoError, parse_exit_signal, run as run_todo_workflow,
};
use loom_workflow::{DefaultObserverChain, init, logs_cmd, plan, spec, status, use_spec};

/// Top-level CLI surface.
#[derive(Debug, Parser)]
#[command(name = "loom", version, about = "Run the Loom workflow harness.")]
struct Cli {
    /// Workspace root. Defaults to the current working directory.
    #[arg(long, short = 'w', global = true, value_name = "PATH")]
    workspace: Option<PathBuf>,

    /// Override the agent backend for this invocation.
    #[arg(long, short = 'A', global = true, value_enum, value_name = "BACKEND")]
    agent: Option<AgentBackendArg>,

    #[command(subcommand)]
    command: Command,
}

/// CLI surface for `--agent`. Maps one-to-one with [`AgentKind`] so the
/// dispatcher does not need to re-parse strings — clap's value-enum
/// validation owns the rejection of unknown names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
enum AgentBackendArg {
    Claude,
    Pi,
    Direct,
}

impl From<AgentBackendArg> for AgentKind {
    fn from(arg: AgentBackendArg) -> Self {
        match arg {
            AgentBackendArg::Claude => AgentKind::Claude,
            AgentBackendArg::Pi => AgentKind::Pi,
            AgentBackendArg::Direct => AgentKind::Direct,
        }
    }
}

#[derive(Debug, Subcommand)]
enum GateSubcommand {
    /// Read cached gate results for an explicit scope.
    Status(GateScopeArgs),
    /// Run deterministic verification followed by LLM review.
    Audit(GateScopeArgs),
    /// Run scope-derived deterministic verifier lanes.
    Verify(GateScopeArgs),
    /// Run only `[check]`-tier annotations.
    Check(GateScopeArgs),
    /// Run only `[test]`-tier annotations.
    Test(GateScopeArgs),
    /// Run only `[system]`-tier annotations.
    System(GateScopeArgs),
    /// Run criterion-attached judges and the LLM rubric.
    Review(GateReviewArgs),
    /// Run only criterion-attached `[judge]` verifiers.
    Judge(GateScopeArgs),
    /// Run only the rubric walk.
    Rubric(GateScopeArgs),
    /// Materialize gate findings into remediation work.
    Mint(GateMintArgs),
    /// Validate `.loom/marker.json` against the workspace's HEAD tree
    /// and porcelain — prek's pre-push short-circuit.
    VerifyMarker(GateVerifyMarkerArgs),
}

/// `loom gate mint` arg surface. Mint is an act command, so its scope
/// surface is intentionally narrower than inspection subcommands.
#[derive(Debug, clap::Args)]
#[command(group(
    ArgGroup::new("mint_scope")
        .args(["molecule", "tree"])
        .multiple(false)
        .required(false),
))]
struct GateMintArgs {
    /// Promote deferred remediation beads for one molecule.
    #[arg(long, short = 'm', value_name = "ID")]
    molecule: Option<String>,
    /// Run the standing safety-net sweep across the workspace.
    #[arg(long)]
    tree: bool,
    /// Preview proposed mint writes without changing bd state.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, clap::Args)]
struct GateReviewArgs {
    #[command(flatten)]
    scope: GateScopeArgs,
    /// Attach review context to an explicit diff scope.
    #[arg(long, short = 'b', value_name = "ID")]
    bead: Option<String>,
}

#[derive(Debug, clap::Args)]
struct GateVerifyMarkerArgs {
    /// Pre-push hook id for coverage validation.
    #[arg(long, requires = "hook_entry", requires = "push_range")]
    hook_id: Option<String>,
    /// Pre-push hook entry command for coverage validation.
    #[arg(long, requires = "hook_id", requires = "push_range")]
    hook_entry: Option<String>,
    /// Resolved push range for coverage validation.
    #[arg(long, requires = "hook_id", requires = "hook_entry")]
    push_range: Option<String>,
}

#[derive(Debug, clap::Args)]
#[command(group(
    ArgGroup::new("gate_scope")
        .args(["files", "diff", "tree", "target"])
        .multiple(false)
        .required(false),
))]
struct GateScopeArgs {
    /// Scope to verifiers whose declared inputs intersect this file set.
    #[arg(long, value_name = "PATH", value_delimiter = ',', num_args = 1..)]
    files: Vec<PathBuf>,
    /// Run annotations whose target exactly matches this string.
    #[arg(long, value_name = "TARGET")]
    target: Option<String>,
    /// Scope to a git diff range.
    #[arg(long, value_name = "RANGE")]
    diff: Option<String>,
    /// Scope to every file in the workspace.
    #[arg(long)]
    tree: bool,
}

/// Subcommands of `loom note`.
#[derive(Debug, Subcommand)]
enum NoteAction {
    /// Replace all notes for the spec.
    Set {
        /// Spec label.
        label: String,
        /// JSON array of note strings: `'["note 1", "note 2"]'`.
        #[arg(long)]
        json: String,
        /// Note kind.
        #[arg(long, default_value = "implementation")]
        kind: String,
    },
    /// Append a single note to the spec.
    Add {
        /// Spec label.
        label: String,
        /// Note text.
        #[arg(long)]
        text: String,
        /// Note kind.
        #[arg(long, default_value = "implementation")]
        kind: String,
    },
    /// Delete notes for the spec.
    Clear {
        /// Spec label.
        label: String,
        /// Note kind.
        #[arg(long, default_value = "implementation", conflicts_with = "all_kinds")]
        kind: String,
        /// Clear notes across every kind.
        #[arg(long)]
        all_kinds: bool,
    },
    /// List notes for the spec.
    List {
        /// Spec label; omit to list every spec.
        label: Option<String>,
        /// Note kind.
        #[arg(long, default_value = "implementation", conflicts_with = "all_kinds")]
        kind: String,
        /// List notes across every kind.
        #[arg(long)]
        all_kinds: bool,
    },
    /// Remove a single note by id.
    Rm {
        /// Note id.
        id: i64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
enum InboxKindArg {
    Clarify,
    Blocked,
    Infra,
    Tune,
}

impl From<InboxKindArg> for InboxKind {
    fn from(arg: InboxKindArg) -> Self {
        match arg {
            InboxKindArg::Clarify => InboxKind::Clarify,
            InboxKindArg::Blocked => InboxKind::Blocked,
            InboxKindArg::Infra => InboxKind::Infra,
            InboxKindArg::Tune => InboxKind::Tune,
        }
    }
}

#[derive(Debug, Clone, clap::Args)]
struct InboxFilterArgs {
    /// Filter queue entries to a spec label.
    #[arg(long, short = 's', value_name = "LABEL")]
    spec: Option<String>,
    /// Filter queue entries by kind.
    #[arg(long, short = 'k', value_enum, value_name = "KIND")]
    kind: Option<InboxKindArg>,
}

#[derive(Debug, clap::Args)]
struct InboxArgs {
    #[command(flatten)]
    filters: InboxFilterArgs,
    #[command(subcommand)]
    action: Option<InboxAction>,
}

impl InboxArgs {
    fn mutates_workspace(&self) -> bool {
        matches!(self.action, Some(InboxAction::Chat(_)))
    }
}

#[derive(Debug, Subcommand)]
enum InboxAction {
    /// List pending human-decision and diagnostic items.
    List(InboxListArgs),
    /// Render one item host-side.
    View(InboxViewArgs),
    /// Launch interactive agent-assisted resolution.
    Chat(InboxChatArgs),
}

#[derive(Debug, clap::Args)]
struct InboxListArgs {
    #[command(flatten)]
    filters: InboxFilterArgs,
}

#[derive(Debug, clap::Args)]
struct InboxViewArgs {
    #[command(flatten)]
    filters: InboxFilterArgs,
    /// Select item by 1-based index in the filtered list.
    #[arg(value_name = "N", conflicts_with_all = ["bead", "proposal"])]
    number: Option<u32>,
    /// Select bead-backed item by id.
    #[arg(long, short = 'b', value_name = "ID", conflicts_with_all = ["number", "proposal"])]
    bead: Option<String>,
    /// Select tune proposal by id.
    #[arg(long, short = 'p', value_name = "ID", conflicts_with_all = ["number", "bead"])]
    proposal: Option<String>,
}

#[derive(Debug, clap::Args)]
struct InboxChatArgs {
    #[command(flatten)]
    filters: InboxFilterArgs,
    /// Focus chat on one list item number.
    #[arg(value_name = "N", conflicts_with_all = ["bead", "proposal"])]
    number: Option<u32>,
    /// Focus chat on a bead-backed item.
    #[arg(long, short = 'b', value_name = "ID", conflicts_with_all = ["number", "proposal"])]
    bead: Option<String>,
    /// Focus chat on a tune proposal.
    #[arg(long, short = 'p', value_name = "ID", conflicts_with_all = ["number", "bead"])]
    proposal: Option<String>,
}

#[derive(Debug, clap::Args)]
struct TuneArgs {
    #[command(subcommand)]
    action: Option<TuneAction>,
}

impl TuneArgs {
    fn mutates_workspace(&self) -> bool {
        matches!(
            &self.action,
            Some(TuneAction::Skill(args)) if args.creates_proposal()
        ) || matches!(
            &self.action,
            Some(TuneAction::Phase(args)) if args.creates_proposal()
        ) || matches!(
            &self.action,
            Some(TuneAction::Partial(args)) if args.creates_proposal()
        ) || matches!(
            &self.action,
            Some(TuneAction::All(args)) if args.creates_proposal()
        )
    }
}

#[derive(Debug, Subcommand)]
enum TuneAction {
    /// List or tune skills.
    Skill(TuneSurfaceArgs),
    /// List or tune phase templates.
    Phase(TuneSurfaceArgs),
    /// List or tune partial templates.
    Partial(TuneSurfaceArgs),
    /// List registered tuning checkers.
    Checker,
    /// List or tune every tuneable surface.
    All(TuneAllArgs),
}

#[derive(Debug, clap::Args)]
struct TuneSurfaceArgs {
    /// Proposal level.
    #[arg(value_enum, value_name = "fast|run|full")]
    level: Option<TuneLevelArg>,
    /// Target names to tune after the level.
    #[arg(value_name = "NAME")]
    targets: Vec<String>,
    /// Print the frozen tune plan without creating a proposal.
    #[arg(long, requires = "level")]
    dry_run: bool,
    /// Deterministic checker-plan seed.
    #[arg(long, value_name = "N", requires = "level")]
    seed: Option<u64>,
}

impl TuneSurfaceArgs {
    fn creates_proposal(&self) -> bool {
        self.level.is_some() && !self.dry_run
    }
}

#[derive(Debug, clap::Args)]
struct TuneAllArgs {
    /// Proposal level.
    #[arg(value_enum, value_name = "fast|run|full")]
    level: Option<TuneLevelArg>,
    /// Print the frozen tune plan without creating a proposal.
    #[arg(long, requires = "level")]
    dry_run: bool,
    /// Deterministic checker-plan seed.
    #[arg(long, value_name = "N", requires = "level")]
    seed: Option<u64>,
}

impl TuneAllArgs {
    fn creates_proposal(&self) -> bool {
        self.level.is_some() && !self.dry_run
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
enum TuneLevelArg {
    Fast,
    Run,
    Full,
}

impl From<TuneLevelArg> for loom_tune::checker::Level {
    fn from(value: TuneLevelArg) -> Self {
        match value {
            TuneLevelArg::Fast => Self::Fast,
            TuneLevelArg::Run => Self::Run,
            TuneLevelArg::Full => Self::Full,
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize the workspace (create `.loom/` config + cache DB).
    Init {
        /// Drop and repopulate the cache DB from `specs/*.md` and active beads.
        #[arg(long)]
        rebuild: bool,
    },
    /// Print cache health and work-epic status.
    Status,
    /// Validate that a spec label exists in the cache.
    #[command(name = "use")]
    UseSpec {
        /// Spec label (matches `<workspace>/specs/<label>.md`).
        label: String,
    },
    /// Render (or tail) the most recent per-bead JSONL log.
    Logs {
        /// Restrict the search to a specific bead id.
        #[arg(long, short = 'b', value_name = "ID")]
        bead: Option<String>,
        /// Tail the selected log: block on EOF until the file grows or
        /// the user interrupts.
        #[arg(long, short = 'f')]
        follow: bool,
        /// Emit raw JSONL bytes verbatim (no parsing). Composes with
        /// `-f` to tail raw JSONL. Mutually exclusive with `-v` and
        /// `--path`.
        #[arg(long, conflicts_with_all = ["verbose", "path"])]
        raw: bool,
        /// Add diagnostic event metadata to human replay rendering.
        #[arg(long, short = 'v', conflicts_with_all = ["raw", "path"])]
        verbose: bool,
        /// Print the resolved log file path and exit. Mutually
        /// exclusive with `-f`, `-v`, `--raw`.
        #[arg(long, conflicts_with_all = ["follow", "raw", "verbose"])]
        path: bool,
    },
    /// Inspect annotations and tooling dependencies for a spec.
    Spec {
        /// Spec label to inspect.
        #[arg(value_name = "LABEL")]
        label: String,
        /// Print the unique nixpkgs names referenced by spec annotation targets.
        #[arg(long)]
        deps: bool,
    },
    /// Interactive spec interview with optional initial anchors.
    Plan {
        /// Initial spec anchors; existing specs are pinned and missing specs are proposed.
        #[arg(value_name = "SPEC_LABEL")]
        anchor_labels: Vec<String>,
        /// Override the profile resolution chain. Wins over
        /// `[phase.plan].profile` and `[phase.default].profile` in
        /// `<workspace>/loom.toml` (default `base`).
        #[arg(long, value_name = "PROFILE")]
        profile: Option<String>,
    },
    /// Execute task beads from explicit work roots or the active work epic.
    Loop {
        /// Bead or epic ids to run; omitted means the sole open `loom:active` epic.
        #[arg(value_name = "BEAD_OR_EPIC_ID")]
        work_roots: Vec<String>,
        /// Concurrent dispatch slots (`-p N` / `--parallel N`). Default 1.
        #[arg(long, short = 'p', default_value = "1")]
        parallel: Parallelism,
        /// Override the per-bead `profile:X` label resolution.
        #[arg(long, value_name = "PROFILE")]
        profile: Option<String>,
        /// ASCII output, no color, no OSC 8. Pipe-safe. Implied when
        /// stdout is not a TTY or `NO_COLOR` is set. Mutually exclusive
        /// with `--json` and `--raw`.
        #[arg(long, conflicts_with_all = ["json", "raw"])]
        plain: bool,
        /// Emit one pretty-printed JSON object per line on stdout.
        /// Mutually exclusive with `--plain` and `--raw`.
        #[arg(long, conflicts_with_all = ["plain", "raw"])]
        json: bool,
        /// Emit one compact JSON line per event on stdout — same as
        /// the on-disk JSONL shape. Mutually exclusive with `--plain`,
        /// `--json`, and `-v/--verbose`.
        #[arg(long, conflicts_with_all = ["plain", "json", "verbose"])]
        raw: bool,
        /// Add diagnostic event metadata to human rendering.
        #[arg(long, short = 'v', conflicts_with = "raw")]
        verbose: bool,
        /// Mirror raw Rust tracing diagnostics to stderr.
        #[arg(long)]
        trace: bool,
    },
    /// Quality gate — annotation-dispatched verifiers and LLM rubric.
    Gate {
        #[command(subcommand)]
        subcommand: Option<GateSubcommand>,
    },
    /// Human decision and diagnostic queue.
    Inbox(InboxArgs),
    /// Tune skills and templates through isolated proposals.
    Tune(TuneArgs),
    /// Decompose the deterministic changed specs into the todo work epic.
    Todo,
    /// Manage notes for a spec.
    Note {
        #[command(subcommand)]
        action: NoteAction,
    },
}

impl Command {
    /// `true` when this subcommand spawns containers or mutates workspace
    /// state — those are refused under `LOOM_INSIDE=1` to prevent a nested
    /// driver. Read-only subcommands (`status`, `logs`, `spec`, plain
    /// `gate` status) return `false`. Spec: `harness.md` §
    /// Nested-Loom Guard.
    fn refused_inside_loom(&self) -> bool {
        match self {
            Command::Status | Command::Logs { .. } | Command::Spec { .. } => false,
            // Bare `loom gate` (help print), `gate status` (cache read),
            // and the deterministic tier subcommands are read-only
            // relative to workspace state — they parse spec files, run
            // verifiers, and write the local status cache. The
            // LLM-driven `review` / `judge` / `rubric` / `audit` paths
            // spawn agent containers, so they're refused.
            Command::Gate { subcommand: None } => false,
            Command::Gate {
                subcommand: Some(GateSubcommand::Status(_)),
            }
            | Command::Gate {
                subcommand: Some(GateSubcommand::Verify(_)),
            }
            | Command::Gate {
                subcommand: Some(GateSubcommand::Check(_)),
            }
            | Command::Gate {
                subcommand: Some(GateSubcommand::Test(_)),
            }
            | Command::Gate {
                subcommand: Some(GateSubcommand::System(_)),
            }
            | Command::Gate {
                subcommand: Some(GateSubcommand::VerifyMarker(_)),
            } => false,
            Command::Inbox(args) => args.mutates_workspace(),
            Command::Tune(args) => args.mutates_workspace(),
            Command::Init { .. }
            | Command::UseSpec { .. }
            | Command::Plan { .. }
            | Command::Loop { .. }
            | Command::Gate { .. }
            | Command::Note { .. }
            | Command::Todo => true,
        }
    }
}

/// Subcommand groups rendered in `loom --help`, in spec order. Spec
/// reference: `harness.md` § Functional #1. Clap's
/// `next_help_heading` applies to flags, not subcommands, so the binary
/// regroups the auto-generated `Commands:` block instead.
const HELP_GROUPS: &[(&str, &[&str])] = &[
    (
        "Workflow",
        &["plan", "todo", "loop", "gate", "inbox", "tune"],
    ),
    ("Inspection", &["status", "logs", "spec"]),
    ("State", &["init", "use", "note"]),
];

/// Returns `true` when the raw process args request help for the *top-level*
/// command (e.g. `loom --help`, `loom -h`, `loom help` with no further
/// subcommand). Anything that names a subcommand first — including
/// `loom loop --help` or `loom help loop` — returns `false` so clap handles
/// the per-subcommand help unchanged.
fn args_request_top_level_help(args: &[String]) -> bool {
    if args.is_empty() {
        return true;
    }
    let known_subcommands = [
        "init", "status", "use", "logs", "spec", "plan", "loop", "gate", "inbox", "tune", "todo",
        "note", "help",
    ];
    for (idx, arg) in args.iter().enumerate() {
        if known_subcommands.contains(&arg.as_str()) {
            return arg == "help" && idx == args.len() - 1;
        }
        if arg == "--help" || arg == "-h" {
            return true;
        }
    }
    false
}

/// Render `loom --help` with the spec-required Workflow / Inspection /
/// State sections instead of clap's flat `Commands:` block. Reuses
/// clap-rendered everything-else (about line, usage, options) so flag
/// changes flow through automatically — only the subcommand listing is
/// regrouped.
fn print_grouped_help() {
    use std::fmt::Write;
    let mut cmd = Cli::command();
    let default_help = cmd.render_help().to_string();

    let grouped_names: Vec<&str> = HELP_GROUPS
        .iter()
        .flat_map(|(_, names)| names.iter().copied())
        .collect();
    let width = grouped_names
        .iter()
        .copied()
        .chain(std::iter::once("help"))
        .map(str::len)
        .max()
        .unwrap_or(0);

    let mut grouped = String::new();
    for (heading, names) in HELP_GROUPS {
        let _ = writeln!(grouped, "{heading}:");
        for name in *names {
            let about = cmd
                .get_subcommands()
                .find(|s| s.get_name() == *name)
                .and_then(|s| s.get_about().map(|d| d.to_string()))
                .map(|text| help_sentence(&text))
                .unwrap_or_default();
            let _ = writeln!(grouped, "  {name:<width$}  {about}", width = width);
        }
        grouped.push('\n');
    }
    let help_about = cmd
        .get_subcommands()
        .find(|s| s.get_name() == "help")
        .and_then(|s| s.get_about().map(|d| d.to_string()))
        .map(|text| help_sentence(&text))
        .unwrap_or_else(|| {
            "Print this message or the help of the given subcommand(s).".to_string()
        });
    let _ = writeln!(
        grouped,
        "  {help:<width$}  {help_about}",
        help = "help",
        width = width
    );

    print!("{}", replace_commands_section(&default_help, &grouped));
}

fn help_sentence(text: &str) -> String {
    let trimmed = text.trim_end();
    if trimmed.is_empty() || trimmed.ends_with('.') {
        trimmed.to_string()
    } else {
        format!("{trimmed}.")
    }
}

/// Replace clap's auto-generated `Commands:` block in `help` with `grouped`.
/// The block starts at the literal line `Commands:` and ends at the blank
/// line that precedes the next top-level section (`Options:`, `Arguments:`,
/// etc.) — or EOF if none follows.
fn replace_commands_section(help: &str, grouped: &str) -> String {
    let lines: Vec<&str> = help.split_inclusive('\n').collect();
    let Some(start) = lines.iter().position(|l| *l == "Commands:\n") else {
        return help.to_string();
    };
    let mut end = lines.len();
    for i in (start + 1)..lines.len() {
        let line = lines[i];
        if !line.starts_with(' ') && !line.is_empty() && line.trim_end().ends_with(':') {
            end = i;
            while end > start + 1 && lines[end - 1].trim().is_empty() {
                end -= 1;
            }
            break;
        }
    }
    let mut out = String::new();
    for l in &lines[..start] {
        out.push_str(l);
    }
    out.push_str(grouped);
    for l in &lines[end..] {
        out.push_str(l);
    }
    out
}

fn init_tracing(command: &Command) {
    let Some(default_filter) = tracing_default_filter(command) else {
        return;
    };
    let filter = match tracing_subscriber::EnvFilter::try_from_default_env() {
        Ok(filter) => filter,
        Err(_) => tracing_subscriber::EnvFilter::new(default_filter),
    };
    if let Err(error) = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init()
    {
        eprintln!("loom: failed to initialize tracing: {error}");
    }
}

fn tracing_default_filter(command: &Command) -> Option<&'static str> {
    match command {
        Command::Loop { trace, .. } if *trace => Some("trace"),
        Command::Loop { .. } => None,
        _ => Some("info"),
    }
}

fn main() -> ExitCode {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    if args_request_top_level_help(&raw_args) {
        print_grouped_help();
        return ExitCode::SUCCESS;
    }

    let cli = Cli::parse();
    init_tracing(&cli.command);

    if std::env::var_os(LOOM_INSIDE_ENV).is_some() && cli.command.refused_inside_loom() {
        eprintln!(
            "error: loom cannot run inside a loom-managed container\n  this command spawns containers or mutates workspace state, which\n  would create a nested driver. read-only commands (status, logs,\n  spec) and deterministic gate inspection commands are still available."
        );
        return ExitCode::from(2);
    }

    let workspace = cli
        .workspace
        .unwrap_or_else(|| match std::env::current_dir() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("loom: failed to read current dir: {e}");
                std::process::exit(2);
            }
        });

    let agent_override = cli.agent.map(AgentKind::from);

    let result: anyhow::Result<ExitCode> = match cli.command {
        Command::Init { rebuild } => run_init(&workspace, rebuild).map(|()| ExitCode::SUCCESS),
        Command::Status => run_status(&workspace).map(|()| ExitCode::SUCCESS),
        Command::UseSpec { label } => run_use(&workspace, &label).map(|()| ExitCode::SUCCESS),
        Command::Logs {
            bead,
            follow,
            raw,
            verbose,
            path,
        } => run_logs(&workspace, bead.as_deref(), follow, raw, verbose, path)
            .map(|()| ExitCode::SUCCESS),
        Command::Spec { label, deps } => {
            run_spec(&workspace, label, deps).map(|()| ExitCode::SUCCESS)
        }
        Command::Plan {
            anchor_labels,
            profile,
        } => {
            run_plan(&workspace, anchor_labels, profile, agent_override).map(|()| ExitCode::SUCCESS)
        }
        Command::Loop {
            work_roots,
            parallel,
            profile,
            plain,
            json,
            raw,
            verbose,
            trace: _,
        } => run_loop_cmd(
            &workspace,
            work_roots,
            parallel,
            profile,
            agent_override,
            RenderFlags {
                plain,
                json,
                raw,
                verbose,
            },
        )
        .map(|outcome| exit_code_for_gate(&outcome.gate)),
        Command::Gate { subcommand } => {
            run_gate(&workspace, subcommand, agent_override).map(|()| ExitCode::SUCCESS)
        }
        Command::Inbox(args) => {
            run_inbox(&workspace, args, agent_override).map(|()| ExitCode::SUCCESS)
        }
        Command::Tune(args) => run_tune(&workspace, args).map(|()| ExitCode::SUCCESS),
        Command::Todo => run_todo(&workspace, agent_override).map(|()| ExitCode::SUCCESS),
        Command::Note { action } => run_note(&workspace, action).map(|()| ExitCode::SUCCESS),
    };

    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("loom: {err:#}");
            ExitCode::from(1)
        }
    }
}

/// Spec-table function: the binary's exit code is a pure function of the
/// [`GateOutcome`] variant — `Success(_)` and `NoGate { .. }` exit 0,
/// `Fail(_)` exits non-zero. Lives at the binary boundary so wrapper scripts
/// consume one canonical signal across sequential and parallel loop paths.
fn exit_code_for_gate(gate: &GateOutcome) -> ExitCode {
    match gate {
        GateOutcome::Success(_) | GateOutcome::NoGate { .. } => ExitCode::SUCCESS,
        GateOutcome::Fail(_) => ExitCode::from(1),
    }
}

fn run_tune(workspace: &std::path::Path, args: TuneArgs) -> anyhow::Result<()> {
    let Some(action) = args.action else {
        print_tune_help()?;
        return Ok(());
    };
    let request = tune_request(action);
    let runtime = tokio::runtime::Runtime::new()?;
    let response = runtime.block_on(loom_workflow::tune::run(workspace, request))?;
    print!("{}", response.render());
    Ok(())
}

fn print_tune_help() -> anyhow::Result<()> {
    let mut cmd = Cli::command();
    let Some(tune) = cmd.find_subcommand_mut("tune") else {
        anyhow::bail!("tune subcommand help is unavailable");
    };
    let mut tune_help = tune.clone().bin_name("loom tune");
    tune_help.print_help()?;
    println!();
    Ok(())
}

fn tune_request(action: TuneAction) -> loom_workflow::tune::Request {
    use loom_workflow::tune::{ListSurface, ProposeRequest, Request, Surface};
    match action {
        TuneAction::Skill(args) => tune_surface_request(Surface::Skill, ListSurface::Skill, args),
        TuneAction::Phase(args) => tune_surface_request(Surface::Phase, ListSurface::Phase, args),
        TuneAction::Partial(args) => {
            tune_surface_request(Surface::Partial, ListSurface::Partial, args)
        }
        TuneAction::Checker => Request::List(ListSurface::Checker),
        TuneAction::All(args) => match args.level {
            Some(level) => Request::Propose(ProposeRequest {
                surface: Surface::All,
                level: level.into(),
                targets: Vec::new(),
                dry_run: args.dry_run,
                seed: args.seed,
            }),
            None => Request::List(ListSurface::All),
        },
    }
}

fn tune_surface_request(
    surface: loom_workflow::tune::Surface,
    list: loom_workflow::tune::ListSurface,
    args: TuneSurfaceArgs,
) -> loom_workflow::tune::Request {
    match args.level {
        Some(level) => loom_workflow::tune::Request::Propose(loom_workflow::tune::ProposeRequest {
            surface,
            level: level.into(),
            targets: args.targets,
            dry_run: args.dry_run,
            seed: args.seed,
        }),
        None => loom_workflow::tune::Request::List(list),
    }
}

fn run_init(workspace: &std::path::Path, rebuild: bool) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let molecules = if rebuild {
        runtime.block_on(async {
            let bd = BdClient::new();
            init::fetch_active_molecules(&bd).await
        })?
    } else {
        Vec::new()
    };
    let report = init::run(workspace, init::InitOpts { rebuild }, &molecules)?;
    println!("loom init: workspace={}", workspace.display());
    println!(
        "  config: {} ({})",
        report.config_path.display(),
        if report.config_created {
            "created"
        } else {
            "kept existing"
        }
    );
    println!("  cache.db: {}", report.cache_db_path.display());
    match &report.integration_workspace {
        Some(integ) => println!(
            "  integration: {} ({})",
            integ.path.display(),
            if integ.created {
                "cloned from origin"
            } else {
                "kept existing"
            },
        ),
        None => println!("  integration: skipped (workspace has no `origin` remote)"),
    }
    if let Some(rb) = report.rebuild {
        println!(
            "  rebuilt {} spec(s), {} spec epic(s), {} work epic(s), {} companion(s)",
            rb.specs, rb.spec_epics, rb.work_epics, rb.companions,
        );
    }
    Ok(())
}

fn run_note(workspace: &std::path::Path, action: NoteAction) -> anyhow::Result<()> {
    let db = loom_driver::state::CacheDb::open(workspace.join(".loom/cache.db"))?;
    let clock = SystemClock::new();
    let now_ms = clock
        .wall_now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    match action {
        NoteAction::Set { label, json, kind } => {
            let label = SpecLabel::new(&label);
            let notes: Vec<String> = serde_json::from_str(&json)
                .map_err(|e| anyhow::anyhow!("--json must be a JSON array of strings: {e}"))?;
            db.notes_set(&label, &kind, &notes, now_ms)?;
            println!(
                "loom note set: replaced {} note(s) for spec {} (kind {kind})",
                notes.len(),
                label.as_str(),
            );
        }
        NoteAction::Add { label, text, kind } => {
            let label = SpecLabel::new(&label);
            let id = db.notes_add(&label, &kind, &text, now_ms)?;
            println!(
                "loom note add: id={id} spec={label} kind={kind}",
                label = label.as_str(),
            );
        }
        NoteAction::Clear {
            label,
            kind,
            all_kinds,
        } => {
            let label = SpecLabel::new(&label);
            let kind_arg = if all_kinds { None } else { Some(kind.as_str()) };
            db.notes_clear(&label, kind_arg)?;
            println!(
                "loom note clear: spec={} kind={}",
                label.as_str(),
                if all_kinds { "<all>" } else { kind.as_str() },
            );
        }
        NoteAction::List {
            label,
            kind,
            all_kinds,
        } => {
            let label_obj = label.as_deref().map(SpecLabel::new);
            let kind_arg = if all_kinds { None } else { Some(kind.as_str()) };
            let rows = db.notes_list(label_obj.as_ref(), kind_arg)?;
            for row in rows {
                if all_kinds {
                    println!(
                        "{id:>5} [{spec}/{kind}] {text}",
                        id = row.id,
                        spec = row.spec_label,
                        kind = row.kind,
                        text = row.text,
                    );
                } else {
                    println!(
                        "{id:>5} [{spec}] {text}",
                        id = row.id,
                        spec = row.spec_label,
                        text = row.text,
                    );
                }
            }
        }
        NoteAction::Rm { id } => {
            db.notes_rm(id)?;
            println!("loom note rm: removed id={id}");
        }
    }
    Ok(())
}

fn run_gate(
    workspace: &Path,
    subcommand: Option<GateSubcommand>,
    agent_override: Option<AgentKind>,
) -> anyhow::Result<()> {
    match subcommand {
        None => {
            // Bare `loom gate` prints identical output to
            // `loom gate --help` per spec § Commands. Triggering clap's
            // own help renderer via `try_parse_from` keeps the two
            // surfaces byte-identical without duplicating help text.
            // `--help` always returns `Err(DisplayHelp)`; the `Ok`
            // branch is unreachable but we map it to a bug error rather
            // than `unreachable!` per RS-9.
            match Cli::try_parse_from(["loom", "gate", "--help"]) {
                Ok(_) => Err(anyhow::anyhow!(
                    "clap returned Ok for `--help`; expected DisplayHelp error",
                )),
                Err(err) => {
                    err.print()?;
                    Ok(())
                }
            }
        }
        Some(GateSubcommand::Status(mut args)) => {
            if !has_scope(&args) {
                return print_gate_subcommand_help("status");
            }
            validate_status_scope(&args)?;
            resolve_gate_scope(workspace, &mut args)?;
            run_gate_status(workspace)
        }
        Some(GateSubcommand::Verify(mut args)) => {
            if !has_scope(&args) {
                return print_gate_subcommand_help("verify");
            }
            resolve_gate_scope(workspace, &mut args)?;
            run_gate_verify(workspace, &args)
        }
        Some(GateSubcommand::Check(mut args)) => {
            if !has_scope(&args) {
                return print_gate_subcommand_help("check");
            }
            resolve_gate_scope(workspace, &mut args)?;
            validate_target_for_tier(workspace, &args, Tier::Check)?;
            run_gate_single_tier(workspace, &args, Tier::Check)
        }
        Some(GateSubcommand::Test(mut args)) => {
            if !has_scope(&args) {
                return print_gate_subcommand_help("test");
            }
            resolve_gate_scope(workspace, &mut args)?;
            validate_target_for_tier(workspace, &args, Tier::Test)?;
            run_gate_single_tier(workspace, &args, Tier::Test)
        }
        Some(GateSubcommand::System(mut args)) => {
            if !has_scope(&args) {
                return print_gate_subcommand_help("system");
            }
            resolve_gate_scope(workspace, &mut args)?;
            validate_target_for_tier(workspace, &args, Tier::System)?;
            run_gate_single_tier(workspace, &args, Tier::System)
        }
        Some(GateSubcommand::Audit(mut args)) => {
            if !has_scope(&args) {
                return print_gate_subcommand_help("audit");
            }
            validate_diff_or_tree_scope(&args, "audit")?;
            resolve_gate_scope(workspace, &mut args)?;
            run_gate_audit(workspace, args, agent_override)
        }
        Some(GateSubcommand::Review(mut args)) => {
            if !has_scope(&args.scope) {
                return print_gate_subcommand_help("review");
            }
            validate_review_scope(&args.scope, args.bead.as_deref())?;
            resolve_gate_scope(workspace, &mut args.scope)?;
            run_gate_review(
                workspace,
                args.scope,
                args.bead,
                agent_override,
                ReviewLane::Both,
            )
        }
        Some(GateSubcommand::Judge(mut args)) => {
            if !has_scope(&args) {
                return print_gate_subcommand_help("judge");
            }
            resolve_gate_scope(workspace, &mut args)?;
            validate_target_for_tier(workspace, &args, Tier::Judge)?;
            run_gate_review(workspace, args, None, agent_override, ReviewLane::Judge)
        }
        Some(GateSubcommand::Rubric(mut args)) => {
            if !has_scope(&args) {
                return print_gate_subcommand_help("rubric");
            }
            validate_diff_or_tree_scope(&args, "rubric")?;
            resolve_gate_scope(workspace, &mut args)?;
            run_gate_review(workspace, args, None, agent_override, ReviewLane::Rubric)
        }
        Some(GateSubcommand::Mint(args)) => {
            if !args.tree && args.molecule.is_none() {
                return print_gate_subcommand_help("mint");
            }
            run_gate_mint(workspace, args, agent_override)
        }
        Some(GateSubcommand::VerifyMarker(args)) => run_gate_verify_marker(workspace, args),
    }
}

fn print_gate_subcommand_help(name: &str) -> anyhow::Result<()> {
    match Cli::try_parse_from(["loom", "gate", name, "--help"]) {
        Ok(_) => Err(anyhow::anyhow!(
            "clap returned Ok for `--help`; expected DisplayHelp error",
        )),
        Err(err) => {
            err.print()?;
            Ok(())
        }
    }
}

fn has_scope(args: &GateScopeArgs) -> bool {
    !args.files.is_empty() || args.diff.is_some() || args.tree || args.target.is_some()
}

fn validate_status_scope(args: &GateScopeArgs) -> anyhow::Result<()> {
    if args.target.is_some() {
        anyhow::bail!("loom gate status does not accept --target");
    }
    Ok(())
}

fn validate_diff_or_tree_scope(args: &GateScopeArgs, subcommand: &str) -> anyhow::Result<()> {
    if args.target.is_some() || !args.files.is_empty() {
        anyhow::bail!("loom gate {subcommand} requires --diff <range> or --tree");
    }
    if args.diff.is_none() && !args.tree {
        anyhow::bail!("loom gate {subcommand} requires --diff <range> or --tree");
    }
    Ok(())
}

fn validate_review_scope(args: &GateScopeArgs, bead: Option<&str>) -> anyhow::Result<()> {
    if bead.is_some() && args.diff.is_none() {
        anyhow::bail!("loom gate review --bead requires --diff <range>");
    }
    if args.target.is_some() || !args.files.is_empty() {
        anyhow::bail!("loom gate review requires --diff <range> or --tree");
    }
    if args.diff.is_none() && !args.tree {
        anyhow::bail!("loom gate review requires --diff <range> or --tree");
    }
    Ok(())
}

fn validate_target_for_tier(
    workspace: &Path,
    args: &GateScopeArgs,
    tier: Tier,
) -> anyhow::Result<()> {
    let Some(target) = args.target.as_deref() else {
        return Ok(());
    };
    let matches = target_matches(workspace, target)?;
    if !matches.contains(&tier) {
        anyhow::bail!(
            "no [{tier}] annotation target exactly matched `{target}`; run `loom spec <label> --targets` to list exact targets",
        );
    }
    Ok(())
}

fn target_matches(workspace: &Path, target: &str) -> anyhow::Result<Vec<Tier>> {
    let parsed = loom_gate::annotation::parse(&workspace.join("specs"))?;
    Ok(parsed
        .annotations
        .into_iter()
        .filter(|ann| ann.target == target)
        .map(|ann| ann.tier)
        .collect())
}

fn run_gate_verify_marker(workspace: &Path, args: GateVerifyMarkerArgs) -> anyhow::Result<()> {
    let result = match (args.hook_id, args.hook_entry, args.push_range) {
        (Some(hook_id), Some(hook_entry), Some(push_range)) => {
            let request = loom_gate::MarkerValidationRequest {
                hook_id,
                hook_entry,
                push_range,
            };
            loom_gate::verify_marker_for_hook(workspace, &request)
        }
        (None, None, None) => loom_gate::verify_marker(workspace),
        _ => anyhow::bail!("--hook-id, --hook-entry, and --push-range must be supplied together",),
    };
    match result {
        Ok(_) => Ok(()),
        Err(err) => Err(anyhow::anyhow!("{err}")),
    }
}

/// Convert `args.diff` into a populated `args.files` via
/// `git diff <range> --name-only`, per specs/gate.md § *Scope flags*:
///
/// > `--diff <range>` | Input set = `git diff <range> --name-only`
/// > (committed + working tree in the range)
///
/// Honoured for every gate subcommand that consumes a `GateScopeArgs`:
/// without this expansion the dispatcher's `args.files`-based filter
/// (in `dispatch_tier` and `run_integrity_gate`) silently runs every
/// verifier when only `--diff` is set — the very push-gate scenario
/// the spec promises will scope by intersection.
///
/// Skipped when `args.files` is already populated (explicit `--files`
/// wins) or when no `--diff` is set. `--tree` leaves both `args.diff`
/// and `args.files` unset so dispatcher's "scope was set" check
/// continues to flow through the tree-mode "match all" path.
fn expand_diff_to_files(workspace: &Path, args: &mut GateScopeArgs) -> anyhow::Result<()> {
    if !args.files.is_empty() {
        return Ok(());
    }
    let Some(range) = args.diff.as_deref() else {
        return Ok(());
    };
    let workdir = workspace.to_path_buf();
    let range_owned = range.to_string();
    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(async move {
        let client = loom_driver::git::GitClient::open(&workdir)?;
        client.changed_files_in_range(&range_owned, None).await
    });
    // A valid-but-empty diff returns `Ok(vec![])` (legitimate empty scope —
    // e.g. `HEAD` on a clean tree); only a range git itself rejects (invalid
    // commit, `@{u}` with no upstream, not a git repo) returns `Err`. The
    // latter must fail loudly: an unparseable range silently degraded to an
    // empty `args.files`, which `narrow_to_loom_files` then treats as "no
    // filter" and walks the whole tree — surfacing findings outside the
    // intended scope (and masking that the push range was never verified).
    let files = result.with_context(|| {
        format!("loom gate: --diff {range} could not be resolved to a file set")
    })?;
    args.files = files;
    Ok(())
}

/// Expand a diff scope into files and normalise file paths.
///
/// After expansion, `args.files` is normalised against `workspace`
/// (relative paths become absolute). Downstream filters accept absolute
/// scope files for both spec-section auto-includes and repo-relative
/// verifier globs, matching `--diff` output and pre-commit payloads.
fn resolve_gate_scope(workspace: &Path, args: &mut GateScopeArgs) -> anyhow::Result<()> {
    expand_diff_to_files(workspace, args)?;
    for path in &mut args.files {
        if path.is_relative() {
            *path = workspace.join(&*path);
        }
    }
    Ok(())
}

/// True iff the caller scoped to a finite file set (`--files`,
/// `--diff`, or `--bead` — and post-`resolve_gate_scope`, the
/// auto-defaulted bare invocation that becomes `--diff HEAD`).
/// `--tree` is intentionally absent: it means "run every verifier",
/// no filter. Used by `dispatch_tier` and `run_integrity_gate` to
/// distinguish "scope resolved to empty set — run nothing" (e.g.
/// clean working tree under bare invocation) from "no scope at all —
/// run everything" (`--tree`). Per specs/gate.md § *Scope flags* the
/// contract is that every finite scope flag defines an input set and
/// verifiers run iff their declared inputs intersect.
fn scope_is_finite(args: &GateScopeArgs) -> bool {
    !args.files.is_empty() || args.diff.is_some()
}

fn scope_allows_missing_binary_skip(args: &GateScopeArgs) -> bool {
    !args.files.is_empty() && args.diff.is_none() && !args.tree
}

fn gate_dispatch_options(args: &GateScopeArgs) -> DispatchOptions {
    DispatchOptions {
        files: args.files.clone(),
        spec: None,
    }
}

/// Construct an [`InputResolver`] rooted at `workspace`, wiring up the
/// `cargo metadata`-backed [`CargoMetadataScope`] when the workspace
/// manifest is reachable and `cargo` is available. Graceful degradation
/// matters here: the bead container has no `cargo`, so
/// `CargoMetadataScope::from_manifest` returns `Err` and we proceed
/// with a resolver that has no `TestScope` attached — the spec-section
/// auto-include keeps `[test]` annotations scoped to their owning spec
/// even without the cargo graph.
fn build_input_resolver(workspace: &Path, runners: &[RunnerSpec]) -> InputResolver {
    let resolver = InputResolver::new(workspace.to_path_buf()).with_runners(runners.to_vec());
    let manifest = workspace.join("Cargo.toml");
    if !manifest.exists() {
        return resolver;
    }
    match CargoMetadataScope::from_manifest(&manifest) {
        Ok(scope) => resolver.with_test_scope(Box::new(scope)),
        Err(_) => resolver,
    }
}

/// Scope for annotations already retained by the CLI-level input resolver.
struct SelectedTestScope {
    files: Vec<PathBuf>,
}

impl TestScope for SelectedTestScope {
    fn scope_for(&self, _annotation: &loom_gate::Annotation) -> Vec<PathBuf> {
        self.files.clone()
    }
}

fn filter_annotations(
    annotations: &[loom_gate::Annotation],
    tier: Tier,
    args: &GateScopeArgs,
) -> Vec<loom_gate::Annotation> {
    annotations
        .iter()
        .filter(|a| a.tier == tier)
        .filter(|a| {
            args.target
                .as_deref()
                .is_none_or(|target| a.target == target)
        })
        .filter(|a| !is_allowlisted_check_annotation(a))
        .cloned()
        .collect()
}

/// Allowlist of `(spec_file, target_substring)` pairs identifying
/// `[check]`-tier annotations that are intentionally skipped at
/// execution. The annotation is still parsed and counted by the
/// integrity gate; only the runtime execution is suppressed.
///
/// Only legitimate cases of "the verifier cannot run in this
/// environment" — never as a workaround for a verifier that should
/// pass but doesn't. Every entry carries a comment naming the cause.
const CHECK_ANNOTATION_ALLOWLIST: &[(&str, &str)] = &[
    // The three entrypoint.sh greps shell out via
    // `nix build .#wrixSrc`, which cannot run inside the
    // `nix flake check` build sandbox (no recursive nix). Skip them at
    // execution; the same greps pass when `loom gate check` runs under
    // `nix develop` or anywhere with a working `nix` on PATH.
    ("specs/agent.md", "lib/sandbox/linux/entrypoint.sh"),
];

fn is_allowlisted_check_annotation(ann: &loom_gate::Annotation) -> bool {
    let spec_str = ann.source_spec.to_string_lossy();
    CHECK_ANNOTATION_ALLOWLIST
        .iter()
        .any(|(s, t)| spec_str.ends_with(s) && ann.target.contains(t))
}

fn run_gate_status(workspace: &Path) -> anyhow::Result<()> {
    let cache_path = workspace.join(".loom/cache.db");
    let cache = StatusCache::open(&cache_path)?;
    let parsed = loom_gate::annotation::parse(&workspace.join("specs"))?;
    let now_ms = SystemClock::new()
        .wall_now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let report = render_report(&cache, &parsed, &[], now_ms, 14)?;
    print_gate_status(&report);
    Ok(())
}

fn print_gate_status(report: &loom_gate::Report) {
    for spec in &report.specs {
        println!(
            "{spec}: {total} criteria ({annotated} annotated, {unannotated} un-annotated)",
            spec = spec.spec_label,
            total = spec.criterion_total,
            annotated = spec.criterion_annotated,
            unannotated = spec.criterion_unannotated,
        );
    }
    if report.tiers.is_empty() {
        println!("(no cached verifier runs yet — run `loom gate verify` to populate)");
        return;
    }
    for tier in &report.tiers {
        println!(
            "[{tier}] last_run_ts_ms={ts:?} pass={pass} fail={fail} skipped={skipped}",
            tier = tier.tier,
            ts = tier.last_run_ts_ms,
            pass = tier.pass_count,
            fail = tier.fail_count,
            skipped = tier.skipped_count,
        );
        for fail in &tier.failing {
            println!(
                "  FAIL {spec}/{anchor}: {target} — {evidence}",
                spec = fail.spec_label,
                anchor = fail.criterion_anchor,
                target = fail.annotation_target,
                evidence = fail.evidence,
            );
        }
    }
    if !report.annotation_health.broken_annotations.is_empty() {
        println!("broken annotations:");
        for ann in &report.annotation_health.broken_annotations {
            println!(
                "  {spec}:{line}: [{tier}]({target})",
                spec = ann.source_spec.display(),
                line = ann.line,
                tier = ann.tier,
                target = ann.target,
            );
        }
    }
    if !report.annotation_health.stale_runs.is_empty() {
        println!(
            "stale runs (older than {} days):",
            report.stale_threshold_days
        );
        for stale in &report.annotation_health.stale_runs {
            println!(
                "  {spec}/{anchor}: last_run_ts_ms={ts}",
                spec = stale.spec_label,
                anchor = stale.criterion_anchor,
                ts = stale.last_run_ts_ms,
            );
        }
    }
    if !report.annotation_health.stale_annotations.is_empty() {
        println!("stale annotations:");
        for stale in &report.annotation_health.stale_annotations {
            println!(
                "  {spec}/{id}: cached [{cached_tier}]({cached_target}) current [{current_tier}]({current_target})",
                spec = stale.spec_label,
                id = stale.criterion_id,
                cached_tier = stale.cached_tier,
                cached_target = stale.cached_target,
                current_tier = stale.current_tier,
                current_target = stale.current_target,
            );
        }
    }
}

fn run_gate_verify(workspace: &Path, args: &GateScopeArgs) -> anyhow::Result<()> {
    if nested_diff_gate_skip(args) {
        eprintln!("loom gate verify --files: skipped under parent --diff gate");
        return Ok(());
    }
    let mut combined = run_project_hook_lane(workspace, args)?;
    for tier in verify_tiers_for_args(workspace, args)? {
        eprintln!("--- loom gate verify [{tier}] ---");
        match dispatch_tier(workspace, args, tier) {
            Ok(0) => {}
            Ok(code) => combined = combined.max(code),
            Err(err) => {
                eprintln!("loom gate verify [{tier}]: {err:#}");
                combined = combined.max(1);
            }
        }
    }
    if combined != 0 {
        std::process::exit(combined);
    }
    Ok(())
}

fn verify_tiers_for_args(workspace: &Path, args: &GateScopeArgs) -> anyhow::Result<Vec<Tier>> {
    if let Some(target) = args.target.as_deref() {
        let matches = target_matches(workspace, target)?;
        if matches.is_empty() {
            anyhow::bail!(
                "no annotation target exactly matched `{target}`; run `loom spec <label> --targets` to list exact targets",
            );
        }
        let first = matches[0];
        if matches.iter().any(|tier| *tier != first) {
            anyhow::bail!(
                "target `{target}` matches annotations in multiple tiers; run `loom gate <check|test|system|judge> --target {target:?}`",
            );
        }
        return Ok(vec![first]);
    }
    if args.tree {
        return Ok(vec![Tier::Check, Tier::Test, Tier::System]);
    }
    Ok(vec![Tier::Check, Tier::Test])
}

fn nested_diff_gate_skip(args: &GateScopeArgs) -> bool {
    std::env::var_os("LOOM_PARENT_DIFF_GATE").is_some()
        && !args.files.is_empty()
        && args.diff.is_none()
        && args.target.is_none()
        && !args.tree
}

fn run_project_hook_lane(workspace: &Path, args: &GateScopeArgs) -> anyhow::Result<i32> {
    let Some(range) = args.diff.as_deref() else {
        return Ok(0);
    };
    if args.target.is_some() {
        return Ok(0);
    }
    let (from_ref, to_ref) = concrete_diff_refs(workspace, range)?;
    let status = std::process::Command::new("prek")
        .current_dir(workspace)
        .env("LOOM_PARENT_DIFF_GATE", "1")
        .args([
            "run",
            "--hook-stage",
            "pre-commit",
            "--from-ref",
            &from_ref,
            "--to-ref",
            &to_ref,
        ])
        .status()
        .with_context(|| "run prek pre-commit lane for loom gate verify --diff")?;
    Ok(status.code().unwrap_or(1))
}

fn concrete_diff_refs(workspace: &Path, range: &str) -> anyhow::Result<(String, String)> {
    let (base, head) = if let Some((base, head)) = range.split_once("...") {
        (base, if head.is_empty() { "HEAD" } else { head })
    } else if let Some((base, head)) = range.split_once("..") {
        (base, if head.is_empty() { "HEAD" } else { head })
    } else {
        (range, "HEAD")
    };
    Ok((
        git_rev_parse(workspace, base)?,
        git_rev_parse(workspace, head)?,
    ))
}

fn git_rev_parse(workspace: &Path, rev: &str) -> anyhow::Result<String> {
    Ok(loom_driver::git::sync_rev_parse(workspace, rev)
        .with_context(|| format!("resolve git ref `{rev}`"))?
        .to_string())
}

fn run_gate_single_tier(workspace: &Path, args: &GateScopeArgs, tier: Tier) -> anyhow::Result<()> {
    let code = dispatch_tier(workspace, args, tier)?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

/// Resolve the `[test]`-tier runner template. `[runner.test] command =
/// "..."` from `<workspace>/loom.toml` (the consolidated `LoomConfig`)
/// wins; absent that override, fall back to toolchain detection in
/// `loom_gate::runner::discover`.
fn resolve_test_runner_template(workspace: &Path) -> anyhow::Result<loom_gate::RunnerTemplate> {
    let config = LoomConfig::load(LoomConfig::resolve_path(workspace))?;
    if let Some(tier) = config.runner.tier("test")
        && let Some(command) = tier.command.as_deref()
    {
        return Ok(loom_gate::RunnerTemplate::new(command));
    }
    Ok(loom_gate::runner::discover(workspace, Tier::Test)?)
}

/// Tier-default cwds for all four tiers, read from `<workspace>/loom.toml`'s
/// `[runner.<tier>] cwd` entries. Independent of the runner-spec list, so
/// every runner-context resolver shares one copy.
fn all_tier_cwds(config: &LoomConfig) -> TierCwds {
    TierCwds {
        check: tier_cwd(config, "check"),
        test: tier_cwd(config, "test"),
        system: tier_cwd(config, "system"),
        judge: tier_cwd(config, "judge"),
    }
}

/// Resolve the runner specs and tier-default cwds a single dispatch `tier`
/// consults from `<workspace>/loom.toml`. `[check]` always carries the
/// builtin loom-walk batcher plus any `[runner.check.<name>]` overrides;
/// `[system]` carries its `[runner.system.<name>]` blocks, so a
/// `[system](target)` resolves its inputs end-to-end through the matching
/// runner per `specs/gate.md` § Target resolution — execution stays
/// per-annotation. `[test]` / `[judge]` batch through their own templates
/// and consult no RunnerSpec list. Targets matching no configured runner
/// fall through to per-annotation spawn / the `tokens[0]`-on-PATH fallback.
fn resolve_runner_context(
    workspace: &Path,
    tier: Tier,
) -> anyhow::Result<(Vec<RunnerSpec>, TierCwds)> {
    let config = LoomConfig::load(LoomConfig::resolve_path(workspace))?;
    let tier_cwds = all_tier_cwds(&config);
    let specs = match tier {
        Tier::Check => {
            let mut specs = vec![loom_gate::runner::builtin_loom_walk_runner()?];
            specs.extend(loom_gate::runner::compile_tier_runners(&config, "check")?);
            specs
        }
        Tier::System => loom_gate::runner::compile_tier_runners(&config, "system")?,
        Tier::Test | Tier::Judge => Vec::new(),
    };
    Ok((specs, tier_cwds))
}

/// Resolve the runner specs and tier-default cwds the integrity gate's
/// forward-resolution consults: the union of the builtin loom-walk batcher
/// plus `[check]`- and `[system]`-tier runners
/// ([`loom_gate::runner::integrity_runner_specs`]), paired with every tier's
/// default cwd.
fn resolve_integrity_runner_context(
    workspace: &Path,
) -> anyhow::Result<(Vec<RunnerSpec>, TierCwds)> {
    let config = LoomConfig::load(LoomConfig::resolve_path(workspace))?;
    let tier_cwds = all_tier_cwds(&config);
    let specs = loom_gate::runner::integrity_runner_specs(&config)?;
    Ok((specs, tier_cwds))
}

fn tier_cwd(config: &LoomConfig, tier: &str) -> Option<PathBuf> {
    config
        .runner
        .tier(tier)
        .and_then(|t| t.cwd.clone())
        .map(PathBuf::from)
}

fn dispatch_tier(workspace: &Path, args: &GateScopeArgs, tier: Tier) -> anyhow::Result<i32> {
    let specs_dir = workspace.join("specs");
    let parsed = loom_gate::annotation::parse(&specs_dir)?;
    let mut selected = filter_annotations(&parsed.annotations, tier, args);
    selected.retain(|ann| !ann.pending);
    if scope_is_finite(args) {
        let runner_specs = match tier {
            Tier::Check | Tier::System => resolve_runner_context(workspace, tier)?.0,
            Tier::Test | Tier::Judge => Vec::new(),
        };
        let mut input_resolver = build_input_resolver(workspace, &runner_specs);
        selected = filter_by_files(&selected, &args.files, &mut input_resolver);
        if matches!(tier, Tier::Check | Tier::System) && scope_allows_missing_binary_skip(args) {
            let cmd_resolver = FsCommandResolver::new(workspace);
            selected.retain(|ann| !is_missing_binary_target(&ann.target, &cmd_resolver));
        }
    }

    let mut combined: i32 = 0;
    if tier == Tier::Check && args.target.is_none() {
        combined = combined.max(run_integrity_gate(workspace, args)?);
    }
    if selected.is_empty() {
        eprintln!("loom gate [{tier}]: no annotations matched");
        return Ok(combined);
    }
    let options = gate_dispatch_options(args);
    let cache_path = workspace.join(".loom/cache.db");
    let cache = StatusCache::open(&cache_path)?;
    let now_ms = SystemClock::new()
        .wall_now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let commit = current_commit(workspace).unwrap_or_default();

    match tier {
        Tier::Check => {
            let (specs, tier_cwds) = resolve_runner_context(workspace, Tier::Check)?;
            combined = run_check_with_progress(
                &selected, &options, &specs, workspace, &tier_cwds, &cache, now_ms, &commit,
                combined,
            );
        }
        Tier::System => {
            let (specs, tier_cwds) = resolve_runner_context(workspace, Tier::System)?;
            combined = run_system_with_progress(
                &selected, &options, &specs, workspace, &tier_cwds, &cache, now_ms, &commit,
                combined,
            );
        }
        Tier::Test => {
            let template = resolve_test_runner_template(workspace)?;
            let scope = SelectedTestScope {
                files: options.files.clone(),
            };
            match loom_gate::run_test(&selected, &options, &template, &scope) {
                Ok(Some(outcome)) => {
                    let verdict = if outcome.verdict.skipped {
                        Verdict::Skipped
                    } else if outcome.verdict.pass {
                        Verdict::Pass
                    } else {
                        eprintln!("loom gate [test] failed:\n{}", outcome.verdict.evidence);
                        combined = combined.max(1);
                        Verdict::Fail
                    };
                    persist_outcome(workspace, &cache, &outcome, verdict, now_ms, &commit);
                }
                Ok(None) => eprintln!("loom gate [test]: no annotations matched scope filter"),
                Err(err) => {
                    eprintln!("loom gate [test]: {err:#}");
                    combined = combined.max(1);
                }
            }
        }
        Tier::Judge => anyhow::bail!("dispatch_tier does not handle Tier::Judge"),
    }
    Ok(combined)
}

/// Split `annotations` into `(pending, non_pending)` for the integrity
/// gate's scope-finite path. The self-cleaning `?` modifier per
/// `specs/gate.md` § Pending modifier requires forward-resolution at
/// every gate scope; routing pending annotations through
/// [`filter_by_files`] would drop them whenever the spec they live in
/// is outside the staged set — common for plain test-leaf targets
/// (no `::`-segmented crate prefix) whose `CargoMetadataScope` lookup
/// collapses to the auto-included spec file. The pending half rejoins
/// the kept set after the filter runs.
fn partition_pending_for_forward_resolution(
    annotations: Vec<loom_gate::Annotation>,
) -> (Vec<loom_gate::Annotation>, Vec<loom_gate::Annotation>) {
    annotations.into_iter().partition(|a| a.pending)
}

/// Run the annotation integrity gate. The gate is itself a `[check]`-tier
/// verifier per `specs/gate.md` § Integrity gate — its findings
/// surface alongside every `loom gate check` (and therefore every `loom
/// gate verify`) run. When `--files` is non-empty (e.g. the
/// pre-commit hook), annotations are further narrowed to those whose
/// declared inputs intersect the file set per `specs/pre-commit.md`;
/// only that explicit `--files`
/// feedback path silently skips bare-binary-missing annotations so the
/// bead-container's commit flow is not broken by absent tooling.
///
/// Findings print to stderr in the spec-prescribed form and are
/// **terminal**: this function returns a non-zero exit code when any
/// finding surfaces. The spec pins findings as terminal at the push gate
/// and treats the integrity gate as itself a `[check]`-tier verifier, so
/// the verify lane fails the same way the per-annotation `[check]`
/// dispatch does.
fn run_integrity_gate(workspace: &Path, args: &GateScopeArgs) -> anyhow::Result<i32> {
    let specs_dir = workspace.join("specs");
    if !specs_dir.exists() {
        return Ok(0);
    }
    let parsed = loom_gate::annotation::parse(&specs_dir)?;
    let mut annotations: Vec<loom_gate::Annotation> = parsed.annotations;
    if annotations.is_empty() {
        return Ok(0);
    }
    let cmd_resolver = FsCommandResolver::new(workspace);
    let (specs, tier_cwds) = resolve_integrity_runner_context(workspace)?;
    if scope_is_finite(args) {
        let mut input_resolver = build_input_resolver(workspace, &specs);
        let (pending, candidates): (Vec<_>, Vec<_>) =
            partition_pending_for_forward_resolution(annotations);
        annotations = filter_by_files(&candidates, &args.files, &mut input_resolver);
        annotations.extend(pending);
        if scope_allows_missing_binary_skip(args) {
            annotations
                .retain(|ann| !is_missing_binary_target(&ann.target, &cmd_resolver) || ann.pending);
        }
        if annotations.is_empty() {
            return Ok(0);
        }
    }
    let (test_resolver, stub_scanner) = loom_gate::integrity::scan_workspace_pair(workspace)?;
    let options = DispatchOptions {
        files: args.files.clone(),
        spec: None,
    };
    let pending_executor = DispatchPendingExecutor::new(&specs, options, workspace, tier_cwds);
    let findings = loom_gate::integrity::check(
        &annotations,
        &specs,
        workspace,
        &cmd_resolver,
        &test_resolver,
        &stub_scanner,
        &pending_executor,
    );
    if findings.is_empty() {
        return Ok(0);
    }
    let mut stderr = std::io::stderr().lock();
    use std::io::Write;
    for finding in &findings {
        let _ = writeln!(stderr, "loom gate [integrity]: {finding}");
    }
    Ok(1)
}

/// Batched-aware dispatch loop for the `[check]` tier. Delegates to
/// [`loom_gate::run_check`], which routes matched annotations through
/// [`loom_gate::run_with_runners`] (one subprocess per runner group,
/// per-annotation fallback for unmatched targets) per
/// `specs/gate.md` § Runners. Pass / fail / skip handling mirrors
/// [`run_system_with_progress`].
#[expect(
    clippy::too_many_arguments,
    reason = "progress-driving dispatch surface threads cache + commit + runner context together"
)]
fn run_check_with_progress(
    selected: &[loom_gate::Annotation],
    options: &loom_gate::DispatchOptions,
    specs: &[RunnerSpec],
    repo_root: &Path,
    tier_cwds: &TierCwds,
    cache: &StatusCache,
    now_ms: i64,
    commit: &str,
    mut combined: i32,
) -> i32 {
    use std::io::{IsTerminal, Write};
    let mut stderr = std::io::stderr();
    let is_tty = stderr.is_terminal();
    let check_only: Vec<loom_gate::Annotation> = selected
        .iter()
        .filter(|a| a.tier == Tier::Check && !a.pending)
        .cloned()
        .collect();
    if check_only.is_empty() {
        return combined;
    }
    if is_tty {
        let _ = write!(
            stderr,
            "\x1b[2K\rrunning [check] ({} annotations)",
            check_only.len()
        );
        let _ = stderr.flush();
    }
    let results = loom_gate::run_check(&check_only, specs, options, repo_root, tier_cwds);
    if is_tty {
        let _ = write!(stderr, "\x1b[2K\r");
    }
    for (ann, result) in check_only.iter().zip(results) {
        match result {
            Ok(outcome) if outcome.verdict.skipped => {
                let _ = writeln!(stderr, "loom gate [check] SKIP: {}", ann.target);
                for line in outcome.verdict.evidence.lines().take(5) {
                    let _ = writeln!(stderr, "  {line}");
                }
                persist_outcome(repo_root, cache, &outcome, Verdict::Skipped, now_ms, commit);
            }
            Ok(outcome) if outcome.verdict.pass => {
                persist_outcome(repo_root, cache, &outcome, Verdict::Pass, now_ms, commit);
            }
            Ok(outcome) => {
                let _ = writeln!(stderr, "loom gate [check] FAIL: {}", ann.target);
                for line in outcome.verdict.evidence.lines().take(5) {
                    let _ = writeln!(stderr, "  {line}");
                }
                persist_outcome(repo_root, cache, &outcome, Verdict::Fail, now_ms, commit);
                combined = combined.max(1);
            }
            Err(err) => {
                let _ = writeln!(stderr, "loom gate [check] dispatch error: {}", ann.target);
                for line in format!("{err:#}").lines().take(5) {
                    let _ = writeln!(stderr, "  {line}");
                }
                combined = combined.max(1);
            }
        }
    }
    if is_tty {
        let _ = stderr.flush();
    }
    combined
}

/// Per-annotation dispatch loop for the `[system]` tier with fail-eager,
/// pass-silent output. On a TTY, an overwriting status line tracks the
/// currently-running verifier; on a pipe the line is omitted entirely.
/// Each failing verdict and each dispatch error is printed to stderr
/// as soon as the verifier returns.
///
/// `[check]` shares the same output shape (skip / pass-silent /
/// fail-loud), but routes through [`run_check_with_progress`] so the
/// matched-runner batching from `specs/gate.md` § Runners can collapse
/// N walk shell-outs into one subprocess. `[system]` execution stays
/// per-annotation per that section, but a matched runner's `cwd` (and the
/// `[runner.system]` tier-default cwd) still resolves the per-spawn
/// working directory via [`loom_gate::run_system`].
#[expect(
    clippy::too_many_arguments,
    reason = "progress-driving dispatch surface threads cache + commit + runner context together"
)]
fn run_system_with_progress(
    selected: &[loom_gate::Annotation],
    options: &loom_gate::DispatchOptions,
    specs: &[RunnerSpec],
    repo_root: &Path,
    tier_cwds: &TierCwds,
    cache: &StatusCache,
    now_ms: i64,
    commit: &str,
    mut combined: i32,
) -> i32 {
    use std::io::{IsTerminal, Write};
    let tier = Tier::System;
    let mut stderr = std::io::stderr();
    let is_tty = stderr.is_terminal();
    let total = selected.len();
    for (i, ann) in selected.iter().enumerate() {
        if is_tty {
            let target = truncate_for_progress(&ann.target, 60);
            let _ = write!(stderr, "\x1b[2K\rrunning [{}/{total}]: {target}", i + 1);
            let _ = stderr.flush();
        }
        let results = loom_gate::run_system(
            std::slice::from_ref(ann),
            specs,
            options,
            repo_root,
            tier_cwds,
        );
        for result in results {
            match result {
                Ok(outcome) if outcome.verdict.skipped => {
                    if is_tty {
                        let _ = write!(stderr, "\x1b[2K\r");
                    }
                    let _ = writeln!(stderr, "loom gate [{tier}] SKIP: {}", ann.target);
                    for line in outcome.verdict.evidence.lines().take(5) {
                        let _ = writeln!(stderr, "  {line}");
                    }
                    persist_outcome(repo_root, cache, &outcome, Verdict::Skipped, now_ms, commit);
                }
                Ok(outcome) if outcome.verdict.pass => {
                    persist_outcome(repo_root, cache, &outcome, Verdict::Pass, now_ms, commit);
                }
                Ok(outcome) => {
                    if is_tty {
                        let _ = write!(stderr, "\x1b[2K\r");
                    }
                    let _ = writeln!(stderr, "loom gate [{tier}] FAIL: {}", ann.target);
                    for line in outcome.verdict.evidence.lines().take(5) {
                        let _ = writeln!(stderr, "  {line}");
                    }
                    persist_outcome(repo_root, cache, &outcome, Verdict::Fail, now_ms, commit);
                    combined = combined.max(1);
                }
                Err(err) => {
                    if is_tty {
                        let _ = write!(stderr, "\x1b[2K\r");
                    }
                    let _ = writeln!(stderr, "loom gate [{tier}] dispatch error: {}", ann.target);
                    for line in format!("{err:#}").lines().take(5) {
                        let _ = writeln!(stderr, "  {line}");
                    }
                    combined = combined.max(1);
                }
            }
        }
    }
    if is_tty {
        let _ = write!(stderr, "\x1b[2K\r");
        let _ = stderr.flush();
    }
    combined
}

fn truncate_for_progress(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn persist_outcome(
    workspace: &Path,
    cache: &StatusCache,
    outcome: &loom_gate::DispatchOutcome,
    verdict: Verdict,
    now_ms: i64,
    commit: &str,
) {
    for ann in &outcome.annotations {
        let source_spec = if ann.source_spec.is_absolute() {
            ann.source_spec.clone()
        } else {
            workspace.join(&ann.source_spec)
        };
        let criterion_id = match criterion_id_for_annotation(&source_spec, ann) {
            Ok(id) => id,
            Err(err) => {
                eprintln!("loom gate: failed to derive criterion id: {err:#}");
                continue;
            }
        };
        let row = CacheRow {
            spec_label: spec_label_from_path(&ann.source_spec),
            criterion_anchor: criterion_id,
            tier: ann.tier,
            annotation_target: ann.target.clone(),
            last_run_ts_ms: now_ms,
            last_run_commit: commit.to_string(),
            verdict,
            evidence: outcome.verdict.evidence.clone(),
        };
        if let Err(err) = cache.upsert(&row) {
            eprintln!("loom gate: failed to upsert cache row: {err:#}");
        }
    }
}

fn criterion_id_for_annotation(
    source_spec: &Path,
    ann: &loom_gate::Annotation,
) -> anyhow::Result<String> {
    let content = std::fs::read_to_string(source_spec).with_context(|| {
        format!(
            "read criterion source spec {}",
            source_spec.to_string_lossy()
        )
    })?;
    let parsed = loom_gate::annotation::parse_content(source_spec, &content);
    let next_line = parsed
        .criteria
        .iter()
        .map(|criterion| criterion.line)
        .filter(|line| *line > ann.criterion_line)
        .min();
    let criterion_text =
        loom_workflow::todo::criterion_text_for_line(&content, ann.criterion_line, next_line);
    let label = SpecLabel::new(spec_label_from_path(&ann.source_spec));
    Ok(
        loom_workflow::todo::criterion_id_for(&label, &criterion_text)
            .as_str()
            .to_string(),
    )
}

fn spec_label_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn current_commit(workspace: &Path) -> anyhow::Result<String> {
    let git = GitClient::open(workspace)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let oid = runtime.block_on(async { git.head_commit_sha().await })?;
    Ok(oid.to_string())
}

/// `loom gate mint` CLI arm. Owns act-scope resolution, the
/// `LOOM_INSIDE` guard (delegated to the top-level main check via
/// [`Command::refused_inside_loom`]), filter passthrough, and exit-code
/// mapping. Tree-scope findings are produced by dispatching
/// [`MintScope::Tree`] through the production [`ProductionMintWalker`];
/// molecule scope promotes existing deferred remediation beads.
fn run_gate_mint(
    workspace: &Path,
    args: GateMintArgs,
    agent_override: Option<AgentKind>,
) -> anyhow::Result<()> {
    let head_commit = current_commit(workspace).unwrap_or_default();
    let scope = resolve_mint_scope(workspace, &args)?;

    let runtime = tokio::runtime::Runtime::new()?;
    let summary = match scope {
        ResolvedMintScope::Molecule(molecule) => {
            let _guard = acquire_work_root_lock(workspace, molecule.as_str())?;
            let dry_run = args.dry_run;
            runtime.block_on(async move {
                let bd = BdClient::new();
                Ok::<_, anyhow::Error>(
                    loom_workflow::mint::promote_deferred(&bd, &molecule, dry_run).await,
                )
            })?
        }
        ResolvedMintScope::Tree => {
            let manifest = Arc::new(ProfileImageManifest::from_env()?);
            let labels = resolve_tree_mint_labels(workspace, None)?;
            let config = LoomConfig::load(LoomConfig::resolve_path(workspace))?;
            let opts = loom_workflow::mint::MintOptions {
                dry_run: args.dry_run,
                spec_filter: None,
                suppressions: config.suppress.clone(),
                suppress_closed_same_molecule: true,
                report_stale: true,
            };
            let selection = resolved_agent_for(&config, agent_override, Phase::Review)?;
            let phase_default = selection.profile.clone();
            let kind = selection.kind;
            let shutdown_grace = resolve_shutdown_grace(&selection);
            let direct_output_limits = config.direct_output_limits();
            let state = Arc::new(CacheDb::open(workspace.join(".loom/cache.db"))?);
            let style_rules = config.style_rules.clone();
            let workspace_buf = workspace.to_path_buf();
            let logs_root = workspace.join(".loom/logs");
            let phase_when = phase_when_from_env().unwrap_or_else(|| SystemClock::new().wall_now());
            let render_mode = default_live_render_mode();
            let renderer_id = BeadId::new("lm-gate")?;

            runtime.block_on(async move {
                let bd = BdClient::new();
                let scope = loom_workflow::mint::MintScope::Tree;
                let validator = WorkspaceFindingValidator::new(&workspace_buf);
                if labels.is_empty() {
                    return Err(anyhow::anyhow!(
                        "loom gate mint --tree found no specs to walk"
                    ));
                }

                let label_names = labels
                    .iter()
                    .map(|label| label.as_str().to_owned())
                    .collect::<Vec<_>>();
                let spec_count = label_names.len();
                let mut progress = GateMintProgress::open(
                    &logs_root,
                    &workspace_buf,
                    render_mode,
                    &renderer_id,
                    phase_when,
                )?;
                let progress_log_path = progress.log_path().to_string_lossy().to_string();
                progress.emit(
                    loom_events::DriverKind::GateRunStart,
                    "mint tree run started",
                    serde_json::json!({
                        "gate_phase": "mint",
                        "scope": "tree",
                        "stage": "start",
                        "dry_run": args.dry_run,
                        "spec_count": spec_count,
                        "log_path": progress_log_path,
                    }),
                )?;
                progress.emit(
                    loom_events::DriverKind::GateRunScope,
                    format!("mint tree scope resolved: {spec_count} specs"),
                    serde_json::json!({
                        "gate_phase": "mint",
                        "scope": "tree",
                        "stage": "scope",
                        "spec_labels": label_names,
                    }),
                )?;

                let mut findings = Vec::new();
                let mut walk_errors: Vec<(String, String)> = Vec::new();
                for (index, label) in labels.into_iter().enumerate() {
                    let label_name = label.as_str().to_owned();
                    let label_for_sink = label.clone();
                    let logs_root_for_spawn = logs_root.clone();
                    let workspace_for_walker = workspace_buf.clone();
                    let workspace_for_renderer = workspace_buf.clone();
                    let state_for_walker = Arc::clone(&state);
                    let manifest_for_walker = Arc::clone(&manifest);
                    let phase_default_for_walker = phase_default.clone();
                    let selection_for_walker = selection.clone();
                    let style_rules_for_walker = style_rules.clone();
                    let renderer_id_for_walker = renderer_id.clone();
                    let mut walker = loom_workflow::mint::ProductionMintWalker::new(
                        BdClient::new(),
                        label,
                        workspace_for_walker,
                        state_for_walker,
                        manifest_for_walker,
                        phase_default_for_walker,
                        move |spawn_cfg: SpawnConfig| {
                            let logs_root = logs_root_for_spawn.clone();
                            let label = label_for_sink.clone();
                            let selection = selection_for_walker.clone();
                            let renderer_id = renderer_id_for_walker.clone();
                            let workspace = workspace_for_renderer.clone();
                            async move {
                                let renderer = build_stdout_renderer(
                                    render_mode,
                                    &renderer_id,
                                    &workspace,
                                    false,
                                );
                                let sink = LogSink::open_phase_at(
                                    &logs_root,
                                    &label,
                                    "mint",
                                    Some(renderer),
                                    phase_when,
                                )
                                .map_err(|e| {
                                    ProtocolError::Io(std::io::Error::other(e.to_string()))
                                })?;
                                let mut output = String::new();
                                let mut spawn_cfg = spawn_cfg;
                                selection
                                    .apply_to_spawn_config(&mut spawn_cfg, direct_output_limits);
                                let outcome = dispatch(
                                    kind,
                                    spawn_cfg,
                                    shutdown_grace,
                                    Some(sink),
                                    Some(&mut output),
                                )
                                .await?;
                                let marker = parse_exit_signal(&output);
                                Ok((outcome, marker, output))
                            }
                        },
                    )
                    .with_style_rules(style_rules_for_walker)
                    .with_agent_runtime(kind);
                    if index == 0 {
                        progress.emit(
                            loom_events::DriverKind::GateRunLane,
                            "verifier run started",
                            serde_json::json!({
                                "stage": "verifier",
                                "action": "start",
                                "scope": "tree",
                            }),
                        )?;
                        match walker.run_verifiers(&scope).await {
                            Ok(failures) => {
                                let failure_count = failures.len();
                                let mut normalized_count = 0_usize;
                                for failure in failures {
                                    match loom_workflow::mint::walk::verifier_failure_to_finding(
                                        failure,
                                    ) {
                                        Ok(finding) => {
                                            normalized_count += 1;
                                            findings.push(finding);
                                        }
                                        Err(err) => {
                                            let message = err.to_string();
                                            progress.emit(
                                                loom_events::DriverKind::GateRunLane,
                                                format!(
                                                    "verifier finding normalization failed: {message}"
                                                ),
                                                serde_json::json!({
                                                    "stage": "verifier",
                                                    "action": "normalize-failed",
                                                    "error": message,
                                                    "severity": "warning",
                                                }),
                                            )?;
                                            walk_errors.push((
                                                "walk:verifier-normalize".to_owned(),
                                                message,
                                            ));
                                        }
                                    }
                                }
                                progress.emit(
                                    loom_events::DriverKind::GateRunLane,
                                    format!(
                                        "verifier run finished: {normalized_count}/{failure_count} failures normalized"
                                    ),
                                    serde_json::json!({
                                        "stage": "verifier",
                                        "action": "end",
                                        "failure_count": failure_count,
                                        "normalized_count": normalized_count,
                                    }),
                                )?;
                            }
                            Err(err) => {
                                let message = err.to_string();
                                progress.emit(
                                    loom_events::DriverKind::GateRunLane,
                                    format!("verifier run failed: {message}"),
                                    serde_json::json!({
                                        "stage": "verifier",
                                        "action": "failed",
                                        "error": message,
                                        "severity": "warning",
                                    }),
                                )?;
                                walk_errors.push(("walk:verifiers".to_owned(), message));
                            }
                        }
                    }
                    progress.emit(
                        loom_events::DriverKind::GateRunLane,
                        format!("rubric walk started for spec:{label_name}"),
                        serde_json::json!({
                            "stage": "rubric",
                            "action": "start",
                            "spec_label": label_name,
                            "spec_index": index,
                            "spec_count": spec_count,
                        }),
                    )?;
                    match walker.run_rubric(&scope).await {
                        Ok(stdout) => {
                            let output_bytes = stdout.len();
                            match loom_workflow::review::parse_walk_output(
                                &stdout,
                                scope.dispatch_scope(),
                                &validator,
                            ) {
                                Ok(parsed) => {
                                    let parsed_findings = parsed.len();
                                    findings.extend(parsed);
                                    progress.emit(
                                        loom_events::DriverKind::GateRunLane,
                                        format!(
                                            "rubric walk finished for spec:{label_name}: {parsed_findings} findings"
                                        ),
                                        serde_json::json!({
                                            "stage": "rubric",
                                            "action": "end",
                                            "spec_label": label_name,
                                            "parsed_findings": parsed_findings,
                                            "output_bytes": output_bytes,
                                        }),
                                    )?;
                                }
                                Err(err) => {
                                    let message = err.to_string();
                                    progress.emit(
                                        loom_events::DriverKind::GateRunLane,
                                        format!(
                                            "rubric parse failed for spec:{label_name}: {message}"
                                        ),
                                        serde_json::json!({
                                            "stage": "rubric",
                                            "action": "parse-failed",
                                            "spec_label": label_name,
                                            "output_bytes": output_bytes,
                                            "error": message,
                                            "severity": "warning",
                                        }),
                                    )?;
                                    walk_errors.push((
                                        format!("walk:parse:{label_name}"),
                                        message,
                                    ));
                                }
                            }
                        }
                        Err(err) => {
                            let message = err.to_string();
                            progress.emit(
                                loom_events::DriverKind::GateRunLane,
                                format!("rubric walk failed for spec:{label_name}: {message}"),
                                serde_json::json!({
                                    "stage": "rubric",
                                    "action": "failed",
                                    "spec_label": label_name,
                                    "error": message,
                                    "severity": "warning",
                                }),
                            )?;
                            walk_errors.push((format!("walk:rubric:{label_name}"), message));
                        }
                    }
                }
                let mut mint_opts = opts;
                if !walk_errors.is_empty() {
                    mint_opts.report_stale = false;
                    progress.emit(
                        loom_events::DriverKind::GateRunLane,
                        "stale-candidate reporting suppressed after incomplete tree walk",
                        serde_json::json!({
                            "stage": "stale-reporting",
                            "action": "suppressed-incomplete-walk",
                            "walk_error_count": walk_errors.len(),
                            "report_stale": false,
                        }),
                    )?;
                }
                progress.emit(
                    loom_events::DriverKind::GateRunLane,
                    format!("minting started for {} findings", findings.len()),
                    serde_json::json!({
                        "stage": "mint",
                        "action": "start",
                        "findings_count": findings.len(),
                        "walk_error_count": walk_errors.len(),
                        "report_stale": mint_opts.report_stale,
                        "dry_run": mint_opts.dry_run,
                    }),
                )?;
                let mut summary = loom_workflow::mint::mint_tree_findings_with_options(
                    &bd,
                    &findings,
                    &head_commit,
                    &mint_opts,
                )
                .await;
                for (fingerprint, message) in walk_errors {
                    summary.record_error(fingerprint, message);
                }
                emit_mint_summary_events(&mut progress, &summary)?;
                progress.finish(&summary)?;
                Ok(summary)
            })?
        }
    };
    print!("{}", summary.render());
    if mint_summary_exit_code(&summary) != 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Per `specs/gate.md` § *Molecule mint summary semantics*, mint exits
/// 0 when `refused == 0 && errors == 0`; minted, promoted, and skipped
/// counts do not affect the process status.
#[must_use]
fn mint_summary_exit_code(summary: &loom_workflow::mint::MintSummary) -> i32 {
    if summary.refused > 0 || summary.errors > 0 {
        1
    } else {
        0
    }
}

/// Strict walk-then-mint test seam. Production `run_gate_mint --tree`
/// performs a loss-preserving multi-spec walk so one failed rubric source
/// does not discard findings already collected from other specs; this helper
/// keeps the direct `mint::walk::walk(walker, …)` path exercisable under
/// recording fakes.
#[cfg(test)]
async fn mint_via_walker<W, V, R>(
    walker: &mut W,
    scope: &loom_workflow::mint::MintScope,
    validator: &V,
    bd: &BdClient<R>,
    head_commit: &str,
    opts: &loom_workflow::mint::MintOptions,
) -> anyhow::Result<loom_workflow::mint::MintSummary>
where
    W: loom_workflow::mint::MintWalker,
    V: loom_workflow::review::FindingValidator + ?Sized,
    R: loom_driver::bd::CommandRunner,
{
    let findings = loom_workflow::mint::walk(walker, scope, validator).await?;
    Ok(
        loom_workflow::mint::mint_tree_findings_with_options(bd, &findings, head_commit, opts)
            .await,
    )
}

#[derive(Debug)]
enum ResolvedMintScope {
    Tree,
    Molecule(loom_driver::identifier::MoleculeId),
}

fn resolve_mint_scope(_workspace: &Path, args: &GateMintArgs) -> anyhow::Result<ResolvedMintScope> {
    if args.tree {
        return Ok(ResolvedMintScope::Tree);
    }
    if let Some(molecule) = &args.molecule {
        return Ok(ResolvedMintScope::Molecule(molecule.parse()?));
    }
    Err(anyhow::anyhow!(
        "loom gate mint requires --tree or -m/--molecule",
    ))
}

fn run_gate_audit(
    workspace: &Path,
    args: GateScopeArgs,
    agent_override: Option<AgentKind>,
) -> anyhow::Result<()> {
    let verify_result = run_gate_verify(workspace, &args);
    let review_result = run_gate_review(workspace, args, None, agent_override, ReviewLane::Both);
    verify_result.and(review_result)
}

fn run_gate_review(
    workspace: &Path,
    args: GateScopeArgs,
    bead: Option<String>,
    agent_override: Option<AgentKind>,
    lane: ReviewLane,
) -> anyhow::Result<()> {
    run_review(
        workspace,
        None,
        agent_override,
        ReviewOpts {
            bead,
            diff: args.diff,
            tree: args.tree,
            lane,
        },
    )
}

fn run_status(workspace: &std::path::Path) -> anyhow::Result<()> {
    let db = loom_driver::state::CacheDb::open(workspace.join(".loom/cache.db"))?;
    let config = LoomConfig::load(LoomConfig::resolve_path(workspace))?;
    let report = status::load(&db, config.loom.integration_branch)?;
    print!("{}", status::render(&report));
    Ok(())
}

fn run_use(workspace: &std::path::Path, label: &str) -> anyhow::Result<()> {
    let label = SpecLabel::new(label);
    let db_path = workspace.join(".loom/cache.db");
    use_spec::run(workspace, &label, &db_path)?;
    println!("spec exists: {label}");
    Ok(())
}

fn run_logs(
    workspace: &std::path::Path,
    bead: Option<&str>,
    follow: bool,
    raw: bool,
    verbose: bool,
    path_only: bool,
) -> anyhow::Result<()> {
    let logs_root = workspace.join(".loom/logs");
    let bead_id = bead.map(BeadId::new).transpose()?;
    let path = match logs_cmd::select_log(
        &logs_root,
        logs_cmd::LogsOpts {
            bead: bead_id.as_ref(),
        },
    ) {
        Ok(p) => p,
        // Bare `loom logs` with an empty directory is the steady state
        // on a fresh workspace — render a one-liner and exit 0 instead
        // of a typed error. `--path` keeps the typed error so scripts
        // can detect the missing-file case cheaply.
        Err(logs_cmd::LogsError::NoLogs { .. }) if !path_only => {
            println!("No bead logs yet");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    if path_only {
        println!("{}", path.display());
        return Ok(());
    }
    // Derive a renderer bead id from the selected file's stem so the
    // renderer chrome (header, recovery hints) carries the right id
    // even when `--bead` is not passed. Falls back to a sentinel when
    // the stem doesn't parse — we still render successfully.
    let renderer_bead = match bead_id.clone().or_else(|| derive_bead_id_from_path(&path)) {
        Some(b) => b,
        None => BeadId::new("lm-x")
            .map_err(|err| anyhow::anyhow!("`lm-x` sentinel must parse as BeadId: {err}"))?,
    };
    let mode = resolve_replay_mode(raw, verbose);
    let clock: Arc<dyn loom_driver::clock::Clock> = Arc::new(loom_driver::clock::SystemClock);
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(logs_cmd::replay(
        logs_cmd::ReplayOpts {
            path: &path,
            bead_id: renderer_bead,
            mode,
            follow,
            follow_poll: None,
            follow_max_polls: None,
        },
        Box::new(std::io::stdout()),
        clock,
    ))?;
    Ok(())
}

fn derive_bead_id_from_path(path: &Path) -> Option<BeadId> {
    let stem = path.file_stem().and_then(|s| s.to_str())?;
    // Stem layout: `<bead-id>-<UTC timestamp>`. Stamp shape:
    // `YYYYMMDDTHHMMSSZ` — split on the final `-` and reparse.
    let (head, _) = stem.rsplit_once('-')?;
    BeadId::new(head).ok()
}

fn resolve_replay_mode(raw: bool, verbose: bool) -> logs_cmd::ReplayMode {
    if raw {
        return logs_cmd::ReplayMode::Raw;
    }
    let tty = loom_render::in_place::stdout_supports_indicator();
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let base = loom_render::RenderMode::select(tty, no_color, false, false, false);
    let mode = if verbose {
        match base {
            loom_render::RenderMode::Pretty => loom_render::RenderMode::Verbose,
            loom_render::RenderMode::Plain => loom_render::RenderMode::VerbosePlain,
            other => other,
        }
    } else {
        base
    };
    logs_cmd::ReplayMode::Render(mode)
}

fn run_plan(
    workspace: &std::path::Path,
    anchor_label_args: Vec<String>,
    profile: Option<String>,
    agent_override: Option<AgentKind>,
) -> anyhow::Result<()> {
    let manifest = ProfileImageManifest::from_env()?;
    let anchor_labels = plan::parse_anchor_labels(anchor_label_args)?;
    let report = plan::run(
        workspace,
        plan::PlanOpts {
            anchor_labels,
            wrix_bin: std::env::var_os("LOOM_WRIX_BIN").map(PathBuf::from),
            cli_profile: profile.map(ProfileName::new),
            agent_override,
            manifest,
        },
    )?;
    if report.anchor_labels.is_empty() {
        println!("loom plan: anchors=(none)");
    } else {
        let anchors = report
            .anchor_labels
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        println!("loom plan: anchors={anchors}");
    }
    if report.companion_paths.is_empty() {
        println!("  companions: (none)");
    } else {
        println!("  companions:");
        for path in &report.companion_paths {
            println!("    - {path}");
        }
    }
    Ok(())
}

/// CLI-level render mode flags for `loom loop`. Resolved into a
/// [`loom_render::RenderMode`] at sink-open time via
/// [`loom_render::RenderMode::select`]; the latter takes a TTY bool +
/// the spec'd flag table and decides Pretty/Plain/Json/Raw.
#[derive(Debug, Clone, Copy)]
struct RenderFlags {
    plain: bool,
    json: bool,
    raw: bool,
    verbose: bool,
}

fn run_loop_cmd(
    workspace: &Path,
    work_roots: Vec<String>,
    parallel: Parallelism,
    profile: Option<String>,
    agent_override: Option<AgentKind>,
    render_flags: RenderFlags,
) -> anyhow::Result<LoopOutcome> {
    let manifest = Arc::new(ProfileImageManifest::from_env()?);
    let runtime = tokio::runtime::Runtime::new()?;
    let roots = runtime.block_on(async {
        let bd = BdClient::new();
        resolve_loop_work_roots(&bd, work_roots).await
    })?;
    let multi_root = roots.len() > 1;

    let config = LoomConfig::load(LoomConfig::resolve_path(workspace))?;
    sweep_retention_at(
        &workspace.join(".loom/logs"),
        config.logs.retention_days,
        SystemClock::new().wall_now(),
    );
    // Resolve the per-phase backend up front so an unknown backend name in
    // the config (or via `--agent` — clap covers the latter) fails before
    // any work begins. The resolution itself is the wiring; the dispatch
    // closure handed to the parallel batch driver below is what consumes it.
    let selection = resolved_agent_for(&config, agent_override, Phase::Loop)?;
    let phase_default = selection.profile.clone();
    let cli_profile = profile.map(ProfileName::new);
    let loom_bin = current_loom_bin()?;
    let shutdown_grace = resolve_shutdown_grace(&selection);
    let direct_output_limits = config.direct_output_limits();

    let mut aggregate = LoopOutcomeAccumulator::default();
    if !parallel.is_one() {
        if roots
            .iter()
            .any(|root| matches!(&root.kind, LoopWorkRootKind::Task))
        {
            anyhow::bail!("loom loop --parallel does not accept task bead roots");
        }
        let parallel_n = parallel.get();
        let render_mode = resolve_render_mode(render_flags);
        let mut startup_reconcile = StartupReconcile::FastForward;
        for root in &roots {
            let outcome = run_parallel_loop_root(
                &runtime,
                workspace,
                root,
                parallel_n,
                selection.kind,
                selection.clone(),
                direct_output_limits,
                shutdown_grace,
                Arc::clone(&manifest),
                cli_profile.clone(),
                phase_default.clone(),
                &config,
                render_mode,
                startup_reconcile,
            )?;
            startup_reconcile = StartupReconcile::AlreadyDone;
            print_parallel_loop_summary(multi_root, root, parallel_n, &outcome);
            aggregate.push(outcome);
        }
        let outcome = aggregate.finish();
        if multi_root {
            print_parallel_aggregate_summary(parallel_n, &outcome);
        }
        return Ok(outcome);
    }

    // Origin reconciliation is invocation startup, not per selected root.
    // A root can leave `.loom/integration` ahead when its push gate fails;
    // rerunning the startup fast-forward before the next positional bead
    // would misclassify that in-invocation advance as pre-existing divergence.
    let mut startup_reconcile = StartupReconcile::FastForward;
    for root in &roots {
        let outcome = run_sequential_loop_root(
            &runtime,
            workspace,
            root,
            Arc::clone(&manifest),
            cli_profile.clone(),
            phase_default.clone(),
            selection.kind,
            selection.clone(),
            direct_output_limits,
            shutdown_grace,
            &config,
            loom_bin.clone(),
            render_flags,
            startup_reconcile,
        )?;
        startup_reconcile = StartupReconcile::AlreadyDone;
        print_sequential_loop_summary(multi_root, root, &outcome);
        aggregate.push(outcome);
    }

    let outcome = aggregate.finish();
    if multi_root {
        print_loop_summary("loom loop:", &outcome);
    }
    Ok(outcome)
}

#[derive(Debug, Default)]
struct LoopOutcomeAccumulator {
    beads_processed: u32,
    beads_clarified: u32,
    beads_blocked: u32,
    outer_iterations: u32,
    decisive_gate: Option<GateOutcome>,
    /// At least one explicit bead/root selection processed work without a
    /// molecule-completion gate. Keep the aggregate conservative: a later
    /// gated root must not make earlier ungated work look like a sealed
    /// `GateSuccess` for the whole invocation.
    ungated_work: bool,
}

impl LoopOutcomeAccumulator {
    fn push(&mut self, outcome: LoopOutcome) {
        let LoopOutcome {
            beads_processed,
            beads_clarified,
            beads_blocked,
            outer_iterations,
            gate,
        } = outcome;
        self.beads_processed = self.beads_processed.saturating_add(beads_processed);
        self.beads_clarified = self.beads_clarified.saturating_add(beads_clarified);
        self.beads_blocked = self.beads_blocked.saturating_add(beads_blocked);
        self.outer_iterations = self.outer_iterations.saturating_add(outer_iterations);
        if matches!(&gate, GateOutcome::NoGate { .. }) && beads_processed > 0 {
            self.ungated_work = true;
        }
        self.decisive_gate = merge_decisive_gate(self.decisive_gate.take(), gate);
    }

    fn finish(self) -> LoopOutcome {
        let gate = match self.decisive_gate {
            Some(gate @ GateOutcome::Fail(_)) => gate,
            Some(gate) if !self.ungated_work => gate,
            Some(_) | None => GateOutcome::NoGate {
                beads_processed: self.beads_processed,
                reason: if self.beads_processed == 0 {
                    NoGateReason::NoBeadsReady
                } else {
                    NoGateReason::SelectionPartial
                },
            },
        };
        LoopOutcome {
            beads_processed: self.beads_processed,
            beads_clarified: self.beads_clarified,
            beads_blocked: self.beads_blocked,
            outer_iterations: self.outer_iterations,
            gate,
        }
    }
}

fn merge_decisive_gate(current: Option<GateOutcome>, next: GateOutcome) -> Option<GateOutcome> {
    match (current, next) {
        (Some(gate @ GateOutcome::Fail(_)), _) => Some(gate),
        (_, gate @ GateOutcome::Fail(_)) => Some(gate),
        (_, gate @ GateOutcome::Success(_)) => Some(gate),
        (current, GateOutcome::NoGate { .. }) => current,
    }
}

#[expect(clippy::too_many_arguments, reason = "CLI loop root wiring surface")]
fn run_parallel_loop_root(
    runtime: &tokio::runtime::Runtime,
    workspace: &Path,
    root: &LoopWorkRoot,
    parallel_n: u32,
    kind: AgentKind,
    selection: loom_driver::config::AgentSelection,
    direct_output_limits: loom_driver::agent::OutputLimits,
    shutdown_grace: Option<Duration>,
    manifest: Arc<ProfileImageManifest>,
    cli_profile: Option<ProfileName>,
    phase_default: ProfileName,
    config: &LoomConfig,
    render_mode: loom_render::RenderMode,
    startup_reconcile: StartupReconcile,
) -> anyhow::Result<LoopOutcome> {
    if matches!(&root.kind, LoopWorkRootKind::Task) {
        anyhow::bail!("loom loop --parallel does not accept task bead roots");
    }
    let _guard = acquire_work_root_lock(workspace, root.id.as_str())?;
    prepare_loop_root(runtime, workspace, root, &config.loom, startup_reconcile)?;
    let workspace_buf = workspace.to_path_buf();
    let label_for_async = root.label.clone();
    let ready_parent_for_async = root.ready_parent.clone();
    let style_rules_for_async = config.style_rules.clone();
    let loom_cfg_for_async = config.loom.clone();
    let skills_cfg_for_async = config.skills.clone();
    let observer_config = config.agent.clone();
    let infra_policy = InfraRetryPolicy {
        max_attempts: config.loop_.infra.max_attempts,
    };
    runtime.block_on(async move {
        run_parallel_loop(
            workspace_buf,
            label_for_async,
            ready_parent_for_async,
            parallel_n,
            kind,
            selection,
            direct_output_limits,
            shutdown_grace,
            manifest,
            cli_profile,
            phase_default,
            style_rules_for_async,
            loom_cfg_for_async,
            skills_cfg_for_async,
            observer_config,
            render_mode,
            infra_policy,
        )
        .await
    })
}

#[expect(clippy::too_many_arguments, reason = "CLI loop root wiring surface")]
fn run_sequential_loop_root(
    runtime: &tokio::runtime::Runtime,
    workspace: &Path,
    root: &LoopWorkRoot,
    manifest: Arc<ProfileImageManifest>,
    cli_profile: Option<ProfileName>,
    phase_default: ProfileName,
    kind: AgentKind,
    selection: loom_driver::config::AgentSelection,
    direct_output_limits: loom_driver::agent::OutputLimits,
    shutdown_grace: Option<Duration>,
    config: &LoomConfig,
    loom_bin: PathBuf,
    render_flags: RenderFlags,
    startup_reconcile: StartupReconcile,
) -> anyhow::Result<LoopOutcome> {
    let work_root_guard = acquire_work_root_lock(workspace, root.id.as_str())?;
    prepare_loop_root(runtime, workspace, root, &config.loom, startup_reconcile)?;

    let label = root.label.clone();
    let fixed_bead = matches!(&root.kind, LoopWorkRootKind::Task).then(|| root.bead.clone());
    let ready_parent = root.ready_parent.clone();
    let workspace_buf = workspace.to_path_buf();
    let workspace_for_renderer = workspace.to_path_buf();
    let logs_root = workspace.join(".loom/logs");
    let logs_root_for_controller = logs_root.clone();
    let label_for_sink = label.clone();
    let render_mode = resolve_render_mode(render_flags);
    let style_rules_for_run = config.style_rules.clone();
    let loom_cfg_for_run = config.loom.clone();
    let skills_cfg_for_run = config.skills.clone();
    let observer_config = config.agent.clone();
    let retry_policy = RetryPolicy {
        max_retries: config.loop_.max_retries,
    };
    let infra_policy = InfraRetryPolicy {
        max_attempts: config.loop_.infra.max_attempts,
    };
    let max_iterations = config.loop_.max_iterations;
    let git =
        GitClient::open_with_integration_branch(workspace, config.loom.integration_branch.clone())?
            .with_hook_timeout(config.loom.git_hook_timeout());
    let outcome = runtime.block_on(async move {
        let bd = BdClient::new();
        let mut controller = ProductionAgentLoopController::new(
            bd,
            label.clone(),
            loom_bin,
            workspace_buf,
            git,
            manifest,
            cli_profile,
            phase_default,
            move |spawn_cfg: SpawnConfig, bead_id: BeadId| {
                let logs_root = logs_root.clone();
                let label = label_for_sink.clone();
                let workspace = workspace_for_renderer.clone();
                let observer_config = observer_config.clone();
                let selection = selection.clone();
                async move {
                    // A sink-open failure is pre-spawn — the bead's
                    // JSONL log location is part of the workflow's
                    // pre-flight setup. Bubble it through the same
                    // `infra-preflight` path so `bd update` records the
                    // cause instead of the error tearing down `loom loop`.
                    let sink = match open_bead_sink_with_renderer(
                        &logs_root,
                        &label,
                        &bead_id,
                        render_mode,
                        &workspace,
                        false,
                    ) {
                        Ok(s) => Some(s),
                        Err(err) => {
                            return (
                                SessionResult::PreflightFailed {
                                    error: format!("open log sink: {err:#}"),
                                },
                                None,
                            );
                        }
                    };
                    let mut output = String::new();
                    let mut spawn_cfg = spawn_cfg;
                    selection.apply_to_spawn_config(&mut spawn_cfg, direct_output_limits);
                    let envelope_builder = build_envelope_builder(bead_id.clone());
                    let session = dispatch_classified(
                        kind,
                        spawn_cfg,
                        shutdown_grace,
                        sink,
                        Some(&mut output),
                        Some(envelope_builder),
                        observer_config,
                    )
                    .await;
                    let marker = parse_exit_signal(&output);
                    (session, marker)
                }
            },
        )
        .with_style_rules(style_rules_for_run)
        .with_agent_runtime(kind)
        .with_loom_config(loom_cfg_for_run)
        .with_skills_config(skills_cfg_for_run)
        .with_phase_log_root(logs_root_for_controller);
        if let Some(bead) = fixed_bead {
            controller = controller.with_fixed_queue(std::collections::VecDeque::from([bead]));
        }
        if let Some(parent) = ready_parent {
            controller = controller.with_ready_parent(parent);
        }
        controller = controller.with_handoff_lock(work_root_guard);
        run_loop_with_infra_policy(&mut controller, retry_policy, infra_policy, max_iterations)
            .await
    })?;
    // The marker is minted inside the molecule-completion push gate's
    // critical section (review_loop's Clean path → `mint_marker` →
    // `git_push`), not here: minting post-loop would seal a marker after
    // the push it is meant to authorize, and outside the section that
    // keeps it bound to the pushed `HEAD` (specs/harness.md § Verdict
    // Gate). See `ProductionReviewController::mint_marker`.
    Ok(outcome)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupReconcile {
    FastForward,
    AlreadyDone,
}

fn prepare_loop_root(
    runtime: &tokio::runtime::Runtime,
    workspace: &Path,
    root: &LoopWorkRoot,
    loom_cfg: &loom_driver::config::LoomTopConfig,
    startup_reconcile: StartupReconcile,
) -> anyhow::Result<()> {
    if matches!(startup_reconcile, StartupReconcile::FastForward) {
        let ff_git = GitClient::open_with_integration_branch(
            workspace,
            loom_cfg.integration_branch.clone(),
        )?;
        let ff_workspace = workspace.to_path_buf();
        runtime.block_on(async move {
            let outcome = ff_git
                .fast_forward_integration_to_origin()
                .await
                .with_context(|| {
                    format!(
                        "loom loop startup: integration fast-forward failed in {}",
                        ff_workspace.display(),
                    )
                })?;
            tracing::info!(
                outcome = ?outcome,
                workspace = %ff_workspace.display(),
                "loom loop startup: integration line reconciled with origin",
            );
            anyhow::Ok(())
        })?;
    }

    let Some(gc_molecule) = gc_molecule_for_root(root) else {
        return Ok(());
    };
    let gc_git = GitClient::open(workspace)?;
    let gc_workspace = workspace.to_path_buf();
    runtime.block_on(async move {
        let bd = BdClient::new();
        match gc_git.sweep_orphan_bead_clones(&bd, &gc_molecule).await {
            Ok(removed) if !removed.is_empty() => tracing::info!(
                count = removed.len(),
                workspace = %gc_workspace.display(),
                molecule = %gc_molecule.as_str(),
                "loom loop startup: reaped closed bead workspaces",
            ),
            Ok(_) => {}
            Err(error) => tracing::warn!(
                %error,
                workspace = %gc_workspace.display(),
                molecule = %gc_molecule.as_str(),
                "loom loop startup: orphan-clone sweep failed — continuing",
            ),
        }
    });
    Ok(())
}

fn gc_molecule_for_root(root: &LoopWorkRoot) -> Option<MoleculeId> {
    match &root.kind {
        LoopWorkRootKind::Epic => Some(MoleculeId::new(root.id.as_str())),
        LoopWorkRootKind::Task => root
            .bead
            .parent
            .as_ref()
            .map(|parent| MoleculeId::new(parent.as_str())),
    }
}

fn print_sequential_loop_summary(multi_root: bool, root: &LoopWorkRoot, summary: &LoopOutcome) {
    if multi_root {
        let prefix = format!("loom loop {}:", root.id);
        print_loop_summary(&prefix, summary);
    } else {
        print_loop_summary("loom loop:", summary);
    }
}

fn print_loop_summary(prefix: &str, summary: &LoopOutcome) {
    println!(
        "{prefix} processed {} bead(s), clarified {}, blocked {}, outer_iterations={}, gate={}",
        summary.beads_processed,
        summary.beads_clarified,
        summary.beads_blocked,
        summary.outer_iterations,
        gate_label(&summary.gate),
    );
}

fn print_parallel_loop_summary(
    multi_root: bool,
    root: &LoopWorkRoot,
    parallel_n: u32,
    outcome: &LoopOutcome,
) {
    if multi_root {
        println!(
            "loom loop {} --parallel {parallel_n}: processed {}, gate={}",
            root.id,
            outcome.beads_processed,
            gate_label(&outcome.gate),
        );
    } else {
        print_parallel_aggregate_summary(parallel_n, outcome);
    }
}

fn print_parallel_aggregate_summary(parallel_n: u32, outcome: &LoopOutcome) {
    println!(
        "loom loop --parallel {parallel_n}: processed {}, gate={}",
        outcome.beads_processed,
        gate_label(&outcome.gate),
    );
}

/// One-word render of a [`GateOutcome`] for the operator-facing summary
/// line. The structured variant lives in [`LoopOutcome::gate`] for
/// programmatic consumers; this is the human-friendly column.
fn gate_label(gate: &GateOutcome) -> &'static str {
    match gate {
        GateOutcome::Success(_) => "success",
        GateOutcome::Fail(_) => "fail",
        GateOutcome::NoGate { .. } => "no-gate",
    }
}

/// `UpdateOpts` for a parallel-mode `loom:clarify` / `loom:blocked`
/// self-report. Pairs `status=blocked` with the terminal label so
/// `bd ready` excludes the parked bead via its native status filter
/// (`specs/harness.md` § Labels), mirroring the serial
/// `apply_clarify_or_blocked` / `apply_blocked` paths. Without the
/// paired status the escalated bead stays ready and the next
/// `loom loop` re-dispatches it instead of parking for human resolution.
fn parallel_park_update(label: &str, notes: Option<String>) -> UpdateOpts {
    UpdateOpts {
        status: Some("blocked".to_string()),
        add_labels: vec![label.to_string()],
        notes,
        ..UpdateOpts::default()
    }
}

#[expect(clippy::too_many_arguments, reason = "fan-out wiring surface")]
async fn run_parallel_loop(
    workspace: PathBuf,
    label: SpecLabel,
    ready_parent: Option<BeadId>,
    parallel_n: u32,
    kind: AgentKind,
    selection: loom_driver::config::AgentSelection,
    direct_output_limits: loom_driver::agent::OutputLimits,
    shutdown_grace: Option<Duration>,
    manifest: Arc<ProfileImageManifest>,
    cli_profile: Option<ProfileName>,
    phase_default: ProfileName,
    style_rules: String,
    loom_cfg: loom_driver::config::LoomTopConfig,
    skills_cfg: loom_driver::config::SkillsConfig,
    observer_config: AgentObserversConfig,
    render_mode: loom_render::RenderMode,
    infra_policy: InfraRetryPolicy,
) -> anyhow::Result<LoopOutcome> {
    use loom_driver::bd::UpdateOpts;
    use loom_workflow::r#loop::AgentOutcome;

    let bd = BdClient::new();
    let git = GitClient::open_with_integration_branch(
        workspace.clone(),
        loom_cfg.integration_branch.clone(),
    )?
    .with_hook_timeout(loom_cfg.git_hook_timeout());
    let launcher_env = git.launcher_key_env()?;
    let logs_root = workspace.join(".loom/logs");
    let batch_limit = parallel_n as usize;
    let mut infra_budget = ParallelInfraBudget::new(infra_policy);
    let mut infra_retry_queue: VecDeque<Bead> = VecDeque::new();
    let mut infra_queue_loaded = false;
    let mut finished_ids: HashSet<BeadId> = HashSet::new();
    let mut processed = 0_u32;
    let mut clarified = 0_u32;
    let mut blocked = 0_u32;

    loop {
        let deferred_ids = infra_retry_queue
            .iter()
            .map(|bead| bead.id.clone())
            .collect::<Vec<_>>();
        let mut batch_beads = parallel_ready_batch(
            &bd,
            &label,
            ready_parent.as_ref(),
            batch_limit,
            &deferred_ids,
            &finished_ids,
        )
        .await?;
        if batch_beads.is_empty() {
            if !infra_queue_loaded {
                infra_retry_queue
                    .extend(load_parallel_infra_queue(&bd, &label, ready_parent.as_ref()).await?);
                infra_queue_loaded = true;
            }
            while batch_beads.len() < batch_limit {
                let Some(bead) = infra_retry_queue.pop_front() else {
                    break;
                };
                if finished_ids.contains(&bead.id) {
                    continue;
                }
                batch_beads.push(bead);
            }
        }
        if batch_beads.is_empty() {
            break;
        }

        clear_parallel_infra_state(&bd, &batch_beads).await?;
        let batch_by_id = batch_beads
            .iter()
            .map(|bead| (bead.id.clone(), bead.clone()))
            .collect::<HashMap<_, _>>();
        let logs_root_for_merge = logs_root.clone();
        let logs_root_for_spawn = logs_root.clone();
        let label_for_closure = label.clone();
        let workspace_for_closure = workspace.clone();
        let manifest_for_batch = Arc::clone(&manifest);
        let cli_profile_for_batch = cli_profile.clone();
        let phase_default_for_batch = phase_default.clone();
        let style_rules_for_batch = style_rules.clone();
        let loom_cfg_for_batch = loom_cfg.clone();
        let skills_cfg_for_batch = skills_cfg.clone();
        let observer_config_for_batch = observer_config.clone();
        let selection_for_batch = selection.clone();
        let launcher_env_for_batch = launcher_env.clone();
        let outcome = loom_workflow::r#loop::run_parallel_batch_with_logs(
            &git,
            &label,
            batch_beads,
            Some(&logs_root_for_merge),
            move |slot| {
                let manifest_inner = Arc::clone(&manifest_for_batch);
                let cli_profile_inner = cli_profile_for_batch.clone();
                let phase_default_inner = phase_default_for_batch.clone();
                let logs_root_inner = logs_root_for_spawn.clone();
                let label_inner = label_for_closure.clone();
                let style_rules_inner = style_rules_for_batch.clone();
                let workspace_inner = workspace_for_closure.clone();
                let loom_cfg_inner = loom_cfg_for_batch.clone();
                let skills_cfg_inner = skills_cfg_for_batch.clone();
                let observer_config_inner = observer_config_for_batch.clone();
                let selection_inner = selection_for_batch.clone();
                let launcher_env_inner = launcher_env_for_batch.clone();
                async move {
                    match dispatch_for_slot(
                        kind,
                        shutdown_grace,
                        slot,
                        &manifest_inner,
                        cli_profile_inner.as_ref(),
                        &phase_default_inner,
                        &logs_root_inner,
                        &label_inner,
                        &style_rules_inner,
                        &workspace_inner,
                        &loom_cfg_inner,
                        &skills_cfg_inner,
                        &observer_config_inner,
                        &selection_inner,
                        direct_output_limits,
                        launcher_env_inner,
                        render_mode,
                    )
                    .await
                    {
                        Ok(outcome) => outcome,
                        Err(e) => AgentOutcome::Failure {
                            error: format!("{e:#}"),
                        },
                    }
                }
            },
        )
        .await?;

        for result in outcome.results {
            match result {
                BatchResult::Merged { bead } => {
                    infra_budget.clear(&bead);
                    finished_ids.insert(bead);
                    processed = processed.saturating_add(1);
                }
                BatchResult::Conflict { bead, .. } => {
                    tracing::warn!(
                        bead = %bead,
                        "loom loop: integration conflict — marking for single retry; rerun loom loop to re-dispatch against the moved tip",
                    );
                    bd.update(
                        &bead,
                        UpdateOpts {
                            add_labels: vec![
                                loom_workflow::r#loop::CONFLICT_RETRY_LABEL.to_string(),
                            ],
                            ..UpdateOpts::default()
                        },
                    )
                    .await?;
                    infra_budget.clear(&bead);
                    finished_ids.insert(bead);
                    processed = processed.saturating_add(1);
                }
                BatchResult::AgentFailed { bead, .. } => {
                    infra_budget.clear(&bead);
                    finished_ids.insert(bead);
                    processed = processed.saturating_add(1);
                }
                BatchResult::AgentInfra { bead, failure } => {
                    let route = infra_budget.record(&bead, &failure);
                    match route {
                        ParallelInfraRoute::Retry { diagnostic } => {
                            tracing::warn!(
                                bead = %bead,
                                cause = %diagnostic.cause,
                                attempt = ?diagnostic.attempt,
                                "loom loop: infra failure queued for retry",
                            );
                            let retry_bead = batch_by_id.get(&bead).cloned().ok_or_else(|| {
                                anyhow::anyhow!("parallel infra result missing bead {bead}")
                            })?;
                            infra_retry_queue.push_back(retry_bead);
                        }
                        ParallelInfraRoute::Park { diagnostic } => {
                            bd.update(&bead, parallel_infra_update(&diagnostic)).await?;
                            finished_ids.insert(bead);
                            processed = processed.saturating_add(1);
                            blocked = blocked.saturating_add(1);
                        }
                    }
                }
                BatchResult::AgentBlocked { bead, reason } => {
                    let notes = if reason.is_empty() {
                        "agent-blocked".to_string()
                    } else {
                        format!("agent-blocked: {reason}")
                    };
                    bd.update(&bead, parallel_park_update("loom:blocked", Some(notes)))
                        .await?;
                    infra_budget.clear(&bead);
                    finished_ids.insert(bead);
                    processed = processed.saturating_add(1);
                    blocked = blocked.saturating_add(1);
                }
                BatchResult::AgentClarify { bead, question } => {
                    let notes = if question.is_empty() {
                        None
                    } else {
                        Some(question)
                    };
                    bd.update(&bead, parallel_park_update("loom:clarify", notes))
                        .await?;
                    infra_budget.clear(&bead);
                    finished_ids.insert(bead);
                    processed = processed.saturating_add(1);
                    clarified = clarified.saturating_add(1);
                }
            }
        }
    }

    Ok(LoopOutcome {
        beads_processed: processed,
        beads_clarified: clarified,
        beads_blocked: blocked,
        outer_iterations: 0,
        gate: GateOutcome::NoGate {
            beads_processed: processed,
            reason: if processed == 0 {
                NoGateReason::NoBeadsReady
            } else {
                NoGateReason::SelectionPartial
            },
        },
    })
}

#[derive(Debug)]
struct ParallelInfraBudget {
    attempts: HashMap<BeadId, u32>,
    max_attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParallelInfraRoute {
    Retry { diagnostic: InfraDiagnostic },
    Park { diagnostic: InfraDiagnostic },
}

impl ParallelInfraBudget {
    fn new(policy: InfraRetryPolicy) -> Self {
        Self {
            attempts: HashMap::new(),
            max_attempts: policy.max_attempts.max(1),
        }
    }

    fn record(&mut self, bead: &BeadId, failure: &BatchInfraFailure) -> ParallelInfraRoute {
        if !failure.is_retryable() {
            self.clear(bead);
            return ParallelInfraRoute::Park {
                diagnostic: failure.diagnostic(0, self.max_attempts),
            };
        }
        let attempt = self
            .attempts
            .get(bead)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        self.attempts.insert(bead.clone(), attempt);
        let diagnostic = failure.diagnostic(attempt, self.max_attempts);
        if attempt >= self.max_attempts {
            self.clear(bead);
            ParallelInfraRoute::Park { diagnostic }
        } else {
            ParallelInfraRoute::Retry { diagnostic }
        }
    }

    fn clear(&mut self, bead: &BeadId) {
        self.attempts.remove(bead);
    }
}

async fn parallel_ready_batch(
    bd: &BdClient,
    label: &SpecLabel,
    ready_parent: Option<&BeadId>,
    batch_limit: usize,
    deferred: &[BeadId],
    finished: &HashSet<BeadId>,
) -> anyhow::Result<Vec<Bead>> {
    let beads = bd
        .ready(loom_driver::bd::ReadyOpts {
            limit: None,
            label: ready_parent
                .is_none()
                .then(|| format!("spec:{}", label.as_str())),
            parent: ready_parent.cloned(),
            exclude_label: vec![],
        })
        .await?;
    let mut out = Vec::with_capacity(batch_limit);
    for bead in beads {
        if out.len() >= batch_limit {
            break;
        }
        if deferred.iter().any(|id| id == &bead.id) || finished.contains(&bead.id) {
            continue;
        }
        if bead.issue_type == "epic" {
            tracing::info!(
                bead = %bead.id,
                spec = %label,
                "loom loop: skipping epic-typed ready bead — workers dispatch leaves only",
            );
            continue;
        }
        out.push(bead);
    }
    Ok(out)
}

async fn load_parallel_infra_queue(
    bd: &BdClient,
    label: &SpecLabel,
    ready_parent: Option<&BeadId>,
) -> anyhow::Result<VecDeque<Bead>> {
    let beads = bd
        .list(ListOpts {
            status: Some("blocked".to_string()),
            label: ready_parent
                .is_none()
                .then(|| format!("spec:{}", label.as_str())),
            label_any: vec!["loom:infra".to_string()],
            parent: ready_parent.cloned(),
            ..ListOpts::default()
        })
        .await?;
    let mut queue = VecDeque::new();
    for bead in beads {
        if bead.issue_type == "epic" {
            tracing::info!(
                bead = %bead.id,
                spec = %label,
                "loom loop: skipping epic-typed infra bead — workers dispatch leaves only",
            );
            continue;
        }
        queue.push_back(bead);
    }
    Ok(queue)
}

async fn clear_parallel_infra_state(bd: &BdClient, beads: &[Bead]) -> anyhow::Result<()> {
    for bead in beads {
        if bead.labels.iter().any(loom_driver::bd::Label::is_infra) {
            bd.update(&bead.id, parallel_clear_infra_update()).await?;
        }
    }
    Ok(())
}

fn parallel_clear_infra_update() -> UpdateOpts {
    UpdateOpts {
        status: Some("open".to_string()),
        remove_labels: vec!["loom:infra".to_string()],
        ..UpdateOpts::default()
    }
}

fn parallel_infra_update(diagnostic: &InfraDiagnostic) -> UpdateOpts {
    let mut metadata = vec![
        ("loom.infra.cause".to_string(), diagnostic.cause.clone()),
        ("loom.infra.phase".to_string(), "loop".to_string()),
        (
            "loom.infra.class".to_string(),
            diagnostic.infra_class.clone(),
        ),
    ];
    if let Some(first_event_seen) = diagnostic.first_event_seen {
        metadata.push((
            "loom.infra.first_event_seen".to_string(),
            first_event_seen.to_string(),
        ));
    }
    if let Some(attempt) = diagnostic.attempt {
        metadata.push(("loom.infra.attempt".to_string(), attempt.to_string()));
    }
    if let Some(max_attempts) = diagnostic.max_attempts {
        metadata.push((
            "loom.infra.max_attempts".to_string(),
            max_attempts.to_string(),
        ));
    }
    UpdateOpts {
        status: Some("blocked".to_string()),
        add_labels: vec!["loom:infra".to_string()],
        notes: Some(parallel_diagnostic_notes(
            &diagnostic.cause,
            &diagnostic.error,
        )),
        set_metadata: metadata,
        ..UpdateOpts::default()
    }
}

fn parallel_diagnostic_notes(cause: &str, error: &str) -> String {
    if error.is_empty() {
        cause.to_string()
    } else {
        format!("{cause}: {error}")
    }
}

/// One slot's dispatch: build the per-bead [`SpawnConfig`] against the
/// slot's worktree and hand it to the same [`dispatch`] match the sequential
/// path uses. The pre-resolved [`AgentKind`] from `run_run` is threaded down
/// — this used to reload `LoomConfig` and re-resolve the backend per slot,
/// which let the sequential and parallel paths drift if the on-disk config
/// changed mid-run. A missing manifest entry surfaces as
/// [`ProfileError::UnknownProfile`] and sibling static diagnostics become
/// typed [`AgentOutcome`] variants so the caller can apply infra routing
/// without falling back to a silent default.
///
/// [`ProfileError::UnknownProfile`]: loom_driver::profile_manifest::ProfileError::UnknownProfile
#[expect(clippy::too_many_arguments, reason = "fan-out wiring surface")]
async fn dispatch_for_slot(
    kind: AgentKind,
    shutdown_grace: Option<Duration>,
    slot: loom_workflow::r#loop::WorktreeBead,
    manifest: &ProfileImageManifest,
    cli_profile: Option<&ProfileName>,
    phase_default: &ProfileName,
    logs_root: &Path,
    label: &SpecLabel,
    style_rules: &str,
    loom_workspace: &Path,
    loom_cfg: &loom_driver::config::LoomTopConfig,
    skills_cfg: &loom_driver::config::SkillsConfig,
    observer_config: &AgentObserversConfig,
    selection: &loom_driver::config::AgentSelection,
    direct_output_limits: loom_driver::agent::OutputLimits,
    launcher_env: Vec<(String, String)>,
    render_mode: loom_render::RenderMode,
) -> anyhow::Result<loom_workflow::r#loop::AgentOutcome> {
    use loom_driver::scratch::ScratchSession;
    use loom_workflow::r#loop::{
        AgentOutcome, LoopContextInputs, build_spawn_config_from_manifest, dolt_socket_mount,
        render_loop_prompt, sccache_mount,
    };
    use loom_workflow::skill::SkillPlan;

    let banner = format!("loom loop @ {}", slot.bead.id);
    let key = resolve_scratch_key(
        Phase::Loop,
        std::slice::from_ref(label),
        Some(&slot.bead.id),
    );
    let scratchpad_path = ScratchSession::scratchpad_path_for(&slot.worktree.path, &key);
    let scratch_dir = scratchpad_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("scratchpad path has no parent"))?;
    let bead_git = GitClient::open(&slot.worktree.path)?;
    let tracked_files = bead_git.tracked_files().await?;
    let skill_profile =
        loom_workflow::r#loop::resolve_profile(&slot.bead.labels, cli_profile, phase_default);
    let skill_plan = SkillPlan::resolve(
        &slot.worktree.path,
        &tracked_files,
        Phase::Loop.as_str(),
        &skill_profile,
        kind,
        skills_cfg,
    )?;
    let skill_session = skill_plan.materialize(scratch_dir, &slot.worktree.path)?;
    let initial_prompt = render_loop_prompt(LoopContextInputs {
        label: label.clone(),
        spec_path: format!("specs/{}.md", label.as_str()),
        pinned_context: String::new(),
        companion_paths: vec![],
        molecule_id: None,
        issue_id: slot.bead.id.clone(),
        title: slot.bead.title.clone(),
        description: slot.bead.description.clone(),
        previous_failure: None,
        workspace_recovery: None,
        review_notes: None,
        attempt: 0,
        scratchpad_path: scratchpad_path.to_string_lossy().into_owned(),
        style_rules: style_rules.to_string(),
        skill_index: skill_session.skill_index,
    })?;
    let scratch = ScratchSession::open(&slot.worktree.path, &key, &initial_prompt, &banner)?;
    let mut mounts: Vec<_> = dolt_socket_mount(loom_workspace).into_iter().collect();
    if let Some(spec) = sccache_mount(loom_cfg)? {
        mounts.push(spec);
    }
    let extra_env = loom_cfg.container_sccache_env();
    let mut spawn_config = match build_spawn_config_from_manifest(
        manifest,
        &slot.bead,
        cli_profile,
        phase_default,
        kind,
        slot.worktree.path.clone(),
        initial_prompt,
        scratch.path().to_path_buf(),
        extra_env,
        vec![],
        mounts,
        launcher_env,
    ) {
        Ok(config) => config,
        Err(ProfileError::UnknownProfile { name, .. }) => {
            drop(scratch);
            return Ok(AgentOutcome::UnknownProfile {
                error: format_unknown_profile_error(&name, manifest),
            });
        }
        Err(ProfileError::UnknownRuntimeForProfile {
            profile,
            runtime,
            declared_runtimes,
            ..
        }) => {
            drop(scratch);
            return Ok(AgentOutcome::UnknownRuntimeForProfile {
                error: format_unknown_runtime_for_profile_error(
                    &profile,
                    runtime,
                    &declared_runtimes,
                ),
            });
        }
        Err(e @ ProfileError::InvalidSpawnConfig { .. })
        | Err(e @ ProfileError::RuntimeMetadataMismatch { .. }) => {
            drop(scratch);
            return Ok(AgentOutcome::StaticInfra {
                cause: loom_workflow::r#loop::INVALID_SPAWN_CONFIG_CAUSE.to_string(),
                error: e.to_string(),
            });
        }
        Err(e) => {
            drop(scratch);
            return Err(e.into());
        }
    };
    let skill_session = skill_plan.materialize(scratch.path(), &slot.worktree.path)?;
    spawn_config.skills = Some(skill_session.registered);

    let sink = match open_bead_sink_with_renderer(
        logs_root,
        label,
        &slot.bead.id,
        render_mode,
        &slot.worktree.path,
        true,
    ) {
        Ok(sink) => sink,
        Err(err) => {
            drop(scratch);
            return Ok(AgentOutcome::InfraPreflight {
                error: format!("open log sink: {err:#}"),
            });
        }
    };
    let mut output = String::new();
    selection.apply_to_spawn_config(&mut spawn_config, direct_output_limits);
    let envelope_builder = build_envelope_builder(slot.bead.id.clone());
    let result = dispatch_classified(
        kind,
        spawn_config,
        shutdown_grace,
        Some(sink),
        Some(&mut output),
        Some(envelope_builder),
        observer_config.clone(),
    )
    .await;
    drop(scratch);
    let marker = parse_exit_signal(&output);
    Ok(classify_session(result, marker))
}

/// Backend-agnostic dispatcher. The match is the only place in the binary
/// that knows the concrete backend types — the agent driver is monomorphized
/// once per arm at compile time, so the workflow modules never see them.
///
/// `sink` is consumed: ownership crosses into the agent driver, which finishes
/// it before returning. Phase entry points open the sink before invoking
/// dispatch so the on-disk JSONL and the workflow outcome share one code
/// path. Pass `None` from sites that have not yet been wired.
///
/// `shutdown_grace` is the configured `[claude] post_result_grace_secs`
/// resolved from [`AgentSelection::claude_settings`]. It is patched into
/// `spawn.shutdown_grace` only when the dispatched backend is claude and
/// the field is not already set — pi exits naturally on `agent_end`, and
/// upstream callers that pre-populate the field (tests, future per-bead
/// overrides) are honored as-is.
async fn dispatch(
    kind: AgentKind,
    spawn: SpawnConfig,
    shutdown_grace: Option<Duration>,
    sink: Option<LogSink>,
    text_capture: Option<&mut String>,
) -> Result<SessionOutcome, ProtocolError> {
    dispatch_with_envelope(kind, spawn, shutdown_grace, sink, text_capture, None).await
}

async fn dispatch_with_envelope(
    kind: AgentKind,
    mut spawn: SpawnConfig,
    shutdown_grace: Option<Duration>,
    sink: Option<LogSink>,
    text_capture: Option<&mut String>,
    envelope_builder: Option<loom_events::EnvelopeBuilder>,
) -> Result<SessionOutcome, ProtocolError> {
    if matches!(kind, AgentKind::Claude) && spawn.shutdown_grace.is_none() {
        spawn.shutdown_grace = shutdown_grace;
    }
    if spawn.handshake_timeout.is_none()
        && let Some(d) = duration_env_ms("LOOM_HANDSHAKE_TIMEOUT_MS")
    {
        spawn.handshake_timeout = Some(d);
    }
    if spawn.stall_warn_interval.is_none()
        && let Some(d) = duration_env_ms("LOOM_STALL_WARN_MS")
    {
        spawn.stall_warn_interval = Some(d);
    }
    let result = match kind {
        AgentKind::Pi => {
            run_agent_classified::<PiBackend>(&spawn, sink, None, text_capture, envelope_builder)
                .await
        }
        AgentKind::Claude => {
            run_agent_classified::<ClaudeBackend>(
                &spawn,
                sink,
                None,
                text_capture,
                envelope_builder,
            )
            .await
        }
        AgentKind::Direct => {
            run_agent_classified::<DirectBackend>(
                &spawn,
                sink,
                None,
                text_capture,
                envelope_builder,
            )
            .await
        }
    };
    session_result_to_legacy_result(result)
}

fn session_result_to_legacy_result(result: SessionResult) -> Result<SessionOutcome, ProtocolError> {
    match result {
        SessionResult::Complete(outcome) => Ok(outcome),
        SessionResult::PreflightFailed { error }
        | SessionResult::MidSessionFailed { error }
        | SessionResult::StaticInfra { error, .. } => {
            Err(ProtocolError::Io(std::io::Error::other(error)))
        }
        SessionResult::ObserverAbort { reason } => Err(ProtocolError::Io(std::io::Error::other(
            format!("Session aborted by observer: {reason}"),
        ))),
    }
}

/// Same as [`dispatch`] but preserves the preflight-vs-mid-session split via
/// [`SessionResult`]. The `loom loop` driver consumes this so the verdict gate
/// can route preflight failures to `infra-preflight` immediately and grant
/// mid-session failures one driver-memory retry per `loom loop`.
///
/// `observer_config` is the resolved `LoomConfig::agent` block. When at
/// least one sub-observer is `enabled = true`, a
/// [`loom_workflow::DefaultObserverChain`] is composed from the two
/// `llm` observers and passed to `run_agent_classified` as the
/// session's `observer` arg — wiring the spec's safety nets
/// (`specs/llm.md` § Agent-Loop Observers) into every Pi/Claude
/// session by default.
async fn dispatch_classified(
    kind: AgentKind,
    mut spawn: SpawnConfig,
    shutdown_grace: Option<Duration>,
    sink: Option<LogSink>,
    text_capture: Option<&mut String>,
    envelope_builder: Option<loom_events::EnvelopeBuilder>,
    observer_config: AgentObserversConfig,
) -> SessionResult {
    if matches!(kind, AgentKind::Claude) && spawn.shutdown_grace.is_none() {
        spawn.shutdown_grace = shutdown_grace;
    }
    if spawn.handshake_timeout.is_none()
        && let Some(d) = duration_env_ms("LOOM_HANDSHAKE_TIMEOUT_MS")
    {
        spawn.handshake_timeout = Some(d);
    }
    if spawn.stall_warn_interval.is_none()
        && let Some(d) = duration_env_ms("LOOM_STALL_WARN_MS")
    {
        spawn.stall_warn_interval = Some(d);
    }
    let mut observer_chain = DefaultObserverChain::from_config(&observer_config);
    let observer = observer_chain.as_mut();
    match kind {
        AgentKind::Pi => {
            run_agent_classified::<PiBackend>(
                &spawn,
                sink,
                observer,
                text_capture,
                envelope_builder,
            )
            .await
        }
        AgentKind::Claude => {
            run_agent_classified::<ClaudeBackend>(
                &spawn,
                sink,
                observer,
                text_capture,
                envelope_builder,
            )
            .await
        }
        AgentKind::Direct => {
            run_agent_classified::<DirectBackend>(
                &spawn,
                sink,
                observer,
                text_capture,
                envelope_builder,
            )
            .await
        }
    }
}

/// Build a per-spawn [`loom_events::EnvelopeBuilder`] so every event the
/// session emits carries a stable session id, the live bead id,
/// monotonic `seq`, and real wall-clock `ts_ms`. The workflow layer
/// joins each `ParsedAgentEvent` with the builder's output via
/// `AgentEvent::from_parsed` (RS-12). `molecule_id` is omitted and
/// `iteration` starts at 0 until the driver threads them through.
fn build_envelope_builder(bead_id: BeadId) -> loom_events::EnvelopeBuilder {
    let clock = SystemClock::new();
    let started_ms = clock
        .wall_now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    let session_id = loom_events::identifier::SessionId::new(format!(
        "{}-{}-{started_ms}",
        bead_id.as_str().replace('.', "-"),
        std::process::id(),
    ));
    loom_events::EnvelopeBuilder::new(
        loom_events::SessionScope::bead(session_id, bead_id, None, 0),
        loom_events::Source::Agent,
        move || {
            clock
                .wall_now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_millis() as i64)
        },
    )
}

fn build_phase_envelope_builder(phase: Phase) -> loom_events::EnvelopeBuilder {
    let clock = SystemClock::new();
    let started_ms = clock
        .wall_now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    let phase_name = phase.as_str();
    let session_id = loom_events::identifier::SessionId::new(format!(
        "{phase_name}-{}-{started_ms}",
        std::process::id(),
    ));
    loom_events::EnvelopeBuilder::new(
        loom_events::SessionScope::phase(session_id, None),
        loom_events::Source::Agent,
        move || {
            clock
                .wall_now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_millis() as i64)
        },
    )
}

struct GateMintProgress {
    sink: LogSink,
    builder: loom_events::EnvelopeBuilder,
}

impl GateMintProgress {
    fn open(
        logs_root: &Path,
        workspace: &Path,
        render_mode: loom_render::RenderMode,
        renderer_id: &BeadId,
        when: std::time::SystemTime,
    ) -> anyhow::Result<Self> {
        let renderer = build_stdout_renderer(render_mode, renderer_id, workspace, false);
        let sink = LogSink::open_phase_at(
            logs_root,
            &SpecLabel::new("gate"),
            "mint",
            Some(renderer),
            when,
        )?;
        Ok(Self {
            sink,
            builder: build_gate_mint_envelope_builder(when),
        })
    }

    fn emit(
        &mut self,
        driver_kind: loom_events::DriverKind,
        summary: impl Into<String>,
        payload: serde_json::Value,
    ) -> anyhow::Result<()> {
        let event = loom_events::AgentEvent::from_driver_event(
            loom_events::DriverEventPayload::new(driver_kind, summary.into(), payload),
            self.builder.build_with_source(loom_events::Source::Driver),
        );
        self.sink.emit(&event)?;
        Ok(())
    }

    fn log_path(&self) -> &Path {
        self.sink.log_path()
    }

    fn finish(&mut self, summary: &loom_workflow::mint::MintSummary) -> anyhow::Result<()> {
        let outcome = if mint_summary_exit_code(summary) == 0 {
            loom_driver::logging::BeadOutcome::Done
        } else {
            loom_driver::logging::BeadOutcome::Failed
        };
        self.sink.finish(outcome)?;
        Ok(())
    }
}

fn build_gate_mint_envelope_builder(when: std::time::SystemTime) -> loom_events::EnvelopeBuilder {
    let clock = SystemClock::new();
    let started_ms = when
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    let session_id = loom_events::identifier::SessionId::new(format!(
        "gate-mint-{}-{started_ms}",
        std::process::id(),
    ));
    loom_events::EnvelopeBuilder::new(
        loom_events::SessionScope::phase(session_id, None),
        loom_events::Source::Driver,
        move || {
            clock
                .wall_now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_millis() as i64)
        },
    )
}

fn default_live_render_mode() -> loom_render::RenderMode {
    let tty = loom_render::in_place::stdout_supports_indicator();
    let no_color = std::env::var_os("NO_COLOR").is_some();
    loom_render::RenderMode::select(tty, no_color, false, false, false)
}

fn finding_status_action_wire(action: FindingStatusAction) -> &'static str {
    match action {
        FindingStatusAction::Reported => "reported",
        FindingStatusAction::Minted => "minted",
        FindingStatusAction::SkippedLive => "skipped-live",
        FindingStatusAction::Suppressed => "suppressed",
        FindingStatusAction::StaleCandidate => "stale-candidate",
        FindingStatusAction::PartialStaleCandidate => "partial-stale-candidate",
        FindingStatusAction::Refused => "refused",
    }
}

fn mint_summary_counts(summary: &loom_workflow::mint::MintSummary) -> serde_json::Value {
    serde_json::json!({
        "minted": summary.minted,
        "planned": summary.planned,
        "would_mint": summary.would_mint,
        "promoted_deferred": summary.promoted_deferred,
        "would_promote_deferred": summary.would_promote_deferred,
        "skipped": summary.skipped,
        "skipped_filter": summary.skipped_filter,
        "suppressed": summary.suppressed,
        "ineffective_suppressions": summary.ineffective_suppressions,
        "stale_candidates": summary.stale_candidates,
        "partial_stale_candidates": summary.partial_stale_candidates,
        "refused": summary.refused,
        "errors": summary.errors,
        "findings_across_minted": summary.findings_across_minted,
        "specs_across_minted": summary.specs_across_minted,
        "active_epic": summary.active_epic.as_ref().map(ToString::to_string),
        "exit_code": mint_summary_exit_code(summary),
    })
}

fn mint_batch_payload(outcome: &BatchOutcome) -> serde_json::Value {
    match outcome {
        BatchOutcome::Minted {
            fingerprint,
            bead_id,
            lead_spec,
            findings_count,
        } => serde_json::json!({
            "stage": "mint",
            "action": outcome.kind(),
            "fingerprint": fingerprint,
            "bead_id": bead_id,
            "lead_spec": lead_spec,
            "findings_count": findings_count,
        }),
        BatchOutcome::Planned {
            fingerprint,
            lead_spec,
            findings_count,
        }
        | BatchOutcome::WouldMint {
            fingerprint,
            lead_spec,
            findings_count,
        } => serde_json::json!({
            "stage": "mint",
            "action": outcome.kind(),
            "fingerprint": fingerprint,
            "lead_spec": lead_spec,
            "findings_count": findings_count,
        }),
        BatchOutcome::SkippedDedup {
            fingerprint,
            existing_bead,
            findings_count,
        }
        | BatchOutcome::SkippedClosed {
            fingerprint,
            existing_bead,
            findings_count,
        } => serde_json::json!({
            "stage": "mint",
            "action": outcome.kind(),
            "fingerprint": fingerprint,
            "existing_bead": existing_bead,
            "findings_count": findings_count,
        }),
        BatchOutcome::SkippedFilter {
            fingerprint,
            lead_spec,
            requested,
            findings_count,
        } => serde_json::json!({
            "stage": "mint",
            "action": outcome.kind(),
            "fingerprint": fingerprint,
            "lead_spec": lead_spec,
            "requested_spec": requested,
            "findings_count": findings_count,
        }),
        BatchOutcome::Refused {
            fingerprint,
            reason,
        } => serde_json::json!({
            "stage": "mint",
            "action": outcome.kind(),
            "fingerprint": fingerprint,
            "reason": reason,
        }),
        BatchOutcome::PromotedDeferred {
            bead_id,
            findings_count,
        }
        | BatchOutcome::WouldPromoteDeferred {
            bead_id,
            findings_count,
        } => serde_json::json!({
            "stage": "mint",
            "action": outcome.kind(),
            "bead_id": bead_id,
            "findings_count": findings_count,
        }),
        BatchOutcome::StaleCandidate {
            bead_id,
            absent_hashes,
        } => serde_json::json!({
            "stage": "stale-reporting",
            "action": outcome.kind(),
            "bead_id": bead_id,
            "absent_hashes": absent_hashes,
        }),
        BatchOutcome::PartialStaleCandidate {
            bead_id,
            current_hashes,
            absent_hashes,
        } => serde_json::json!({
            "stage": "stale-reporting",
            "action": outcome.kind(),
            "bead_id": bead_id,
            "current_hashes": current_hashes,
            "absent_hashes": absent_hashes,
        }),
        BatchOutcome::Errored {
            fingerprint,
            message,
        } => serde_json::json!({
            "stage": "mint",
            "action": outcome.kind(),
            "fingerprint": fingerprint,
            "message": message,
            "severity": "warning",
        }),
    }
}

fn mint_batch_summary(outcome: &BatchOutcome) -> String {
    match outcome {
        BatchOutcome::Minted {
            fingerprint,
            bead_id,
            ..
        } => format!("minting decision minted {fingerprint} → {bead_id}"),
        BatchOutcome::Planned { fingerprint, .. } => {
            format!("minting decision planned {fingerprint}")
        }
        BatchOutcome::WouldMint { fingerprint, .. } => {
            format!("minting decision would mint {fingerprint}")
        }
        BatchOutcome::SkippedDedup {
            fingerprint,
            existing_bead,
            ..
        } => format!("minting decision skipped {fingerprint}; live {existing_bead}"),
        BatchOutcome::SkippedFilter {
            fingerprint,
            requested,
            ..
        } => format!("minting decision skipped {fingerprint}; outside spec:{requested}"),
        BatchOutcome::Refused { fingerprint, .. } => {
            format!("minting decision refused {fingerprint}")
        }
        BatchOutcome::PromotedDeferred { bead_id, .. } => {
            format!("minting decision promoted deferred {bead_id}")
        }
        BatchOutcome::WouldPromoteDeferred { bead_id, .. } => {
            format!("minting decision would promote deferred {bead_id}")
        }
        BatchOutcome::SkippedClosed {
            fingerprint,
            existing_bead,
            ..
        } => format!("minting decision skipped {fingerprint}; closed {existing_bead}"),
        BatchOutcome::StaleCandidate { bead_id, .. } => {
            format!("stale candidate reported {bead_id}")
        }
        BatchOutcome::PartialStaleCandidate { bead_id, .. } => {
            format!("partial stale candidate reported {bead_id}")
        }
        BatchOutcome::Errored { fingerprint, .. } => {
            format!("minting decision errored {fingerprint}")
        }
    }
}

fn emit_mint_summary_events(
    progress: &mut GateMintProgress,
    summary: &loom_workflow::mint::MintSummary,
) -> anyhow::Result<()> {
    for status in &summary.statuses {
        let action = finding_status_action_wire(status.action);
        progress.emit(
            loom_events::DriverKind::GateRunLane,
            format!("finding status {action}: {}", status.hash),
            serde_json::json!({
                "stage": "mint",
                "action": "finding-status",
                "status": status,
            }),
        )?;
    }
    for outcome in &summary.batches {
        progress.emit(
            loom_events::DriverKind::GateRunLane,
            mint_batch_summary(outcome),
            mint_batch_payload(outcome),
        )?;
    }
    progress.emit(
        loom_events::DriverKind::GateRunEnd,
        format!(
            "mint tree run finished: minted {} batches, skipped {}, refused {}, errors {}",
            summary.minted, summary.skipped, summary.refused, summary.errors,
        ),
        serde_json::json!({
            "gate_phase": "mint",
            "scope": "tree",
            "stage": "summary",
            "counts": mint_summary_counts(summary),
        }),
    )?;
    Ok(())
}

/// Test seam: read a millisecond budget from `name` if set. Production
/// runs leave the env vars unset and SpawnConfig falls back to the
/// constants in `loom_driver::agent` (30s handshake / 60s stall warn).
fn duration_env_ms(name: &str) -> Option<Duration> {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_millis)
}

/// Resolve the configured shutdown grace from the active agent selection.
/// Pi sessions return `None` because pi exits naturally on `agent_end`;
/// claude sessions return the parsed `[claude] post_result_grace_secs`.
fn resolve_shutdown_grace(selection: &loom_driver::config::AgentSelection) -> Option<Duration> {
    selection
        .claude_settings
        .as_ref()
        .map(|s| Duration::from_secs(u64::from(s.post_result_grace_secs)))
}

/// Open the per-bead JSONL sink at the path the spec promises and
/// attach the resolved renderer so one event stream drives disk and stdout.
fn open_bead_sink_with_renderer(
    logs_root: &Path,
    label: &SpecLabel,
    bead_id: &BeadId,
    render_mode: loom_render::RenderMode,
    workspace: &Path,
    parallel: bool,
) -> Result<LogSink, ProtocolError> {
    let renderer = build_stdout_renderer(render_mode, bead_id, workspace, parallel);
    LogSink::open_in_at(
        logs_root,
        label,
        bead_id,
        Some(renderer),
        SystemClock::new().wall_now(),
    )
    .map_err(|e| ProtocolError::Io(std::io::Error::other(e.to_string())))
}

fn open_todo_sink_with_renderer(
    logs_root: &Path,
    workspace: &Path,
    render_mode: loom_render::RenderMode,
    renderer_id: &BeadId,
) -> Result<LogSink, ProtocolError> {
    open_todo_sink_with_writer(
        logs_root,
        workspace,
        render_mode,
        renderer_id,
        SystemClock::new().wall_now(),
        Box::new(std::io::stdout()),
    )
}

fn open_todo_sink_with_writer(
    logs_root: &Path,
    workspace: &Path,
    render_mode: loom_render::RenderMode,
    renderer_id: &BeadId,
    when: std::time::SystemTime,
    out: Box<dyn std::io::Write + Send>,
) -> Result<LogSink, ProtocolError> {
    let renderer = build_renderer_with_writer(render_mode, renderer_id, workspace, false, out);
    LogSink::open_phase_at(
        logs_root,
        &SpecLabel::new("todo"),
        "todo",
        Some(renderer),
        when,
    )
    .map_err(|e| ProtocolError::Io(std::io::Error::other(e.to_string())))
}

fn todo_renderer_id(spawn_cfg: &SpawnConfig) -> Result<BeadId, ProtocolError> {
    let name = spawn_cfg
        .scratch_dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| {
            ProtocolError::Io(std::io::Error::other(
                "todo scratch directory has no valid file name",
            ))
        })?;
    BeadId::new(name).map_err(|e| ProtocolError::Io(std::io::Error::other(e.to_string())))
}

/// Resolve a [`loom_render::RenderMode`] from the CLI flag tuple and
/// the runtime TTY / `NO_COLOR` environment. Spec table:
/// `--raw` > `--json` > `--plain` or non-TTY or `NO_COLOR` > Pretty.
fn resolve_render_mode(flags: RenderFlags) -> loom_render::RenderMode {
    let tty = loom_render::in_place::stdout_supports_indicator();
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let base = loom_render::RenderMode::select(tty, no_color, flags.plain, flags.json, flags.raw);
    if flags.verbose {
        match base {
            loom_render::RenderMode::Pretty => loom_render::RenderMode::Verbose,
            loom_render::RenderMode::Plain => loom_render::RenderMode::VerbosePlain,
            other => other,
        }
    } else {
        base
    }
}

/// Build the right [`loom_render::Renderer`] for the resolved mode and
/// wrap it in a `Box<dyn Renderer>` so the sink can hold any concrete
/// impl. The writer is `io::stdout()` so the user sees the rendered
/// stream; each per-bead renderer carries its own handle.
fn build_stdout_renderer(
    mode: loom_render::RenderMode,
    bead_id: &BeadId,
    workspace: &Path,
    parallel: bool,
) -> Box<dyn loom_render::Renderer> {
    build_renderer_with_writer(
        mode,
        bead_id,
        workspace,
        parallel,
        Box::new(std::io::stdout()),
    )
}

fn build_renderer_with_writer(
    mode: loom_render::RenderMode,
    bead_id: &BeadId,
    workspace: &Path,
    parallel: bool,
    out: Box<dyn std::io::Write + Send>,
) -> Box<dyn loom_render::Renderer> {
    let osc8_supported = loom_render::osc8::supports_osc8(
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
    );
    let osc8 = if osc8_supported {
        loom_render::tool_body::Osc8Context::enabled(workspace.to_path_buf())
    } else {
        loom_render::tool_body::Osc8Context::disabled().with_cwd(workspace.to_path_buf())
    };
    match mode {
        loom_render::RenderMode::Verbose | loom_render::RenderMode::VerbosePlain => {
            let color = matches!(mode, loom_render::RenderMode::Verbose) && !parallel;
            Box::new(
                loom_render::TerminalRenderer::new(out, mode, bead_id.clone(), parallel, color)
                    .with_osc8(osc8),
            )
        }
        loom_render::RenderMode::Pretty | loom_render::RenderMode::Default => Box::new(
            loom_render::TerminalRenderer::new(
                out,
                loom_render::RenderMode::Default,
                bead_id.clone(),
                parallel,
                true,
            )
            .with_osc8(osc8),
        ),
        loom_render::RenderMode::Plain => Box::new(loom_render::TerminalRenderer::new(
            out,
            loom_render::RenderMode::Default,
            bead_id.clone(),
            parallel,
            false,
        )),
        loom_render::RenderMode::Json => Box::new(loom_render::JsonRenderer::new(out)),
        loom_render::RenderMode::Raw => Box::new(loom_render::RawRenderer::new(out)),
    }
}

/// Resolve `phase`'s [`AgentKind`] honoring the global `--agent` override.
/// CLI override wins over `[phase.<phase>] agent.backend` and
/// `[phase.default] agent.backend`. Returns the full [`AgentSelection`] so
/// callers retain access to profile / provider / model / claude_settings.
fn resolved_agent_for(
    config: &LoomConfig,
    agent_override: Option<AgentKind>,
    phase: Phase,
) -> anyhow::Result<loom_driver::config::AgentSelection> {
    let mut selection = config.agent_for(phase)?;
    if let Some(kind) = agent_override {
        selection.kind = kind;
        selection.claude_settings = match kind {
            AgentKind::Claude => Some(loom_driver::config::ClaudeSettings {
                denied_tools: config.security.denied_tools.clone(),
                post_result_grace_secs: config.claude.post_result_grace_secs,
            }),
            AgentKind::Pi | AgentKind::Direct => None,
        };
    }
    Ok(selection)
}

struct ReviewOpts {
    bead: Option<String>,
    diff: Option<String>,
    tree: bool,
    /// Which lane(s) of the review to run — `Both` for `loom gate review`,
    /// `Judge`/`Rubric` for the focused single-lane re-runs surfaced by
    /// `loom gate judge` / `loom gate rubric`.
    lane: ReviewLane,
}

fn run_review(
    workspace: &Path,
    spec: Option<String>,
    agent_override: Option<AgentKind>,
    opts: ReviewOpts,
) -> anyhow::Result<()> {
    let manifest = Arc::new(ProfileImageManifest::from_env()?);
    let label = resolve_spec_label(workspace, spec)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let work_root_guard =
        acquire_review_work_root_lock(workspace, &label, opts.bead.as_deref(), &runtime)?;

    let config = LoomConfig::load(LoomConfig::resolve_path(workspace))?;
    let selection = resolved_agent_for(&config, agent_override, Phase::Review)?;
    let phase_default = selection.profile.clone();
    let kind = selection.kind;
    let shutdown_grace = resolve_shutdown_grace(&selection);
    let direct_output_limits = config.direct_output_limits();

    let loom_bin = current_loom_bin()?;
    let state = std::sync::Arc::new(CacheDb::open(workspace.join(".loom/cache.db"))?);
    let workspace_buf = workspace.to_path_buf();
    let logs_root = workspace.join(".loom/logs");
    let label_for_sink = label.clone();
    // Pin one phase timestamp so the verdict gate's `push_gate_*`
    // driver events and the reviewer agent's events land in the same
    // JSONL log file. Both writers compute the path from
    // `(logs_root, label, "review", phase_when)`. When invoked by
    // `loom loop`'s molecule-completion handoff, the parent pins
    // `phase_when` and passes it via `LOOM_REVIEW_PHASE_WHEN_MILLIS`
    // so both sides resolve the same log path.
    let phase_when = phase_when_from_env().unwrap_or_else(|| SystemClock::new().wall_now());
    let emit_stdout = std::env::var_os(REVIEW_EMIT_STDOUT_ENV).is_some();
    let logs_root_for_spawn = logs_root.clone();
    let style_rules_for_review = config.style_rules.clone();
    let integration_branch_for_review = config.loom.integration_branch.clone();
    let hook_timeout_for_review = config.loom.git_hook_timeout();
    let suppressions_for_review = config.suppress.clone();
    let skills_cfg_for_review = config.skills.clone();
    let dispatch_scope = if opts.tree {
        DispatchScope::Tree
    } else {
        DispatchScope::PerBead
    };
    let captured_review_stdout = Arc::new(Mutex::new(String::new()));
    let stdout_capture_for_spawn = Arc::clone(&captured_review_stdout);
    let result = runtime.block_on(async move {
        let bd = BdClient::new();
        let mut controller = ProductionReviewController::new(
            bd,
            label.clone(),
            loom_bin,
            workspace_buf,
            state,
            manifest,
            phase_default,
            move |spawn_cfg: SpawnConfig| {
                let logs_root = logs_root_for_spawn.clone();
                let label = label_for_sink.clone();
                let stdout_capture = Arc::clone(&stdout_capture_for_spawn);
                let selection = selection.clone();
                async move {
                    let sink =
                        LogSink::open_phase_at(&logs_root, &label, "review", None, phase_when)
                            .map_err(|e| ProtocolError::Io(std::io::Error::other(e.to_string())))?;
                    let mut output = String::new();
                    let mut spawn_cfg = spawn_cfg;
                    selection.apply_to_spawn_config(&mut spawn_cfg, direct_output_limits);
                    let outcome = dispatch(
                        kind,
                        spawn_cfg,
                        shutdown_grace,
                        Some(sink),
                        Some(&mut output),
                    )
                    .await?;
                    let marker = parse_exit_signal(&output);
                    *stdout_capture.lock().map_err(|_| {
                        ProtocolError::Io(std::io::Error::other("review stdout capture poisoned"))
                    })? = output.clone();
                    Ok((outcome, marker, output))
                }
            },
        );
        if let Some(guard) = work_root_guard {
            controller = controller.with_handoff_lock(guard);
        }
        let mut controller = controller
            .with_phase_log(logs_root, phase_when)
            .with_agent_runtime(kind)
            .with_style_rules(style_rules_for_review)
            .with_integration_branch(integration_branch_for_review)
            .with_hook_timeout(hook_timeout_for_review)
            .with_push_range(opts.diff.clone())
            .with_lane(opts.lane)
            .with_dispatch_scope(dispatch_scope)
            .with_suppressions(suppressions_for_review)
            .with_skills_config(skills_cfg_for_review);
        run_review_loop(&mut controller, IterationCap::default()).await
    })?;
    let review_stdout = captured_review_stdout
        .lock()
        .map_err(|_| anyhow::anyhow!("review stdout capture poisoned"))?
        .clone();
    emit_review_finding_statuses(&review_stdout, dispatch_scope, &config.suppress)?;
    if emit_stdout {
        print!("{review_stdout}");
    } else {
        println!("loom review: {result:?}");
    }
    Ok(())
}

fn emit_review_finding_statuses(
    review_stdout: &str,
    dispatch_scope: DispatchScope,
    suppressions: &[loom_driver::config::SuppressionConfig],
) -> anyhow::Result<()> {
    for record in review_finding_status_records(review_stdout, dispatch_scope, suppressions) {
        println!("{}", record.render()?);
    }
    Ok(())
}

fn review_finding_status_records(
    review_stdout: &str,
    dispatch_scope: DispatchScope,
    suppressions: &[loom_driver::config::SuppressionConfig],
) -> Vec<FindingStatusRecord> {
    let walk = WalkOutput::from_stdout(review_stdout, dispatch_scope, &AcceptAllFindingValidator);
    walk.findings()
        .iter()
        .map(|finding| {
            let action = if suppression_matches_finding(suppressions, finding) {
                FindingStatusAction::Suppressed
            } else {
                FindingStatusAction::Reported
            };
            FindingStatusRecord::new(finding, action)
        })
        .collect()
}

fn suppression_matches_finding(
    suppressions: &[loom_driver::config::SuppressionConfig],
    finding: &loom_workflow::review::Finding,
) -> bool {
    let id = finding.id();
    let hash = finding.hash();
    suppressions.iter().any(|entry| {
        entry.id.as_deref() == Some(id.as_str()) || entry.hash.as_deref() == Some(hash.as_str())
    })
}

/// Parse a parent-pinned `phase_when` from
/// [`REVIEW_PHASE_WHEN_ENV`] (set by `loom loop`'s `exec_review` so the
/// child's JSONL log lands at the same `phase_log_path` the parent
/// threads into `HandoffEvidence`). Returns `None` when the env var is
/// unset or unparseable so direct human callers fall through to
/// `SystemClock::wall_now()`.
fn phase_when_from_env() -> Option<std::time::SystemTime> {
    let raw = std::env::var(REVIEW_PHASE_WHEN_ENV).ok()?;
    let millis: u64 = raw.parse().ok()?;
    Some(std::time::UNIX_EPOCH + Duration::from_millis(millis))
}

fn run_inbox(
    workspace: &Path,
    args: InboxArgs,
    agent_override: Option<AgentKind>,
) -> anyhow::Result<()> {
    match args.action {
        None => print_inbox_help(),
        Some(InboxAction::List(list)) => {
            run_inbox_list(workspace, merge_inbox_filters(args.filters, list.filters)?)
        }
        Some(InboxAction::View(view)) => run_inbox_view(
            workspace,
            merge_inbox_filters(args.filters, view.filters)?,
            view.number,
            view.bead,
            view.proposal,
        ),
        Some(InboxAction::Chat(chat)) => run_inbox_chat(
            workspace,
            merge_inbox_filters(args.filters, chat.filters)?,
            chat.number,
            chat.bead,
            chat.proposal,
            agent_override,
        ),
    }
}

fn print_inbox_help() -> anyhow::Result<()> {
    let mut cmd = Cli::command();
    let Some(inbox) = cmd.find_subcommand_mut("inbox") else {
        anyhow::bail!("inbox subcommand help is unavailable");
    };
    let mut inbox_help = inbox.clone().bin_name("loom inbox");
    inbox_help.print_help()?;
    println!();
    Ok(())
}

#[derive(Debug, Clone)]
struct ResolvedInboxFilters {
    spec: Option<SpecLabel>,
    kind: Option<InboxKind>,
}

fn merge_inbox_filters(
    parent: InboxFilterArgs,
    child: InboxFilterArgs,
) -> anyhow::Result<ResolvedInboxFilters> {
    let spec = match (parent.spec, child.spec) {
        (Some(a), Some(b)) if a != b => anyhow::bail!("conflicting --spec filters: {a} and {b}"),
        (Some(a), _) | (_, Some(a)) => Some(SpecLabel::new(a)),
        (None, None) => None,
    };
    let kind = match (parent.kind, child.kind) {
        (Some(a), Some(b)) if a != b => anyhow::bail!("conflicting --kind filters"),
        (Some(a), _) | (_, Some(a)) => Some(InboxKind::from(a)),
        (None, None) => None,
    };
    Ok(ResolvedInboxFilters { spec, kind })
}

fn run_inbox_list(_workspace: &Path, filters: ResolvedInboxFilters) -> anyhow::Result<()> {
    let beads = load_inbox_beads()?;
    let items = build_queue(&beads, filters.spec.as_ref(), filters.kind, true);
    let rows = build_rows(&items, filters.spec.as_ref());
    if rows.is_empty() {
        println!("(no outstanding inbox items)");
        return Ok(());
    }
    for row in rows {
        match row.spec {
            Some(spec) => println!(
                "{:>3}. {} [{}] [spec:{}] ({}) {}",
                row.index,
                row.id,
                row.kind.tag(),
                spec,
                row.status,
                row.summary
            ),
            None => println!(
                "{:>3}. {} [{}] ({}) {}",
                row.index,
                row.id,
                row.kind.tag(),
                row.status,
                row.summary
            ),
        }
    }
    Ok(())
}

fn run_inbox_view(
    workspace: &Path,
    filters: ResolvedInboxFilters,
    number: Option<u32>,
    bead: Option<String>,
    proposal: Option<String>,
) -> anyhow::Result<()> {
    let beads = load_inbox_beads()?;
    let items = build_queue(&beads, filters.spec.as_ref(), filters.kind, true);
    let item = select_inbox_item(&items, number, bead.as_deref(), proposal.as_deref())?;
    render_inbox_item_view(workspace, item);
    Ok(())
}

fn run_inbox_chat(
    workspace: &Path,
    filters: ResolvedInboxFilters,
    number: Option<u32>,
    bead: Option<String>,
    proposal: Option<String>,
    agent_override: Option<AgentKind>,
) -> anyhow::Result<()> {
    let target = chat_target(number, bead, proposal)?;
    let _guard = match &target {
        Some(loom_workflow::inbox::chat::ChatTarget::Bead(id))
        | Some(loom_workflow::inbox::chat::ChatTarget::Proposal(id)) => {
            Some(acquire_work_root_lock(workspace, id)?)
        }
        _ => None,
    };
    let manifest = ProfileImageManifest::from_env()?;
    let opts = loom_workflow::inbox::chat::ChatOpts {
        spec_filter: filters.spec,
        kind_filter: filters.kind,
        target,
        cli_profile: None,
        agent_override,
        manifest,
        wrix_bin: std::env::var_os("LOOM_WRIX_BIN").map(PathBuf::from),
    };
    let report = loom_workflow::inbox::chat::run(workspace, opts)?;
    if report.items_surfaced == 0 {
        println!("(no outstanding inbox items)");
    } else {
        let resolved = report.items_surfaced.saturating_sub(report.items_remaining);
        println!(
            "loom inbox chat: surfaced {}, resolved {}, remaining {}, applied {}",
            report.items_surfaced, resolved, report.items_remaining, report.applied_proposals,
        );
    }
    Ok(())
}

fn load_inbox_beads() -> anyhow::Result<Vec<Bead>> {
    let runtime = tokio::runtime::Runtime::new()?;
    Ok(runtime.block_on(async {
        let bd = BdClient::new();
        bd.list(ListOpts {
            status: Some("open,in_progress,blocked,deferred".to_string()),
            ..ListOpts::default()
        })
        .await
    })?)
}

fn select_inbox_item<'a>(
    items: &'a [InboxItem],
    number: Option<u32>,
    bead: Option<&str>,
    proposal: Option<&str>,
) -> anyhow::Result<&'a InboxItem> {
    let selectors =
        u8::from(number.is_some()) + u8::from(bead.is_some()) + u8::from(proposal.is_some());
    if selectors > 1 {
        anyhow::bail!("use only one address selector: number, --bead, or --proposal");
    }
    if let Some(index) = number {
        let total = u32::try_from(items.len()).unwrap_or(u32::MAX);
        return find_by_index(items, index).ok_or_else(|| {
            anyhow::anyhow!("no inbox item at index {index} ({total} outstanding)")
        });
    }
    if let Some(id) = bead {
        return find_by_bead_id(items, id)
            .ok_or_else(|| anyhow::anyhow!("no inbox item with bead id {id}"));
    }
    if let Some(id) = proposal {
        return find_by_proposal_id(items, id)
            .ok_or_else(|| anyhow::anyhow!("no tune proposal with id {id}"));
    }
    anyhow::bail!("inbox view requires <N>, --bead <id>, or --proposal <id>")
}

fn chat_target(
    number: Option<u32>,
    bead: Option<String>,
    proposal: Option<String>,
) -> anyhow::Result<Option<loom_workflow::inbox::chat::ChatTarget>> {
    let selectors =
        u8::from(number.is_some()) + u8::from(bead.is_some()) + u8::from(proposal.is_some());
    if selectors > 1 {
        anyhow::bail!("use only one address selector: number, --bead, or --proposal");
    }
    Ok(match (number, bead, proposal) {
        (Some(index), None, None) => Some(loom_workflow::inbox::chat::ChatTarget::Index(index)),
        (None, Some(id), None) => Some(loom_workflow::inbox::chat::ChatTarget::Bead(id)),
        (None, None, Some(id)) => Some(loom_workflow::inbox::chat::ChatTarget::Proposal(id)),
        (None, None, None) => None,
        _ => None,
    })
}

fn render_inbox_item_view(workspace: &Path, item: &InboxItem) {
    print!("{}", render_inbox_item_view_text(workspace, item));
}

fn render_inbox_item_view_text(workspace: &Path, item: &InboxItem) -> String {
    let mut out = String::new();
    push_line(
        &mut out,
        format!(
            "inbox item {} [{}] ({})",
            item.durable_id(),
            item.kind.tag(),
            item.bead.status,
        ),
    );
    push_line(&mut out, format!("bead: {}", item.bead.id));
    if let Some(spec) = &item.spec {
        push_line(&mut out, format!("spec: {spec}"));
    }
    push_line(&mut out, format!("title: {}", item.bead.title));
    if let Some(infra) = &item.infra {
        push_infra_diagnostics(&mut out, infra);
    }
    if let Some(tune) = &item.tune {
        push_line(&mut out, format!("tune state: {}", tune.state));
        if let Some(branch) = &tune.proposal_branch {
            push_line(&mut out, format!("proposal branch: {branch}"));
        }
        if let Some(base) = &tune.base_commit {
            push_line(&mut out, format!("base commit: {base}"));
        }
        if let Some(head) = &tune.proposal_head {
            push_line(&mut out, format!("proposal head: {head}"));
        }
        let envelope = workspace.join(".loom/tune").join(&tune.proposal_id);
        push_line(&mut out, "artifacts:");
        push_artifact(&mut out, "envelope", &envelope);
        push_artifact(&mut out, "repo", &envelope.join("repo"));
        push_artifact(&mut out, "manifest", &envelope.join("manifest.json"));
        push_artifact(&mut out, "evidence", &envelope.join("evidence.md"));
    }
    push_blank(&mut out);
    push_line(&mut out, format!("description:\n{}", item.bead.description));
    if let Some(notes) = item.bead.notes.as_deref().filter(|notes| !notes.is_empty()) {
        push_blank(&mut out);
        push_line(&mut out, format!("notes:\n{notes}"));
    }
    let parsed = parse_options_in(item.bead.notes.as_deref(), &item.bead.description);
    if !parsed.summary.is_empty() || !parsed.options.is_empty() {
        push_blank(&mut out);
        push_line(&mut out, format!("options summary: {}", parsed.summary));
        for option in parsed.options {
            push_line(&mut out, format!("option {}: {}", option.n, option.title));
            if !option.body.is_empty() {
                push_line(&mut out, option.body);
            }
        }
    }
    push_blank(&mut out);
    push_line(&mut out, "manual escape hatches:");
    push_line(&mut out, format!("  bd show {}", item.bead.id));
    if item.tune.is_some() {
        push_line(
            &mut out,
            format!("  loom inbox chat -p {}", item.durable_id()),
        );
        push_line(
            &mut out,
            format!(
                "  inspect {}/.loom/tune/{}",
                workspace.display(),
                item.durable_id()
            ),
        );
    } else {
        push_line(&mut out, format!("  loom inbox chat -b {}", item.bead.id));
        if item.kind == InboxKind::Infra {
            push_line(
                &mut out,
                format!(
                    "  bd update {} --remove-label=loom:infra --status=open",
                    item.bead.id
                ),
            );
        }
    }
    out
}

fn push_infra_diagnostics(out: &mut String, infra: &loom_workflow::inbox::InfraInfo) {
    push_line(
        out,
        "flow: loom:infra — infrastructure/operator diagnostic; worker did not reach semantic judgement.",
    );
    push_line(out, "infra diagnostics:");
    push_optional_line(out, "phase", infra.phase.as_deref());
    if let Some(seen) = infra.first_event_seen {
        push_line(out, format!("  first_event_seen: {seen}"));
    }
    match (infra.attempt.as_deref(), infra.max_attempts.as_deref()) {
        (Some(attempt), Some(max)) => push_line(out, format!("  attempt: {attempt}/{max}")),
        (Some(attempt), None) => push_line(out, format!("  attempt: {attempt}")),
        (None, Some(max)) => push_line(out, format!("  max_attempts: {max}")),
        (None, None) => {}
    }
    push_optional_line(out, "exit_status", infra.exit_status.as_deref());
    push_optional_line(out, "stderr_tail", infra.stderr_tail.as_deref());
    push_optional_line(out, "spawn_error_tail", infra.spawn_error_tail.as_deref());
    push_optional_line(out, "log_path", infra.log_path.as_deref());
}

fn push_optional_line(out: &mut String, label: &str, value: Option<&str>) {
    if let Some(value) = value {
        push_line(out, format!("  {label}: {value}"));
    }
}

fn push_line(out: &mut String, line: impl AsRef<str>) {
    out.push_str(line.as_ref());
    out.push('\n');
}

fn push_blank(out: &mut String) {
    out.push('\n');
}

fn push_artifact(out: &mut String, label: &str, path: &Path) {
    let state = if path.exists() { "present" } else { "missing" };
    push_line(out, format!("  {label}: {} ({state})", path.display()));
}

fn run_todo(workspace: &Path, agent_override: Option<AgentKind>) -> anyhow::Result<()> {
    let manifest = Arc::new(ProfileImageManifest::from_env()?);
    let lock_mgr = LockManager::new(workspace)?;
    let _guard = lock_mgr.acquire_todo()?;

    let config = LoomConfig::load(LoomConfig::resolve_path(workspace))?;
    let selection = resolved_agent_for(&config, agent_override, Phase::Todo)?;
    let phase_default = selection.profile.clone();
    let kind = selection.kind;
    let shutdown_grace = resolve_shutdown_grace(&selection);
    let direct_output_limits = config.direct_output_limits();

    let state = Arc::new(CacheDb::open(workspace.join(".loom/cache.db"))?);
    let git = Arc::new(GitClient::open_with_integration_branch(
        workspace,
        config.loom.integration_branch.clone(),
    )?);
    let bd = Arc::new(BdClient::new());
    let runtime = tokio::runtime::Runtime::new()?;
    let workspace_buf = workspace.to_path_buf();
    let logs_root = workspace.join(".loom/logs");
    let workspace_for_renderer = workspace.to_path_buf();
    let render_mode = resolve_render_mode(RenderFlags {
        plain: false,
        json: false,
        raw: false,
        verbose: false,
    });
    let loom_cfg_for_todo = config.loom.clone();
    let skills_cfg_for_todo = config.skills.clone();
    let result = runtime.block_on(async move {
        let mut controller = ProductionTodoController::for_workspace(
            workspace_buf,
            state,
            manifest,
            phase_default,
            git,
            bd,
        )
        .with_loom_config(loom_cfg_for_todo)
        .with_agent_runtime(kind)
        .with_skills_config(skills_cfg_for_todo);
        run_todo_workflow(&mut controller, |spawn_cfg: SpawnConfig| {
            let selection = selection.clone();
            async move {
                let mut output = String::new();
                let mut spawn_cfg = spawn_cfg;
                selection.apply_to_spawn_config(&mut spawn_cfg, direct_output_limits);
                let renderer_id = todo_renderer_id(&spawn_cfg)?;
                let sink = open_todo_sink_with_renderer(
                    &logs_root,
                    &workspace_for_renderer,
                    render_mode,
                    &renderer_id,
                )?;
                let outcome = dispatch_with_envelope(
                    kind,
                    spawn_cfg,
                    shutdown_grace,
                    Some(sink),
                    Some(&mut output),
                    Some(build_phase_envelope_builder(Phase::Todo)),
                )
                .await?;
                let final_line = output.lines().rev().find(|line| !line.trim().is_empty());
                let todo_success = match final_line {
                    Some(line) if line.starts_with(loom_protocol::todo::TODO_SUCCESS_PREFIX) => {
                        Some(
                            parse_todo_success(line).map_err(|e| {
                                ProtocolError::Io(std::io::Error::other(e.to_string()))
                            })?,
                        )
                    }
                    _ => None,
                };
                let marker = parse_exit_signal(&output);
                Ok((outcome, marker, todo_success))
            }
        })
        .await
    });
    match result {
        Ok(summary) => {
            println!(
                "loom todo: agent exited {}, cost_usd={:?}",
                summary.exit_code, summary.cost_usd
            );
            for spec in summary.spec_outcomes {
                println!("loom todo: {} — {}", spec.label, spec.outcome);
            }
            Ok(())
        }
        Err(TodoError::NoChangedSpecs) => {
            println!("loom todo: no specs changed since their todo cursors");
            Ok(())
        }
        Err(TodoError::MultiSpecCollision { clarify_id }) => {
            println!(
                "loom todo: multi-spec collision detected; loom:clarify bead {clarify_id} created — resolve via `loom inbox`",
            );
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

fn acquire_work_root_lock(workspace: &Path, root: &str) -> anyhow::Result<LockGuard> {
    let root = BeadId::new(root)?;
    let lock_mgr = LockManager::new(workspace)?;
    Ok(lock_mgr.acquire_work_root(&root)?)
}

fn acquire_active_work_root_lock(
    workspace: &Path,
    label: &SpecLabel,
    runtime: &tokio::runtime::Runtime,
) -> anyhow::Result<Option<LockGuard>> {
    let active = runtime.block_on(async {
        let bd = BdClient::new();
        loom_workflow::resolve::resolve_open_epic(&bd, label).await
    })?;
    active
        .map(|root| acquire_work_root_lock(workspace, root.as_str()))
        .transpose()
}

fn acquire_review_work_root_lock(
    workspace: &Path,
    label: &SpecLabel,
    bead: Option<&str>,
    runtime: &tokio::runtime::Runtime,
) -> anyhow::Result<Option<LockGuard>> {
    if let Some(bead) = bead {
        return acquire_work_root_lock(workspace, bead).map(Some);
    }
    acquire_active_work_root_lock(workspace, label, runtime)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LoopWorkRootKind {
    Task,
    Epic,
}

#[derive(Debug, Clone)]
struct LoopWorkRoot {
    id: BeadId,
    label: SpecLabel,
    kind: LoopWorkRootKind,
    bead: Bead,
    ready_parent: Option<BeadId>,
}

async fn resolve_loop_work_roots<R: CommandRunner>(
    bd: &BdClient<R>,
    roots: Vec<String>,
) -> anyhow::Result<Vec<LoopWorkRoot>> {
    if roots.is_empty() {
        return Ok(vec![resolve_active_loop_work_root(bd).await?]);
    }

    let mut resolved = Vec::with_capacity(roots.len());
    for root in roots {
        resolved.push(resolve_explicit_loop_work_root(bd, &root).await?);
    }
    Ok(resolved)
}

async fn resolve_active_loop_work_root<R: CommandRunner>(
    bd: &BdClient<R>,
) -> anyhow::Result<LoopWorkRoot> {
    let active = bd
        .list(ListOpts {
            status: Some("open".to_string()),
            label: Some("loom:active".to_string()),
            issue_type: Some("epic".to_string()),
            ..ListOpts::default()
        })
        .await?;
    match active.len() {
        1 => loop_work_root_from_bead(active[0].clone(), LoopWorkRootKind::Epic),
        0 => Err(anyhow::anyhow!(
            "no open loom:active work epic found; pass a bead or epic id explicitly"
        )),
        _ => {
            let ids = active
                .iter()
                .map(|epic| epic.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Err(anyhow::anyhow!(
                "multiple open loom:active epics found: {ids}; keep exactly one active work epic"
            ))
        }
    }
}

async fn resolve_explicit_loop_work_root<R: CommandRunner>(
    bd: &BdClient<R>,
    root: &str,
) -> anyhow::Result<LoopWorkRoot> {
    let bead = bd.show_selector(root).await?;
    let kind = if bead.issue_type == "epic" {
        LoopWorkRootKind::Epic
    } else {
        LoopWorkRootKind::Task
    };
    loop_work_root_from_bead(bead, kind)
}

fn loop_work_root_from_bead(bead: Bead, kind: LoopWorkRootKind) -> anyhow::Result<LoopWorkRoot> {
    let label = primary_spec_label_from_work_root(&bead)?;
    let ready_parent = matches!(&kind, LoopWorkRootKind::Epic).then(|| bead.id.clone());
    Ok(LoopWorkRoot {
        id: bead.id.clone(),
        label,
        kind,
        bead,
        ready_parent,
    })
}

fn primary_spec_label_from_work_root(root: &Bead) -> anyhow::Result<SpecLabel> {
    let mut labels = root
        .labels
        .iter()
        .filter_map(|label| label.spec_label())
        .collect::<Vec<_>>();
    labels.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    labels.dedup_by(|a, b| a.as_str() == b.as_str());

    labels
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("work root {} has no spec:<label> label", root.id))
}

fn resolve_spec_label(workspace: &Path, spec: Option<String>) -> anyhow::Result<SpecLabel> {
    if let Some(s) = spec {
        return Ok(SpecLabel::new(s));
    }
    match std::env::var(REVIEW_SPEC_LABEL_ENV) {
        Ok(s) => return Ok(s.parse()?),
        Err(std::env::VarError::NotPresent) => {}
        Err(std::env::VarError::NotUnicode(raw)) => {
            anyhow::bail!("{REVIEW_SPEC_LABEL_ENV} must be valid UTF-8, got {:?}", raw,);
        }
    }
    resolve_spec_label_from_tree(workspace)
}

fn resolve_spec_label_from_tree(workspace: &Path) -> anyhow::Result<SpecLabel> {
    let labels = resolve_tree_mint_labels(workspace, None)?;
    if labels.len() == 1 {
        return labels
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no spec files found under specs/"));
    }
    Err(anyhow::anyhow!(
        "multiple specs found and no review context label is available; run from a single-spec workspace or via loom loop"
    ))
}

fn resolve_tree_mint_labels(
    workspace: &Path,
    spec: Option<&str>,
) -> anyhow::Result<Vec<SpecLabel>> {
    if let Some(label) = spec {
        return Ok(vec![SpecLabel::new(label)]);
    }

    let specs_dir = workspace.join("specs");
    let mut labels = Vec::new();
    for entry in std::fs::read_dir(&specs_dir)
        .with_context(|| format!("read specs directory `{}`", specs_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        labels.push(SpecLabel::new(stem));
    }
    labels.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    if labels.is_empty() {
        return Err(anyhow::anyhow!(
            "loom gate mint --tree found no specs under `{}`",
            specs_dir.display(),
        ));
    }
    Ok(labels)
}

fn current_loom_bin() -> anyhow::Result<PathBuf> {
    if let Some(bin) = std::env::var_os("LOOM_BIN") {
        return Ok(PathBuf::from(bin));
    }
    Ok(std::env::current_exe()?)
}

fn run_spec(workspace: &std::path::Path, label: String, deps: bool) -> anyhow::Result<()> {
    let label = SpecLabel::new(label);
    if deps {
        let pkgs = spec::deps_for_label(workspace, &label)?;
        for pkg in pkgs {
            println!("{pkg}");
        }
    } else {
        let spec_path = workspace
            .join("specs")
            .join(format!("{}.md", label.as_str()));
        let body = std::fs::read_to_string(&spec_path)?;
        let parsed = loom_gate::annotation::parse_content(&spec_path, &body);
        let mut annotated_lines: std::collections::BTreeSet<u32> =
            std::collections::BTreeSet::new();
        for ann in &parsed.annotations {
            annotated_lines.insert(ann.criterion_line);
            println!(
                "{tier}\t{line}\t{target}",
                tier = ann.tier.as_wire(),
                line = ann.criterion_line,
                target = ann.target,
            );
        }
        for crit in &parsed.criteria {
            if !annotated_lines.contains(&crit.line) {
                println!("none\t{line}\t", line = crit.line);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_workflow::review::FindingValidator;
    use std::collections::VecDeque;
    use std::ffi::OsString;

    #[derive(Clone)]
    struct ScriptedRunner {
        outputs: Arc<Mutex<VecDeque<&'static str>>>,
    }

    impl ScriptedRunner {
        fn new(outputs: impl IntoIterator<Item = &'static str>) -> Self {
            Self {
                outputs: Arc::new(Mutex::new(outputs.into_iter().collect())),
            }
        }
    }

    impl CommandRunner for ScriptedRunner {
        async fn run(
            &self,
            _args: Vec<OsString>,
            _timeout: Duration,
        ) -> Result<loom_driver::bd::RunOutput, loom_driver::bd::BdError> {
            let mut outputs = self.outputs.lock().expect("scripted output lock");
            let stdout = outputs.pop_front().expect("scripted bd output");
            Ok(loom_driver::bd::RunOutput {
                status: 0,
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
            })
        }
    }

    #[derive(Clone)]
    struct SharedOutputWriter {
        inner: Arc<Mutex<Vec<u8>>>,
    }

    impl std::io::Write for SharedOutputWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.inner
                .lock()
                .map_err(|_| std::io::Error::other("shared writer poisoned"))?
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn todo_agent_events_render_live_progress() -> anyhow::Result<()> {
        for render_mode in [
            loom_render::RenderMode::Plain,
            loom_render::RenderMode::Pretty,
        ] {
            let tmp = tempfile::tempdir()?;
            let logs_root = tmp.path().join(".loom/logs");
            let output = Arc::new(Mutex::new(Vec::new()));
            let writer = SharedOutputWriter {
                inner: Arc::clone(&output),
            };
            let renderer_id = BeadId::new("lm-todo")?;
            let mut sink = open_todo_sink_with_writer(
                &logs_root,
                tmp.path(),
                render_mode,
                &renderer_id,
                std::time::SystemTime::UNIX_EPOCH,
                Box::new(writer.clone()),
            )?;

            let mut envelope_builder = build_phase_envelope_builder(Phase::Todo);
            let envelope = envelope_builder.build();
            let session_id = envelope.session_id.as_str().to_string();
            assert!(envelope.bead_id.is_none());
            assert!(envelope.iteration.is_none());
            sink.emit(&loom_events::AgentEvent::TextDelta {
                envelope,
                text: "agent progress before summary\n".to_string(),
            })?;
            let path = sink.log_path().to_path_buf();
            sink.finish(loom_driver::logging::BeadOutcome::Done)?;
            let mut summary_writer = writer;
            std::io::Write::write_all(
                &mut summary_writer,
                b"loom todo: agent exited 0, cost_usd=None\n",
            )?;

            let rendered = String::from_utf8(output.lock().expect("output lock").clone())?;
            let progress = rendered
                .find("agent progress before summary")
                .ok_or_else(|| anyhow::anyhow!("missing live progress in {rendered:?}"))?;
            let summary = rendered
                .find("loom todo: agent exited")
                .ok_or_else(|| anyhow::anyhow!("missing final summary in {rendered:?}"))?;
            assert!(
                progress < summary,
                "{render_mode:?} live progress must precede final summary: {rendered:?}",
            );

            let body = std::fs::read_to_string(path)?;
            assert!(
                body.contains("agent progress before summary"),
                "phase JSONL log must persist the same event: {body}",
            );
            let first_event: serde_json::Value = serde_json::from_str(
                body.lines()
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("empty phase log"))?,
            )?;
            assert_eq!(
                first_event["session_id"].as_str(),
                Some(session_id.as_str())
            );
            assert!(first_event["bead_id"].is_null());
        }
        Ok(())
    }

    #[tokio::test]
    async fn loop_accepts_positional_work_roots_and_defaults_to_active_epic() -> anyhow::Result<()>
    {
        let active = r#"[{"id":"lm-active","title":"active","status":"open","priority":2,"issue_type":"epic","labels":["loom:active","spec:agent","spec:harness"],"metadata":{}}]"#;
        let task = r#"[{"id":"lm-task","title":"task","status":"open","priority":2,"issue_type":"task","labels":["spec:gate","spec:harness"],"metadata":{}}]"#;
        let epic = r#"[{"id":"lm-epic","title":"epic","status":"open","priority":2,"issue_type":"epic","labels":["spec:skills"],"metadata":{}}]"#;
        let bd = BdClient::with_runner(ScriptedRunner::new([active, task, epic]));

        let default_roots = resolve_loop_work_roots(&bd, Vec::new()).await?;
        assert_eq!(default_roots.len(), 1);
        let default_root = &default_roots[0];
        assert_eq!(default_root.id, BeadId::new("lm-active")?);
        assert_eq!(default_root.label, SpecLabel::new("agent"));
        assert_eq!(default_root.kind, LoopWorkRootKind::Epic);
        assert_eq!(default_root.ready_parent, Some(BeadId::new("lm-active")?));

        let explicit_roots =
            resolve_loop_work_roots(&bd, vec!["task".to_string(), "epic".to_string()]).await?;
        assert_eq!(explicit_roots.len(), 2);

        let task_root = &explicit_roots[0];
        assert_eq!(task_root.id, BeadId::new("lm-task")?);
        assert_eq!(task_root.label, SpecLabel::new("gate"));
        assert_eq!(task_root.kind, LoopWorkRootKind::Task);
        assert_eq!(task_root.ready_parent, None);

        let epic_root = &explicit_roots[1];
        assert_eq!(epic_root.id, BeadId::new("lm-epic")?);
        assert_eq!(epic_root.label, SpecLabel::new("skills"));
        assert_eq!(epic_root.kind, LoopWorkRootKind::Epic);
        assert_eq!(epic_root.ready_parent, Some(BeadId::new("lm-epic")?));
        Ok(())
    }

    /// Spec contract `specs/harness.md` § Labels: parallel-mode
    /// `loom:clarify` / `loom:blocked` self-reports must pair
    /// `status=blocked` with the label so `bd ready` excludes the parked
    /// bead via its native status filter. Without it the escalated bead
    /// stays ready and the next `loom loop` re-dispatches it instead of
    /// parking for human resolution — the divergence from the serial
    /// `apply_*` paths this guards against.
    #[test]
    fn parallel_park_pairs_status_blocked_with_label() {
        for label in ["loom:clarify", "loom:blocked"] {
            let opts = parallel_park_update(label, Some("a-note".to_string()));
            assert_eq!(
                opts.status.as_deref(),
                Some("blocked"),
                "{label}: must transition status=blocked so `bd ready` excludes it",
            );
            assert!(
                opts.add_labels.iter().any(|l| l == label),
                "{label}: terminal label must be applied: {:?}",
                opts.add_labels,
            );
        }
    }

    #[test]
    fn parallel_infra_budget_retries_then_parks_with_attempt_metadata() {
        let bead = BeadId::new("lm-infra").expect("valid bead id");
        let mut budget = ParallelInfraBudget::new(InfraRetryPolicy { max_attempts: 2 });
        let failure = BatchInfraFailure::Preflight {
            error: "spawn eof".to_string(),
        };

        let first = budget.record(&bead, &failure);
        match first {
            ParallelInfraRoute::Retry { diagnostic } => {
                assert_eq!(diagnostic.cause, "infra-preflight");
                assert_eq!(diagnostic.attempt, Some(1));
                assert_eq!(diagnostic.max_attempts, Some(2));
                assert_eq!(diagnostic.first_event_seen, Some(false));
            }
            other => panic!("first preflight failure should retry, got {other:?}"),
        }
        let second = budget.record(&bead, &failure);
        match second {
            ParallelInfraRoute::Park { diagnostic } => {
                assert_eq!(diagnostic.cause, "infra-preflight");
                assert_eq!(diagnostic.attempt, Some(2));
                assert_eq!(diagnostic.max_attempts, Some(2));
            }
            other => panic!("second preflight failure should park, got {other:?}"),
        }
    }

    #[test]
    fn parallel_infra_update_pairs_status_label_and_metadata() {
        let diagnostic = InfraDiagnostic::retryable(
            "infra-interrupted",
            "infra-interrupted",
            "stream eof".to_string(),
            2,
            3,
            true,
        );

        let opts = parallel_infra_update(&diagnostic);

        assert_eq!(opts.status.as_deref(), Some("blocked"));
        assert!(opts.add_labels.iter().any(|label| label == "loom:infra"));
        assert_eq!(opts.notes.as_deref(), Some("infra-interrupted: stream eof"),);
        assert!(opts.set_metadata.contains(&(
            "loom.infra.cause".to_string(),
            "infra-interrupted".to_string(),
        )));
        assert!(opts.set_metadata.contains(&(
            "loom.infra.first_event_seen".to_string(),
            "true".to_string(),
        )));
        assert!(
            opts.set_metadata
                .contains(&("loom.infra.attempt".to_string(), "2".to_string(),))
        );
        assert!(
            opts.set_metadata
                .contains(&("loom.infra.max_attempts".to_string(), "3".to_string(),))
        );
    }

    #[test]
    fn parallel_clear_infra_update_reopens_and_removes_label() {
        let opts = parallel_clear_infra_update();

        assert_eq!(opts.status.as_deref(), Some("open"));
        assert!(opts.remove_labels.iter().any(|label| label == "loom:infra"),);
    }

    #[test]
    fn inbox_kind_arg_accepts_infra() {
        let cli = Cli::try_parse_from(["loom", "inbox", "list", "--kind", "infra"])
            .expect("parse inbox infra kind");
        let Command::Inbox(args) = cli.command else {
            panic!("expected inbox command");
        };
        let Some(InboxAction::List(list)) = args.action else {
            panic!("expected inbox list action");
        };
        assert_eq!(list.filters.kind, Some(InboxKindArg::Infra));
    }

    #[test]
    fn inbox_view_text_renders_infra_diagnostics() {
        let mut bead = Bead {
            id: BeadId::new("lm-infra").expect("valid bead id"),
            title: "Infra diagnostic".into(),
            description: "worker never started".into(),
            status: "blocked".into(),
            priority: 2,
            issue_type: "task".into(),
            labels: vec![
                loom_driver::bd::Label::new("spec:harness"),
                loom_driver::bd::Label::new("loom:infra"),
            ],
            parent: None,
            metadata: Default::default(),
            notes: Some("infra-preflight: image load failed".into()),
        };
        bead.metadata
            .insert("loom.infra.phase".into(), serde_json::json!("pre-stream"));
        bead.metadata.insert(
            "loom.infra.first_event_seen".into(),
            serde_json::json!(false),
        );
        bead.metadata
            .insert("loom.infra.attempt".into(), serde_json::json!(2));
        bead.metadata
            .insert("loom.infra.max_attempts".into(), serde_json::json!(3));
        bead.metadata
            .insert("loom.infra.exit_status".into(), serde_json::json!(127));
        bead.metadata.insert(
            "loom.infra.stderr_tail".into(),
            serde_json::json!("container stderr"),
        );
        bead.metadata.insert(
            "loom.infra.spawn_error_tail".into(),
            serde_json::json!("spawn failed"),
        );
        bead.metadata
            .insert("loom.infra.log_path".into(), serde_json::json!("log.jsonl"));

        let items = build_queue(&[bead], None, Some(InboxKind::Infra), true);
        let body = render_inbox_item_view_text(Path::new("/workspace"), &items[0]);
        assert!(
            body.contains("inbox item lm-infra [infra] (blocked)"),
            "{body}"
        );
        assert!(
            body.contains("worker did not reach semantic judgement"),
            "{body}"
        );
        assert!(body.contains("phase: pre-stream"), "{body}");
        assert!(body.contains("first_event_seen: false"), "{body}");
        assert!(body.contains("attempt: 2/3"), "{body}");
        assert!(body.contains("exit_status: 127"), "{body}");
        assert!(body.contains("stderr_tail: container stderr"), "{body}");
        assert!(body.contains("spawn_error_tail: spawn failed"), "{body}");
        assert!(body.contains("log_path: log.jsonl"), "{body}");
        assert!(
            body.contains("--remove-label=loom:infra --status=open"),
            "{body}"
        );
    }

    /// Spec contract `specs/gate.md` § *Standing-safety-net checks*:
    /// bare `mint --tree` walks the full spec tree rather than resolving
    /// a single active spec label.
    #[test]
    fn mint_tree_without_spec_filter_resolves_every_workspace_spec() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let specs_dir = tmp.path().join("specs");
        std::fs::create_dir_all(&specs_dir).expect("mkdir specs");
        std::fs::write(specs_dir.join("harness.md"), "# Harness\n").expect("write harness");
        std::fs::write(specs_dir.join("gate.md"), "# Gate\n").expect("write gate");
        std::fs::write(specs_dir.join("notes.txt"), "ignored\n").expect("write non-md");

        let labels = resolve_tree_mint_labels(tmp.path(), None).expect("resolve labels");
        assert_eq!(
            labels,
            vec![SpecLabel::new("gate"), SpecLabel::new("harness")],
            "tree-scope mint must enumerate every markdown spec in lexical order",
        );
    }

    /// Spec contract `specs/gate.md` § *Molecule mint summary semantics*:
    /// `loom gate mint -m/--molecule <id>` exits 0 when promotion
    /// succeeds, including a no-op or promoted-deferred summary.
    #[test]
    fn mint_molecule_exits_zero_on_successful_promotion_summary() {
        use loom_workflow::mint::MintSummary;

        let mk = |minted: usize, promoted_deferred: usize, skipped: usize| MintSummary {
            batches: Vec::new(),
            statuses: Vec::new(),
            active_epic: None,
            minted,
            planned: 0,
            would_mint: 0,
            promoted_deferred,
            would_promote_deferred: 0,
            skipped,
            skipped_filter: 0,
            suppressed: 0,
            ineffective_suppressions: 0,
            stale_candidates: 0,
            partial_stale_candidates: 0,
            refused: 0,
            errors: 0,
            findings_across_minted: 0,
            specs_across_minted: 0,
        };

        for (label, summary) in [
            ("empty", mk(0, 0, 0)),
            ("minted-only", mk(3, 0, 0)),
            ("promoted-only", mk(0, 2, 0)),
            ("skipped-only", mk(0, 0, 2)),
            ("mixed minted+promoted+skipped", mk(2, 1, 4)),
        ] {
            assert_eq!(
                mint_summary_exit_code(&summary),
                0,
                "{label}: refused/errors == 0 → exit 0: {summary:?}",
            );
            assert!(
                summary.render().contains("promoted"),
                "summary names promoted count: {}",
                summary.render(),
            );
        }
    }

    /// Spec contract `specs/gate.md` § *Molecule mint summary semantics*:
    /// `loom gate mint -m/--molecule <id>` exits non-zero when
    /// promotion reports structural conflicts or bd write errors.
    #[test]
    fn mint_molecule_exits_nonzero_on_structural_or_write_errors() {
        use loom_workflow::mint::MintSummary;

        let mk = |refused: usize, errors: usize| MintSummary {
            batches: Vec::new(),
            statuses: Vec::new(),
            active_epic: None,
            minted: 0,
            planned: 0,
            would_mint: 0,
            promoted_deferred: 0,
            would_promote_deferred: 0,
            skipped: 0,
            skipped_filter: 0,
            suppressed: 0,
            ineffective_suppressions: 0,
            stale_candidates: 0,
            partial_stale_candidates: 0,
            refused,
            errors,
            findings_across_minted: 0,
            specs_across_minted: 0,
        };

        // Refused-only.
        let refused = mk(1, 0);
        assert_ne!(
            mint_summary_exit_code(&refused),
            0,
            "refused > 0 → non-zero exit: {refused:?}",
        );
        assert!(
            refused.render().contains("refused 1"),
            "header lists refused count: {}",
            refused.render(),
        );

        // Errored-only.
        let errored = mk(0, 1);
        assert_ne!(
            mint_summary_exit_code(&errored),
            0,
            "errors > 0 → non-zero exit: {errored:?}",
        );
        assert!(
            errored.render().contains("errors 1"),
            "header lists errors count: {}",
            errored.render(),
        );

        // Refused + errored together.
        let both = mk(1, 1);
        assert_ne!(
            mint_summary_exit_code(&both),
            0,
            "refused + errored → non-zero exit: {both:?}",
        );
    }

    #[test]
    fn verify_tiers_for_args_scopes_files_to_check_and_test() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut args = empty_scope_args();
        args.files.push(PathBuf::from(".pre-commit-config.yaml"));
        assert_eq!(
            verify_tiers_for_args(tmp.path(), &args).expect("tiers"),
            vec![Tier::Check, Tier::Test],
        );
    }

    #[test]
    fn verify_tiers_for_args_scopes_tree_to_all_deterministic_tiers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut args = empty_scope_args();
        args.tree = true;
        assert_eq!(
            verify_tiers_for_args(tmp.path(), &args).expect("tiers"),
            vec![Tier::Check, Tier::Test, Tier::System],
        );
    }

    #[test]
    fn verify_tiers_for_args_scopes_diff_to_check_and_test() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut args = empty_scope_args();
        args.diff = Some("HEAD".to_owned());
        assert_eq!(
            verify_tiers_for_args(tmp.path(), &args).expect("tiers"),
            vec![Tier::Check, Tier::Test],
        );
    }

    #[test]
    fn verify_target_rejects_cross_tier_ambiguity() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let specs = tmp.path().join("specs");
        std::fs::create_dir_all(&specs).expect("specs dir");
        std::fs::write(
            specs.join("alpha.md"),
            "## Success Criteria\n\n- check [check](same-target)\n- test [test](same-target)\n",
        )
        .expect("write spec");
        let mut args = empty_scope_args();
        args.target = Some("same-target".to_owned());
        let err = verify_tiers_for_args(tmp.path(), &args).expect_err("ambiguous target fails");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("multiple tiers"),
            "diagnostic suggests tier-specific target rerun: {rendered}",
        );
    }

    #[test]
    fn verify_target_accepts_same_tier_duplicates() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let specs = tmp.path().join("specs");
        std::fs::create_dir_all(&specs).expect("specs dir");
        std::fs::write(
            specs.join("alpha.md"),
            "## Success Criteria\n\n- one [check](same-target)\n- two [check](same-target)\n",
        )
        .expect("write spec");
        let mut args = empty_scope_args();
        args.target = Some("same-target".to_owned());
        assert_eq!(
            verify_tiers_for_args(tmp.path(), &args).expect("same-tier target"),
            vec![Tier::Check],
        );
    }

    #[test]
    fn target_check_does_not_run_integrity_gate_for_other_annotations() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let specs = tmp.path().join("specs");
        std::fs::create_dir_all(&specs).expect("specs dir");
        std::fs::write(
            specs.join("alpha.md"),
            "## Success Criteria\n\n- selected [check](true)\n- unrelated [check](definitely-missing-target-check)\n",
        )
        .expect("write spec");
        let mut args = empty_scope_args();
        args.target = Some("true".to_owned());
        let code = dispatch_tier(tmp.path(), &args, Tier::Check).expect("target dispatch");
        assert_eq!(
            code, 0,
            "unrelated unresolved annotations stay outside --target"
        );
    }

    #[test]
    fn missing_binary_skip_is_limited_to_explicit_files_scope() {
        let mut files = empty_scope_args();
        files.files = vec![PathBuf::from("src/lib.rs")];
        assert!(scope_allows_missing_binary_skip(&files));

        let mut diff = empty_scope_args();
        diff.diff = Some("HEAD".to_owned());
        diff.files = vec![PathBuf::from("src/lib.rs")];
        assert!(!scope_allows_missing_binary_skip(&diff));
    }

    #[test]
    fn integrity_gate_flags_missing_binary_under_diff_scope() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let specs = tmp.path().join("specs");
        std::fs::create_dir_all(&specs).expect("specs dir");
        std::fs::write(
            specs.join("alpha.md"),
            "## Success Criteria\n\n- broken verifier [check](definitely-no-such-loom-binary --flag)\n",
        )
        .expect("write spec");

        let mut args = empty_scope_args();
        args.diff = Some("HEAD".to_owned());
        args.files = vec![tmp.path().join("src/lib.rs")];

        let code = run_integrity_gate(tmp.path(), &args).expect("integrity gate runs");
        assert_eq!(code, 1);
    }

    #[test]
    fn review_status_records_report_unsuppressed_and_suppressed_findings() {
        let reported = loom_workflow::review::Finding {
            token: loom_workflow::review::ConcernToken::SpecCoherenceFail,
            route: loom_workflow::review::FindingRoute::Deferred,
            bonds: vec![SpecLabel::new("gate")],
            target: loom_workflow::review::FindingTarget::Criterion {
                spec: SpecLabel::new("gate"),
                anchor: "finding-status-output".to_owned(),
            },
            evidence: "live finding".to_owned(),
        };
        let suppressed = loom_workflow::review::Finding {
            token: loom_workflow::review::ConcernToken::VerifierBypass,
            route: loom_workflow::review::FindingRoute::Deferred,
            bonds: vec![SpecLabel::new("gate")],
            target: loom_workflow::review::FindingTarget::Annotation {
                target_string: "cargo test --lib sample".to_owned(),
            },
            evidence: "suppressed finding".to_owned(),
        };
        let stdout = format!(
            "LOOM_FINDING: {}\nLOOM_FINDING: {}\nLOOM_CONCERN: {{\"summary\":\"two findings\"}}\n",
            serde_json::to_string(&reported).expect("finding json"),
            serde_json::to_string(&suppressed).expect("finding json"),
        );
        let suppressions = vec![loom_driver::config::SuppressionConfig {
            id: Some(suppressed.id()),
            hash: None,
            reason: "false positive".to_owned(),
        }];

        let records = review_finding_status_records(&stdout, DispatchScope::PerBead, &suppressions);

        assert!(records.iter().any(|record| {
            record.id == reported.id() && record.action == FindingStatusAction::Reported
        }));
        assert!(records.iter().any(|record| {
            record.id == suppressed.id() && record.action == FindingStatusAction::Suppressed
        }));
    }

    #[test]
    fn review_status_records_report_tree_scope_only_findings_at_tree_scope() {
        let finding = loom_workflow::review::Finding {
            token: loom_workflow::review::ConcernToken::CrossSpecClash,
            route: loom_workflow::review::FindingRoute::Deferred,
            bonds: vec![SpecLabel::new("gate")],
            target: loom_workflow::review::FindingTarget::Criterion {
                spec: SpecLabel::new("gate"),
                anchor: "standing-safety-net-checks".to_owned(),
            },
            evidence: "tree-scope cross-spec clash".to_owned(),
        };
        let stdout = format!(
            "LOOM_FINDING: {}\nLOOM_CONCERN: {{\"summary\":\"tree finding\"}}\n",
            serde_json::to_string(&finding).expect("finding json"),
        );

        let records = review_finding_status_records(&stdout, DispatchScope::Tree, &[]);

        assert!(records.iter().any(|record| {
            record.id == finding.id() && record.action == FindingStatusAction::Reported
        }));
    }

    #[test]
    fn pending_forward_resolution_survives_finite_scope_filter() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let specs = tmp.path().join("specs");
        std::fs::create_dir_all(&specs).expect("specs dir");
        std::fs::write(
            specs.join("alpha.md"),
            "## Success Criteria\n\n- stale pending verifier [check?](true)\n",
        )
        .expect("write spec");

        let mut args = empty_scope_args();
        args.files = vec![PathBuf::from("src/lib.rs")];

        let code = run_integrity_gate(tmp.path(), &args).expect("integrity gate runs");
        assert_eq!(
            code, 1,
            "the resolved pending marker must still fire even when --files excludes its spec",
        );
    }

    #[test]
    fn dispatch_tier_skips_pending_annotations_before_scope_input_queries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let specs = tmp.path().join("specs");
        std::fs::create_dir_all(&specs).expect("specs dir");
        std::fs::write(
            specs.join("alpha.md"),
            "## Success Criteria\n\n- deferred verifier [check?](pending)\n",
        )
        .expect("write spec");
        let counter = tmp.path().join("inputs-count.txt");
        std::fs::write(&counter, "0").expect("write counter");
        let responder = tmp.path().join("inputs.sh");
        std::fs::write(
            &responder,
            format!(
                "#!/usr/bin/env bash\n\
                 set -euo pipefail\n\
                 n=$(< {:?})\n\
                 printf '%s\\n' \"$((n + 1))\" > {:?}\n\
                 printf '{{\"inputs\":[\"src/lib.rs\"]}}\\n'\n",
                counter, counter,
            ),
        )
        .expect("write responder");
        std::fs::write(
            tmp.path().join("loom.toml"),
            format!(
                "[runner.check.pending]\n\
                 match = '^pending$'\n\
                 command = 'false'\n\
                 inputs = 'bash {} {{print_inputs}} {{targets}}'\n",
                responder.display(),
            ),
        )
        .expect("write loom config");

        let mut args = empty_scope_args();
        args.files = vec![PathBuf::from("src/lib.rs")];

        let code = dispatch_tier(tmp.path(), &args, Tier::Check).expect("check tier dispatch runs");
        assert_eq!(code, 0, "unresolved pending marker remains silent");
        assert_eq!(
            std::fs::read_to_string(&counter)
                .expect("read counter")
                .trim(),
            "0",
            "pending dispatch skip avoids runner input-query side effects",
        );
    }

    #[test]
    fn integrity_gate_skips_missing_binary_under_explicit_files_scope() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let specs = tmp.path().join("specs");
        std::fs::create_dir_all(&specs).expect("specs dir");
        std::fs::write(
            specs.join("alpha.md"),
            "## Success Criteria\n\n- broken verifier [check](definitely-no-such-loom-binary --flag)\n",
        )
        .expect("write spec");

        let mut args = empty_scope_args();
        args.files = vec![tmp.path().join("src/lib.rs")];

        let code = run_integrity_gate(tmp.path(), &args).expect("integrity gate runs");
        assert_eq!(code, 0);
    }

    fn empty_scope_args() -> GateScopeArgs {
        GateScopeArgs {
            files: Vec::new(),
            target: None,
            diff: None,
            tree: false,
        }
    }

    /// An unparseable `--diff` range (here a ref that does not exist, the
    /// same failure shape as `@{u}` with no upstream) must fail loudly
    /// rather than degrade to an empty `args.files` — empty scope reads as
    /// "no filter" downstream and walks the whole tree.
    #[test]
    fn expand_diff_to_files_errors_on_unresolvable_range() {
        let tmp = tempfile::tempdir().expect("tempdir");
        loom_driver::git::init_test_repo_with_integration(tmp.path()).expect("init repo");
        let mut args = empty_scope_args();
        args.diff = Some("no-such-ref..HEAD".into());
        let err = expand_diff_to_files(tmp.path(), &mut args)
            .expect_err("an unresolvable diff range must fail loudly");
        assert!(
            args.files.is_empty(),
            "a failed expansion must not leave a partial scope",
        );
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no-such-ref..HEAD"),
            "error must name the offending range: {msg}",
        );
    }

    /// A valid range that simply has no changes (`HEAD` on a clean tree) is
    /// a legitimate empty scope, not an error — only ranges git rejects
    /// trip the fail-loud path.
    #[test]
    fn expand_diff_to_files_accepts_valid_empty_range() {
        let tmp = tempfile::tempdir().expect("tempdir");
        loom_driver::git::init_test_repo_with_integration(tmp.path()).expect("init repo");
        let mut args = empty_scope_args();
        args.diff = Some("HEAD".into());
        expand_diff_to_files(tmp.path(), &mut args)
            .expect("a valid range with no changes is a legitimate empty scope");
        assert!(args.files.is_empty());
    }

    /// Spec contract `specs/gate.md` § *Findings and Minting* (criterion
    /// `mint_refuses_when_loom_inside_env_is_set`): `loom gate mint`
    /// must refuse to run inside a loom-managed container with a
    /// deterministic non-zero exit. The top-level main check consults
    /// [`Command::refused_inside_loom`] against the parsed command; this
    /// test pins the variant-to-marker mapping so a future GateSubcommand
    /// edit can't accidentally let mint slip past the guard.
    #[test]
    fn mint_refuses_when_loom_inside_env_is_set() {
        let mint_args = GateMintArgs {
            molecule: None,
            tree: true,
            dry_run: false,
        };
        let cmd = Command::Gate {
            subcommand: Some(GateSubcommand::Mint(mint_args)),
        };
        assert!(
            cmd.refused_inside_loom(),
            "loom gate mint must be refused under LOOM_INSIDE — it spawns containers and mutates bd state",
        );
    }

    #[test]
    fn mint_bare_invocation_requires_explicit_scope() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for rejected in ["-b", "--diff", "--files", "--spec", "--target"] {
            let parsed = Cli::try_parse_from(["loom", "gate", "mint", rejected, "value"]);
            assert!(
                parsed.is_err(),
                "mint must reject inspection-only scope flag {rejected}",
            );
        }

        let args = GateMintArgs {
            molecule: None,
            tree: false,
            dry_run: false,
        };
        let err =
            resolve_mint_scope(tmp.path(), &args).expect_err("bare mint has no default molecule");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("--tree"),
            "diagnostic names --tree: {rendered}"
        );
        assert!(
            rendered.contains("-m/--molecule"),
            "diagnostic names -m/--molecule: {rendered}",
        );
    }

    /// `loom gate mint` validates finding bonds and targets against the
    /// workspace before minting.
    #[test]
    fn workspace_finding_validator_checks_spec_anchor_and_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("specs")).expect("specs dir");
        std::fs::create_dir_all(tmp.path().join("tests")).expect("tests dir");
        std::fs::write(
            tmp.path().join("specs/gate.md"),
            "# Gate\n\n### Findings and Minting\n\n## Architecture\n\nService identity derives from sha256(workspace path).\n\n## Out of Scope\n\nloom-runs-podman is not part of this spec.\n",
        )
        .expect("write spec");
        std::fs::write(tmp.path().join("tests/gate.rs"), "#[test] fn live() {}\n")
            .expect("write test");
        let validator = WorkspaceFindingValidator::new(tmp.path());
        let gate = SpecLabel::new("gate");

        assert!(validator.spec_label_is_known(&gate));
        assert!(validator.criterion_anchor_resolves(&gate, "findings-and-minting"));
        assert!(validator.file_exists("tests/gate.rs::live"));
        assert!(validator.invariant_resolves(&gate, "Out of Scope", "loom-runs-podman"));
        assert!(validator.invariant_resolves(&gate, "Architecture", "workspace-service-identity"));
        assert!(!validator.spec_label_is_known(&SpecLabel::new("harness")));
        assert!(!validator.criterion_anchor_resolves(&gate, "missing-anchor"));
        assert!(!validator.invariant_resolves(&gate, "Architecture", "workspace-service-missing"));
        assert!(!validator.file_exists("tests/missing.rs::live"));
    }

    #[test]
    fn workspace_finding_validator_accepts_reviewer_target_aliases() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("specs")).expect("specs dir");
        std::fs::write(
            tmp.path().join("specs/beads.md"),
            "# Beads\n\n## Success Criteria\n\n- Push failures fall back only after fast-forward rejection\n  [judge](tests/judges/beads.sh test_beadspush_pushes_before_pulls)\n- Pending verifier placeholder [check?](true)\n",
        )
        .expect("write spec");
        let output = concat!(
            r#"LOOM_FINDING: {"token":"spec-coherence-fail","route":"deferred","bonds":["beads"],"target":{"kind":"Criterion","spec":"beads","anchor":"test_beadspush_pushes_before_pulls"},"evidence":"criterion target uses attached judge function"}"#,
            "\n",
            r#"LOOM_FINDING: {"token":"verifier-bypass","route":"deferred","bonds":["beads"],"target":{"kind":"Annotation","target_string":"specs/beads.md:7 [check?](true)"},"evidence":"annotation target includes rendered source coordinate"}"#,
            "\nLOOM_CONCERN: {\"summary\":\"target aliases\"}\n",
        );
        let findings = loom_workflow::review::parse_walk_output(
            output,
            DispatchScope::Tree,
            &WorkspaceFindingValidator::new(tmp.path()),
        )
        .expect("reviewer target aliases should parse");

        assert_eq!(findings.len(), 2);
        match &findings[1].target {
            loom_workflow::review::FindingTarget::Annotation { target_string } => {
                assert_eq!(target_string, "true");
            }
            other => panic!("expected Annotation target, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mint_via_walker_rejects_unresolved_workspace_finding_target() {
        use loom_driver::bd::{BdClient, BdError, CommandRunner, RunOutput};
        use loom_workflow::mint::{MintOptions, MintScope, MintWalker, VerifierFailure, WalkError};
        use std::ffi::OsString;
        use std::time::Duration;

        struct OneFindingWalker {
            stdout: String,
        }

        impl MintWalker for OneFindingWalker {
            async fn run_rubric(&mut self, _scope: &MintScope) -> Result<String, WalkError> {
                Ok(self.stdout.clone())
            }

            async fn run_verifiers(
                &mut self,
                _scope: &MintScope,
            ) -> Result<Vec<VerifierFailure>, WalkError> {
                Ok(Vec::new())
            }
        }

        struct PanicOnBdCall;
        impl CommandRunner for PanicOnBdCall {
            async fn run(&self, args: Vec<OsString>, _t: Duration) -> Result<RunOutput, BdError> {
                panic!("bd must not be invoked when parsing refuses the finding: {args:?}");
            }
        }

        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("specs")).expect("specs dir");
        std::fs::write(tmp.path().join("specs/gate.md"), "# Gate\n").expect("write spec");
        let finding = loom_workflow::review::Finding {
            token: loom_workflow::review::ConcernToken::SpecCoherenceFail,
            route: loom_workflow::review::FindingRoute::Deferred,
            bonds: vec![SpecLabel::new("gate")],
            target: loom_workflow::review::FindingTarget::Criterion {
                spec: SpecLabel::new("gate"),
                anchor: "missing-anchor".to_owned(),
            },
            evidence: "missing anchor".to_owned(),
        };
        let stdout = format!(
            "LOOM_FINDING: {}\nLOOM_CONCERN: {{\"summary\":\"missing\"}}\n",
            serde_json::to_string(&finding).expect("finding json"),
        );
        let mut walker = OneFindingWalker { stdout };
        let validator = WorkspaceFindingValidator::new(tmp.path());
        let bd = BdClient::with_runner(PanicOnBdCall);

        let err = mint_via_walker(
            &mut walker,
            &MintScope::Tree,
            &validator,
            &bd,
            "abc123",
            &MintOptions::default(),
        )
        .await
        .expect_err("unresolved target must refuse before minting");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("criterion `missing-anchor` not present in spec `gate`"),
            "unexpected error: {rendered}",
        );
    }

    /// [`mint_via_walker`] must obtain its `Vec<Finding>` from
    /// `walk(walker, scope, validator)` and feed it into
    /// tree materialization. Drives the strict helper seam with a
    /// recording [`MintWalker`]; the live-path subprocess verifier
    /// under `specs/gate.md` § *Production walker wiring* pins that
    /// `run_gate_mint` reaches the production walker, closing off the
    /// `Vec::<Finding>::new()` shortcut.
    #[tokio::test]
    async fn mint_via_walker_obtains_findings_from_walker_run_rubric() {
        use loom_driver::bd::{BdClient, BdError, CommandRunner, RunOutput};
        use loom_workflow::mint::{MintOptions, MintScope, MintWalker, VerifierFailure, WalkError};
        use loom_workflow::review::AcceptAllFindingValidator;
        use std::ffi::OsString;
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        struct RecordingMintWalker {
            rubric_calls: Arc<Mutex<usize>>,
            verifier_calls: Arc<Mutex<usize>>,
            scopes_seen: Arc<Mutex<Vec<MintScope>>>,
            rubric_stdout: String,
        }

        impl MintWalker for RecordingMintWalker {
            async fn run_rubric(&mut self, scope: &MintScope) -> Result<String, WalkError> {
                *self.rubric_calls.lock().expect("not poisoned") += 1;
                self.scopes_seen
                    .lock()
                    .expect("not poisoned")
                    .push(scope.clone());
                Ok(self.rubric_stdout.clone())
            }

            async fn run_verifiers(
                &mut self,
                _scope: &MintScope,
            ) -> Result<Vec<VerifierFailure>, WalkError> {
                *self.verifier_calls.lock().expect("not poisoned") += 1;
                Ok(Vec::new())
            }
        }

        struct PanicOnBdCall;
        impl CommandRunner for PanicOnBdCall {
            async fn run(&self, args: Vec<OsString>, _t: Duration) -> Result<RunOutput, BdError> {
                panic!("bd must not be invoked when no findings are produced: {args:?}");
            }
        }

        let rubric_calls = Arc::new(Mutex::new(0_usize));
        let verifier_calls = Arc::new(Mutex::new(0_usize));
        let scopes_seen = Arc::new(Mutex::new(Vec::new()));
        let mut walker = RecordingMintWalker {
            rubric_calls: Arc::clone(&rubric_calls),
            verifier_calls: Arc::clone(&verifier_calls),
            scopes_seen: Arc::clone(&scopes_seen),
            rubric_stdout: "LOOM_COMPLETE\n".to_string(),
        };
        let scope = MintScope::Tree;
        let validator = AcceptAllFindingValidator;
        let bd = BdClient::with_runner(PanicOnBdCall);
        let opts = MintOptions::default();

        let summary = mint_via_walker(&mut walker, &scope, &validator, &bd, "abc123", &opts)
            .await
            .expect("mint_via_walker succeeds with an empty rubric");

        assert_eq!(
            *rubric_calls.lock().expect("not poisoned"),
            1,
            "walker.run_rubric must be invoked exactly once — a \
             Vec::<Finding>::new() shortcut bypassing the walker would \
             leave rubric_calls=0",
        );
        let seen = scopes_seen.lock().expect("not poisoned").clone();
        assert_eq!(
            seen,
            vec![scope.clone()],
            "the walker must receive the same scope the caller passed",
        );
        assert_eq!(
            *verifier_calls.lock().expect("not poisoned"),
            1,
            "tree scope dispatches verifiers before rubric",
        );
        assert!(
            summary.batches.is_empty(),
            "no LOOM_FINDING lines in rubric → empty summary: {summary:?}",
        );
    }

    fn workspace_with_check_and_system_runners() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("loom.toml"),
            r#"
[runner.check.grep]
match   = '^grep '
command = "{targets}"

[runner.system.nix]
match   = '^nix (build|run) \.#(\S+)$'
command = "nix build {targets}"
parse   = "nix-build-status"
"#,
        )
        .expect("write loom.toml");
        tmp
    }

    /// Spec contract `specs/gate.md` § Target resolution: a `[system](target)`
    /// resolves by matching a `[runner.system.<name>]` block, so the binary's
    /// System-tier runner context MUST compile those blocks. The earlier
    /// check-only producer never read `tier("system")`, leaving every system
    /// runner unmatched.
    #[test]
    fn resolve_runner_context_system_tier_compiles_runner_system_blocks() {
        let tmp = workspace_with_check_and_system_runners();
        let (specs, _) =
            resolve_runner_context(tmp.path(), Tier::System).expect("resolve system context");

        assert!(
            specs
                .iter()
                .any(|s| s.name == "nix" && s.matches("nix run .#test-loom")),
            "[runner.system.nix] must compile into the System-tier context and match its target: {:?}",
            specs.iter().map(|s| &s.name).collect::<Vec<_>>(),
        );
        assert!(
            !specs.iter().any(|s| s.name == "builtin-loom-walk"),
            "the [check]-tier builtin batcher must not leak into the System-tier context",
        );
        assert!(
            !specs.iter().any(|s| s.name == "grep"),
            "the System-tier context must not carry [runner.check.<name>] runners",
        );
    }

    /// The `[check]`-tier context keeps the always-present builtin loom-walk
    /// batcher alongside any `[runner.check.<name>]` overrides, and never
    /// carries `[runner.system.<name>]` runners.
    #[test]
    fn resolve_runner_context_check_tier_includes_builtin_and_check_runners() {
        let tmp = workspace_with_check_and_system_runners();
        let (specs, _) =
            resolve_runner_context(tmp.path(), Tier::Check).expect("resolve check context");

        assert!(
            specs.iter().any(|s| s.name == "builtin-loom-walk"),
            "the builtin loom-walk batcher is always present for [check]",
        );
        assert!(
            specs.iter().any(|s| s.name == "grep"),
            "[runner.check.<name>] overrides layer onto the [check] context",
        );
        assert!(
            !specs.iter().any(|s| s.name == "nix"),
            "the [check] context must not carry [runner.system.<name>] runners",
        );
    }

    /// The integrity gate checks every annotation regardless of tier, so its
    /// runner context unions `[check]`- and `[system]`-tier runners: both a
    /// `[check]` and a `[system]` target resolve by runner ownership. Before
    /// the fix the system target matched no spec and fell through to the
    /// `tokens[0]`-on-PATH check.
    #[test]
    fn resolve_integrity_runner_context_unions_check_and_system_runners() {
        let tmp = workspace_with_check_and_system_runners();
        let (specs, _) =
            resolve_integrity_runner_context(tmp.path()).expect("resolve integrity context");

        assert!(
            specs.iter().any(|s| s.matches("grep -q X file")),
            "integrity context resolves a [check] target through its runner",
        );
        assert!(
            specs.iter().any(|s| s.matches("nix run .#test-loom")),
            "integrity context resolves a [system] target through its runner",
        );
    }
}

//! `loom` CLI binary entry point.
//!
//! Parses command-line arguments and dispatches to the workflow modules in
//! `loom-workflow`. The set of subcommands matches the harness specification:
//! `init`, `status`, `use`, `logs`, `spec`, plus the previously-implemented
//! `run`, `gate`, `msg`. There is no `sync` or `tune` — Askama compiled
//! templates make per-project sync unnecessary (see `specs/harness.md`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use clap::{ArgGroup, CommandFactory, Parser, Subcommand, ValueEnum};

use loom_agent::{ClaudeBackend, PiBackend};
use loom_driver::agent::{AgentKind, LOOM_INSIDE_ENV, ProtocolError, SessionOutcome, SpawnConfig};
use loom_driver::bd::{BdClient, ListOpts, UpdateOpts};
use loom_driver::clock::{Clock, SystemClock};
use loom_driver::config::{AgentObserversConfig, LoomConfig, Phase};
use loom_driver::git::GitClient;
use loom_driver::identifier::{BeadId, ProfileName, SpecLabel};
use loom_driver::lock::LockManager;
use loom_driver::logging::{LogSink, sweep_retention_at};
use loom_driver::profile_manifest::ProfileImageManifest;
use loom_driver::scratch::resolve_scratch_key;
use loom_driver::state::StateDb;
use loom_gate::{
    self, CacheRow, CargoMetadataScope, CommandResolver, DispatchOptions, DispatchPendingExecutor,
    FsCommandResolver, InputResolver, RunnerSpec, StatusCache, TestScope, Tier, TierCwds, Verdict,
    filter_by_files, is_missing_binary_target, render_report, row_for,
};
use loom_workflow::r#loop::{
    GateOutcome, LoopMode, LoopOutcome, NoGateReason, Parallelism, ProductionAgentLoopController,
    REVIEW_EMIT_STDOUT_ENV, REVIEW_PHASE_WHEN_ENV, RetryPolicy, SessionResult, run_loop,
};
use loom_workflow::mint::{FindingStatusAction, FindingStatusRecord, MintWalker};
use loom_workflow::msg::{
    DISMISS_NOTE, build_rows, compose_option_note, compose_resolved_notes, filter_msg_beads,
    kind_of, resolve_target, spec_label_of,
};
use loom_workflow::review::{
    AcceptAllFindingValidator, DispatchScope, FindingValidator, IterationCap,
    ProductionReviewController, ReviewLane, WalkOutput, review_loop as run_review_loop,
};
use loom_workflow::todo::{
    ExitSignal, ProductionTodoController, TodoError, parse_exit_signal, run as run_todo_workflow,
};
use loom_workflow::{DefaultObserverChain, init, logs_cmd, plan, spec, status, use_spec};
use loom_workflow::{run_agent, run_agent_classified};

/// Top-level CLI surface.
#[derive(Debug, Parser)]
#[command(name = "loom", version, about = "Loom harness CLI")]
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
}

impl From<AgentBackendArg> for AgentKind {
    fn from(arg: AgentBackendArg) -> Self {
        match arg {
            AgentBackendArg::Claude => AgentKind::Claude,
            AgentBackendArg::Pi => AgentKind::Pi,
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
    VerifyMarker,
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

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize the workspace (create `.loom/` config + state DB).
    Init {
        /// Drop and repopulate the state DB from `specs/*.md` and active beads.
        #[arg(long)]
        rebuild: bool,
    },
    /// Print the active spec, current molecule, and iteration counter.
    Status,
    /// Set the active spec.
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
        /// Stream assistant text deltas during render — equivalent to
        /// `loom loop -v`. Mutually exclusive with `--raw` and `--path`.
        #[arg(long, short = 'v', conflicts_with_all = ["raw", "path"])]
        verbose: bool,
        /// Print the resolved log file path and exit. Mutually
        /// exclusive with `-f`, `-v`, `--raw`.
        #[arg(long, conflicts_with_all = ["follow", "raw", "verbose"])]
        path: bool,
    },
    /// Inspect spec annotations and tooling dependencies.
    Spec {
        /// Print the unique nixpkgs names referenced by spec annotation targets.
        #[arg(long)]
        deps: bool,
    },
    /// Interactive spec interview (`-n <label>` new, `-u <label>` update).
    Plan {
        /// New-spec interview for `<label>`.
        #[arg(short = 'n', value_name = "LABEL")]
        new: Option<String>,
        /// Update-spec interview for `<label>`.
        #[arg(short = 'u', value_name = "LABEL")]
        update: Option<String>,
        /// Override the profile resolution chain. Wins over
        /// `[phase.plan].profile` and `[phase.default].profile` in
        /// `<workspace>/loom.toml` (default `base`).
        #[arg(long, value_name = "PROFILE")]
        profile: Option<String>,
    },
    /// Per-bead execution loop. Continuous by default; `--once` exits after one bead.
    Loop {
        /// Process a single bead then exit (no auto-handoff to `loom gate verify` / `loom gate review`).
        #[arg(long)]
        once: bool,
        /// Concurrent dispatch slots (`-p N` / `--parallel N`). Default 1.
        #[arg(long, short = 'p', default_value = "1")]
        parallel: Parallelism,
        /// Override the per-bead `profile:X` label resolution.
        #[arg(long, value_name = "PROFILE")]
        profile: Option<String>,
        /// Spec label override (defaults to `current_spec`).
        #[arg(long, short = 's', value_name = "LABEL")]
        spec: Option<String>,
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
        /// Widen the rendered text: streams `TextDelta` verbatim and
        /// renders each `ToolResult` body capped at 10 lines with a
        /// recovery hint. Mutually exclusive with `--raw`.
        #[arg(long, short = 'v', conflicts_with = "raw")]
        verbose: bool,
    },
    /// Quality gate — annotation-dispatched verifiers and LLM rubric.
    Gate {
        #[command(subcommand)]
        subcommand: Option<GateSubcommand>,
    },
    /// Resolve outstanding `loom:clarify` and `loom:blocked` beads.
    Msg {
        /// Filter to a specific spec label.
        #[arg(long, short = 's', value_name = "LABEL")]
        spec: Option<String>,
        /// Select bead by 1-based index in the printed list. Mutually
        /// exclusive with `-b`.
        #[arg(long, short = 'n', value_name = "N", conflicts_with = "bead")]
        number: Option<u32>,
        /// Select bead by id. Mutually exclusive with `-n`.
        #[arg(long, short = 'b', value_name = "ID")]
        bead: Option<String>,
        /// Fast-reply with the body of `### Option <int>` for a clarify
        /// bead. Validated — missing subsection exits non-zero before any
        /// bd state is mutated. Mutually exclusive with `-r` and `-d`.
        #[arg(
            long,
            short = 'o',
            value_name = "INT",
            conflicts_with_all = ["reply", "dismiss"]
        )]
        option: Option<u32>,
        /// Fast-reply with verbatim text. Works on any bead regardless of
        /// whether it has an `## Options` section. Mutually exclusive with
        /// `-o` and `-d`.
        #[arg(
            long,
            short = 'r',
            value_name = "TEXT",
            conflicts_with_all = ["option", "dismiss"]
        )]
        reply: Option<String>,
        /// Dismiss the bead (write canonical note + remove the loom:* label).
        #[arg(long, short = 'd')]
        dismiss: bool,
        /// Launch an interactive Drafter chat session.
        /// Renders the msg.md template and spawns a container with the
        /// claude backend attached to the user's terminal. Mutually
        /// exclusive with `-o`, `-r`, `-d`, `-b`, `-n` — the chat
        /// session walks every outstanding clarify, no single-bead
        /// selection. `-s <label>` may scope the walk to one spec.
        #[arg(
            long,
            short = 'c',
            conflicts_with_all = ["option", "reply", "dismiss", "bead", "number"]
        )]
        chat: bool,
    },
    /// Decompose every touched spec into beads (multi-spec fan-out).
    Todo {
        /// Override the anchor's `base_commit` for tier-1 detection.
        #[arg(long, value_name = "COMMIT")]
        since: Option<String>,
    },
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
                subcommand: Some(GateSubcommand::VerifyMarker),
            } => false,
            Command::Init { .. }
            | Command::UseSpec { .. }
            | Command::Plan { .. }
            | Command::Loop { .. }
            | Command::Gate { .. }
            | Command::Msg { .. }
            | Command::Note { .. }
            | Command::Todo { .. } => true,
        }
    }
}

/// Subcommand groups rendered in `loom --help`, in spec order. Spec
/// reference: `harness.md` § Functional #1. Clap's
/// `next_help_heading` applies to flags, not subcommands, so the binary
/// regroups the auto-generated `Commands:` block instead.
const HELP_GROUPS: &[(&str, &[&str])] = &[
    ("Workflow", &["plan", "todo", "loop", "gate", "msg"]),
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
        "init", "status", "use", "logs", "spec", "plan", "loop", "gate", "msg", "todo", "note",
        "help",
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
                .unwrap_or_default();
            let _ = writeln!(grouped, "  {name:<width$}  {about}", width = width);
        }
        grouped.push('\n');
    }
    let help_about = cmd
        .get_subcommands()
        .find(|s| s.get_name() == "help")
        .and_then(|s| s.get_about().map(|d| d.to_string()))
        .unwrap_or_else(|| "Print this message or the help of the given subcommand(s)".to_string());
    let _ = writeln!(
        grouped,
        "  {help:<width$}  {help_about}",
        help = "help",
        width = width
    );

    print!("{}", replace_commands_section(&default_help, &grouped));
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

fn main() -> ExitCode {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    if args_request_top_level_help(&raw_args) {
        print_grouped_help();
        return ExitCode::SUCCESS;
    }

    let cli = Cli::parse();

    if std::env::var_os(LOOM_INSIDE_ENV).is_some() && cli.command.refused_inside_loom() {
        eprintln!(
            "error: loom cannot run inside a loom-managed container\n  this command spawns containers or mutates workspace state, which\n  would create a nested driver. read-only commands (status, logs,\n  spec) are still available."
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
        Command::Spec { deps } => run_spec(&workspace, deps).map(|()| ExitCode::SUCCESS),
        Command::Plan {
            new,
            update,
            profile,
        } => run_plan(&workspace, new, update, profile, agent_override).map(|()| ExitCode::SUCCESS),
        Command::Loop {
            once,
            parallel,
            profile,
            spec,
            plain,
            json,
            raw,
            verbose,
        } => run_loop_cmd(
            &workspace,
            once,
            parallel,
            profile,
            spec,
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
        Command::Msg {
            spec,
            number,
            bead,
            option,
            reply,
            dismiss,
            chat,
        } => run_msg(
            &workspace,
            spec,
            number,
            bead,
            option,
            reply,
            dismiss,
            chat,
            agent_override,
        )
        .map(|()| ExitCode::SUCCESS),
        Command::Todo { since } => {
            run_todo(&workspace, since, agent_override).map(|()| ExitCode::SUCCESS)
        }
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
/// consume one canonical signal across sequential, parallel, once, and
/// all-specs codepaths.
fn exit_code_for_gate(gate: &GateOutcome) -> ExitCode {
    match gate {
        GateOutcome::Success(_) | GateOutcome::NoGate { .. } => ExitCode::SUCCESS,
        GateOutcome::Fail(_) => ExitCode::from(1),
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
    println!("  state.db: {}", report.state_db_path.display());
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
            "  rebuilt {} spec(s), {} molecule(s), {} companion(s)",
            rb.specs, rb.molecules, rb.companions,
        );
    }
    Ok(())
}

fn run_note(workspace: &std::path::Path, action: NoteAction) -> anyhow::Result<()> {
    let db = loom_driver::state::StateDb::open(workspace.join(".loom/state.db"))?;
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
        Some(GateSubcommand::VerifyMarker) => run_gate_verify_marker(workspace),
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

fn run_gate_verify_marker(workspace: &Path) -> anyhow::Result<()> {
    match loom_gate::verify_marker(workspace) {
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
    let cache_path = workspace.join(".loom/gate-cache.sqlite");
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
    let cache_path = workspace.join(".loom/gate-cache.sqlite");
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
                    persist_outcome(&cache, &outcome, verdict, now_ms, &commit);
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
                persist_outcome(cache, &outcome, Verdict::Skipped, now_ms, commit);
            }
            Ok(outcome) if outcome.verdict.pass => {
                persist_outcome(cache, &outcome, Verdict::Pass, now_ms, commit);
            }
            Ok(outcome) => {
                let _ = writeln!(stderr, "loom gate [check] FAIL: {}", ann.target);
                for line in outcome.verdict.evidence.lines().take(5) {
                    let _ = writeln!(stderr, "  {line}");
                }
                persist_outcome(cache, &outcome, Verdict::Fail, now_ms, commit);
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
                    persist_outcome(cache, &outcome, Verdict::Skipped, now_ms, commit);
                }
                Ok(outcome) if outcome.verdict.pass => {
                    persist_outcome(cache, &outcome, Verdict::Pass, now_ms, commit);
                }
                Ok(outcome) => {
                    if is_tty {
                        let _ = write!(stderr, "\x1b[2K\r");
                    }
                    let _ = writeln!(stderr, "loom gate [{tier}] FAIL: {}", ann.target);
                    for line in outcome.verdict.evidence.lines().take(5) {
                        let _ = writeln!(stderr, "  {line}");
                    }
                    persist_outcome(cache, &outcome, Verdict::Fail, now_ms, commit);
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
    cache: &StatusCache,
    outcome: &loom_gate::DispatchOutcome,
    verdict: Verdict,
    now_ms: i64,
    commit: &str,
) {
    for ann in &outcome.annotations {
        let row: CacheRow = row_for(
            ann,
            verdict,
            outcome.verdict.evidence.clone(),
            now_ms,
            commit,
        );
        if let Err(err) = cache.upsert(&row) {
            eprintln!("loom gate: failed to upsert cache row: {err:#}");
        }
    }
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
            let state = Arc::new(StateDb::open(workspace.join(".loom/state.db"))?);
            let style_rules = config.style_rules.clone();
            let workspace_buf = workspace.to_path_buf();
            let logs_root = workspace.join(".loom/logs");
            let phase_when = phase_when_from_env().unwrap_or_else(|| SystemClock::new().wall_now());

            runtime.block_on(async move {
                let bd = BdClient::new();
                let scope = loom_workflow::mint::MintScope::Tree;
                let validator = WorkspaceFindingValidator::new(workspace);
                if labels.len() == 1 {
                    let label = labels.into_iter().next().ok_or_else(|| {
                        anyhow::anyhow!("loom gate mint --tree found no specs to walk")
                    })?;
                    let label_for_sink = label.clone();
                    let logs_root_for_spawn = logs_root.clone();
                    let mut walker = loom_workflow::mint::ProductionMintWalker::new(
                        BdClient::new(),
                        label,
                        workspace_buf,
                        state,
                        manifest,
                        phase_default,
                        move |spawn_cfg: SpawnConfig| {
                            let logs_root = logs_root_for_spawn.clone();
                            let label = label_for_sink.clone();
                            async move {
                                let sink = LogSink::open_phase_at(
                                    &logs_root, &label, "mint", None, phase_when,
                                )
                                .map_err(|e| {
                                    ProtocolError::Io(std::io::Error::other(e.to_string()))
                                })?;
                                let mut output = String::new();
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
                    .with_style_rules(style_rules);
                    return mint_via_walker(
                        &mut walker,
                        &scope,
                        &validator,
                        &bd,
                        &head_commit,
                        &opts,
                    )
                    .await;
                }

                let mut findings = Vec::new();
                for (index, label) in labels.into_iter().enumerate() {
                    let label_for_sink = label.clone();
                    let logs_root_for_spawn = logs_root.clone();
                    let workspace_for_walker = workspace_buf.clone();
                    let state_for_walker = Arc::clone(&state);
                    let manifest_for_walker = Arc::clone(&manifest);
                    let phase_default_for_walker = phase_default.clone();
                    let style_rules_for_walker = style_rules.clone();
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
                            async move {
                                let sink = LogSink::open_phase_at(
                                    &logs_root, &label, "mint", None, phase_when,
                                )
                                .map_err(|e| {
                                    ProtocolError::Io(std::io::Error::other(e.to_string()))
                                })?;
                                let mut output = String::new();
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
                    .with_style_rules(style_rules_for_walker);
                    if index == 0 {
                        for failure in walker.run_verifiers(&scope).await? {
                            findings.push(loom_workflow::mint::walk::verifier_failure_to_finding(
                                failure,
                            )?);
                        }
                    }
                    let stdout = walker.run_rubric(&scope).await?;
                    let parsed = loom_workflow::review::parse_walk_output(
                        &stdout,
                        scope.dispatch_scope(),
                        &validator,
                    )?;
                    findings.extend(parsed);
                }
                Ok(loom_workflow::mint::mint_findings_with_options(
                    &bd,
                    &findings,
                    &head_commit,
                    &opts,
                )
                .await)
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

struct WorkspaceFindingValidator {
    workspace: PathBuf,
}

impl WorkspaceFindingValidator {
    fn new(workspace: &Path) -> Self {
        Self {
            workspace: workspace.to_path_buf(),
        }
    }

    fn spec_path(&self, label: &SpecLabel) -> PathBuf {
        self.workspace.join("specs").join(format!("{label}.md"))
    }

    fn selector_path(target: &str) -> &str {
        let without_hash = target.split_once('#').map_or(target, |(path, _)| path);
        without_hash
            .split_once("::")
            .map_or(without_hash, |(path, _)| path)
    }

    fn target_path_exists(&self, target: &str) -> bool {
        let path = Self::selector_path(target.trim());
        if path.is_empty() {
            return false;
        }
        let candidate = Path::new(path);
        if candidate.is_absolute() {
            candidate.exists()
        } else {
            self.workspace.join(candidate).exists()
        }
    }
}

impl FindingValidator for WorkspaceFindingValidator {
    fn spec_label_is_known(&self, label: &SpecLabel) -> bool {
        self.spec_path(label).is_file()
    }

    fn criterion_anchor_resolves(&self, spec: &SpecLabel, anchor: &str) -> bool {
        let Ok(body) = std::fs::read_to_string(self.spec_path(spec)) else {
            return false;
        };
        body.lines()
            .filter_map(markdown_heading_anchor)
            .any(|candidate| candidate == anchor)
    }

    fn annotation_resolves(&self, target_string: &str) -> bool {
        if self.target_path_exists(target_string) {
            return true;
        }
        let Some(first_token) = target_string.split_whitespace().next() else {
            return false;
        };
        FsCommandResolver::new(&self.workspace).resolves(first_token)
    }

    fn file_exists(&self, path: &str) -> bool {
        self.target_path_exists(path)
    }

    fn invariant_resolves(&self, spec: &SpecLabel, section: &str, tag: &str) -> bool {
        let Ok(body) = std::fs::read_to_string(self.spec_path(spec)) else {
            return false;
        };
        let section_anchor = markdown_slug(section);
        let tag_anchor = markdown_slug(tag);
        body.lines()
            .filter_map(markdown_heading_anchor)
            .any(|candidate| candidate == section_anchor)
            && markdown_slug(&body).contains(&tag_anchor)
    }
}

fn markdown_heading_anchor(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let text = trimmed.strip_prefix('#')?.trim_start_matches('#').trim();
    if text.is_empty() {
        None
    } else {
        Some(markdown_slug(text))
    }
}

fn markdown_slug(input: &str) -> String {
    let mut out = String::new();
    let mut previous_was_separator = true;
    for c in input.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            previous_was_separator = false;
        } else if !previous_was_separator {
            out.push('-');
            previous_was_separator = true;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}

/// Walk-then-mint pipeline seam. `run_gate_mint` constructs the
/// production walker and delegates here so the dispatch path is
/// exercisable under a recording [`MintWalker`] in tests. Per
/// `specs/gate.md` § *Production walker wiring*: findings reach
/// `mint_findings_with_options` only via `mint::walk::walk(walker, …)`;
/// no `Vec::<Finding>::new()` shortcut.
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
    Ok(loom_workflow::mint::mint_findings_with_options(bd, &findings, head_commit, opts).await)
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
    let db = loom_driver::state::StateDb::open(workspace.join(".loom/state.db"))?;
    let config = LoomConfig::load(LoomConfig::resolve_path(workspace))?;
    let report = status::load(&db, config.loom.integration_branch)?;
    print!("{}", status::render(&report));
    Ok(())
}

fn run_use(workspace: &std::path::Path, label: &str) -> anyhow::Result<()> {
    let label = SpecLabel::new(label);
    let db_path = workspace.join(".loom/state.db");
    use_spec::run(workspace, &label, &db_path)?;
    println!("active spec: {label}");
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
    if verbose
        && matches!(
            base,
            loom_render::RenderMode::Pretty | loom_render::RenderMode::Plain
        )
    {
        logs_cmd::ReplayMode::Render(loom_render::RenderMode::Verbose)
    } else {
        logs_cmd::ReplayMode::Render(base)
    }
}

fn run_plan(
    workspace: &std::path::Path,
    new: Option<String>,
    update: Option<String>,
    profile: Option<String>,
    agent_override: Option<AgentKind>,
) -> anyhow::Result<()> {
    let manifest = ProfileImageManifest::from_env()?;
    let mode = plan::parse_mode(new, update)?;
    let report = plan::run(
        workspace,
        plan::PlanOpts {
            mode,
            wrix_bin: std::env::var_os("LOOM_WRIX_BIN").map(PathBuf::from),
            cli_profile: profile.map(ProfileName::new),
            agent_override,
            manifest,
        },
    )?;
    println!("loom plan: spec={}", report.spec_path.display());
    if report.companion_paths.is_empty() {
        if report.companions_section_present {
            println!("  companions: (none)");
        } else {
            println!("  companions: (none — interview did not declare companions)");
        }
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
    once: bool,
    parallel: Parallelism,
    profile: Option<String>,
    spec: Option<String>,
    agent_override: Option<AgentKind>,
    render_flags: RenderFlags,
) -> anyhow::Result<LoopOutcome> {
    let manifest = Arc::new(ProfileImageManifest::from_env()?);
    let label = resolve_spec_label(workspace, spec)?;
    let lock_mgr = LockManager::new(workspace)?;
    let guard = lock_mgr.acquire_spec(&label)?;

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
    let selection = resolved_agent_for(&config, agent_override, Phase::Run)?;
    let phase_default = selection.profile.clone();
    let cli_profile = profile.map(ProfileName::new);

    let loom_bin = current_loom_bin()?;
    let runtime = tokio::runtime::Runtime::new()?;

    // Per-bead-close GC. Under the spec advisory lock (held via `guard`),
    // reap closed bead workspaces only for the molecule this loop owns.
    // Errors are log-and-continue; a stuck sweep must not block dispatch.
    let gc_git = GitClient::open(workspace)?;
    let gc_workspace = workspace.to_path_buf();
    let gc_molecule = runtime.block_on(async {
        let bd = BdClient::new();
        loom_workflow::resolve::resolve_open_epic(&bd, &label).await
    })?;
    if let Some(gc_molecule) = gc_molecule {
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
    }

    // Reconcile the integration line with published HEAD before any bead
    // clone is materialized, so `loom/<id>` always branches off
    // origin/<integration-branch>. A diverged line (local commits never
    // pushed) fails loud here rather than seeding every bead with a stale
    // base — per `specs/harness.md` § Bead dispatch.
    let ff_git =
        GitClient::open_with_integration_branch(workspace, config.loom.integration_branch.clone())?;
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

    if !parallel.is_one() {
        let parallel_n = parallel.get();
        let workspace_buf = workspace.to_path_buf();
        let label_for_async = label.clone();
        let manifest_for_async = Arc::clone(&manifest);
        let cli_profile_for_async = cli_profile.clone();
        let phase_default_for_async = phase_default.clone();
        let kind = selection.kind;
        let shutdown_grace = resolve_shutdown_grace(&selection);
        let style_rules_for_async = config.style_rules.clone();
        let loom_cfg_for_async = config.loom.clone();
        let outcome = runtime.block_on(async move {
            run_parallel_loop(
                workspace_buf,
                label_for_async,
                parallel_n,
                kind,
                shutdown_grace,
                manifest_for_async,
                cli_profile_for_async,
                phase_default_for_async,
                style_rules_for_async,
                loom_cfg_for_async,
            )
            .await
        })?;
        println!(
            "loom loop --parallel {parallel_n}: processed {}, gate={}",
            outcome.beads_processed,
            gate_label(&outcome.gate),
        );
        return Ok(outcome);
    }

    let mode = if once {
        LoopMode::Once
    } else {
        LoopMode::Continuous
    };
    let manifest_for_seq = Arc::clone(&manifest);
    let kind = selection.kind;
    let shutdown_grace = resolve_shutdown_grace(&selection);
    let workspace_buf = workspace.to_path_buf();
    let workspace_for_renderer = workspace.to_path_buf();
    let logs_root = workspace.join(".loom/logs");
    let logs_root_for_controller = logs_root.clone();
    let label_for_sink = label.clone();
    let render_mode = resolve_render_mode(render_flags);
    let style_rules_for_run = config.style_rules.clone();
    let loom_cfg_for_run = config.loom.clone();
    let observer_config = config.agent.clone();
    let retry_policy = RetryPolicy {
        max_retries: config.loop_.max_retries,
    };
    let max_iterations = config.loop_.max_iterations;
    let git =
        GitClient::open_with_integration_branch(workspace, config.loom.integration_branch.clone())?
            .with_hook_timeout(config.loom.git_hook_timeout());
    let summary = runtime.block_on(async move {
        let bd = BdClient::new();
        let mut controller = ProductionAgentLoopController::new(
            bd,
            label.clone(),
            loom_bin,
            workspace_buf,
            git,
            manifest_for_seq,
            cli_profile,
            phase_default,
            move |spawn_cfg: SpawnConfig, bead_id: BeadId| {
                let logs_root = logs_root.clone();
                let label = label_for_sink.clone();
                let workspace = workspace_for_renderer.clone();
                let observer_config = observer_config.clone();
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
        .with_handoff_lock(guard)
        .with_style_rules(style_rules_for_run)
        .with_loom_config(loom_cfg_for_run)
        .with_phase_log_root(logs_root_for_controller);
        run_loop(&mut controller, mode, retry_policy, max_iterations).await
    })?;
    // The marker is minted inside the molecule-completion push gate's
    // critical section (review_loop's Clean path → `mint_marker` →
    // `git_push`), not here: minting post-loop would seal a marker after
    // the push it is meant to authorize, and outside the section that
    // keeps it bound to the pushed `HEAD` (specs/harness.md § Verdict
    // Gate). See `ProductionReviewController::mint_marker`.
    println!(
        "loom loop: processed {} bead(s), clarified {}, blocked {}, outer_iterations={}, gate={}",
        summary.beads_processed,
        summary.beads_clarified,
        summary.beads_blocked,
        summary.outer_iterations,
        gate_label(&summary.gate),
    );
    Ok(summary)
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
    parallel_n: u32,
    kind: AgentKind,
    shutdown_grace: Option<Duration>,
    manifest: Arc<ProfileImageManifest>,
    cli_profile: Option<ProfileName>,
    phase_default: ProfileName,
    style_rules: String,
    loom_cfg: loom_driver::config::LoomTopConfig,
) -> anyhow::Result<LoopOutcome> {
    use loom_driver::bd::UpdateOpts;
    use loom_workflow::r#loop::AgentOutcome;

    let bd = BdClient::new();
    let beads = bd
        .ready(loom_driver::bd::ReadyOpts {
            limit: Some(parallel_n),
            label: Some(format!("spec:{}", label.as_str())),
            // Dedup of clarify/blocked beads relies on the paired
            // `status=blocked` transition that the apply paths write
            // alongside the label. `bd ready` natively excludes
            // status=blocked, so no exclude-label flag is needed.
            exclude_label: vec![],
        })
        .await?;
    if beads.is_empty() {
        return Ok(LoopOutcome {
            beads_processed: 0,
            beads_clarified: 0,
            beads_blocked: 0,
            outer_iterations: 0,
            gate: GateOutcome::NoGate {
                beads_processed: 0,
                reason: NoGateReason::NoBeadsReady,
            },
        });
    }

    let git = GitClient::open_with_integration_branch(
        workspace.clone(),
        loom_cfg.integration_branch.clone(),
    )?
    .with_hook_timeout(loom_cfg.git_hook_timeout());
    // Resolve the host deploy/signing key paths once for the whole batch —
    // every bead shares the same loom workspace, so the launcher env is
    // identical across slots. Each `wrix spawn` child receives these so the
    // wrapper mounts the keys into the bead container (`specs/harness.md`
    // § Commit signing).
    let launcher_env = git.launcher_key_env()?;
    let logs_root = workspace.join(".loom/logs");
    let logs_root_for_merge = logs_root.clone();
    let label_for_closure = label.clone();
    let workspace_for_closure = workspace.clone();
    let outcome = loom_workflow::r#loop::run_parallel_batch_with_logs(
        &git,
        &label,
        beads,
        Some(&logs_root_for_merge),
        move |slot| {
            let manifest_inner = Arc::clone(&manifest);
            let cli_profile_inner = cli_profile.clone();
            let phase_default_inner = phase_default.clone();
            let logs_root_inner = logs_root.clone();
            let label_inner = label_for_closure.clone();
            let style_rules_inner = style_rules.clone();
            let workspace_inner = workspace_for_closure.clone();
            let loom_cfg_inner = loom_cfg.clone();
            let launcher_env_inner = launcher_env.clone();
            async move {
                // Marker is the primary signal here too — without it, parallel
                // mode would swallow `LOOM_BLOCKED` / `LOOM_CLARIFY` self-reports
                // the same way the sequential path used to.
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
                    launcher_env_inner,
                )
                .await
                {
                    Ok((session, marker)) => match (marker, session.exit_code) {
                        (Some(ExitSignal::Blocked { reason }), _) => {
                            AgentOutcome::Blocked { reason }
                        }
                        (Some(ExitSignal::Clarify { question }), _) => {
                            AgentOutcome::Clarify { question }
                        }
                        (Some(ExitSignal::Retry { reason }), _) => AgentOutcome::Retry { reason },
                        (Some(ExitSignal::Concern { summary }), _) => AgentOutcome::Failure {
                            error: format!(
                                "wrong-phase-marker: LOOM_CONCERN ({summary}) is review-phase only",
                            ),
                        },
                        (Some(ExitSignal::BadWalk(_)), _) => AgentOutcome::Failure {
                            error: "wrong-phase-marker: LOOM_CONCERN is review-phase only"
                                .to_string(),
                        },
                        (Some(ExitSignal::Complete | ExitSignal::Noop), 0) => AgentOutcome::Success,
                        (Some(ExitSignal::Complete | ExitSignal::Noop), code) => {
                            AgentOutcome::Failure {
                                error: format!(
                                    "agent emitted COMPLETE/NOOP but exited code {code}"
                                ),
                            }
                        }
                        (None, 0) => AgentOutcome::Failure {
                            error: "agent exited 0 without LOOM_* marker (swallowed marker)"
                                .to_string(),
                        },
                        (None, code) => AgentOutcome::Failure {
                            error: format!("agent exited with code {code}"),
                        },
                    },
                    Err(e) => AgentOutcome::Failure {
                        error: format!("{e:#}"),
                    },
                }
            }
        },
    )
    .await?;

    // Apply labels for marker self-reports. The bd-side cleanup mirrors the
    // sequential path's `apply_clarify` / `apply_blocked` so a clarify in
    // parallel mode is indistinguishable from one in sequential mode.
    let bd_label = BdClient::new();
    for (bead, question) in outcome.clarified() {
        let notes = if question.is_empty() {
            None
        } else {
            Some(question)
        };
        bd_label
            .update(&bead, parallel_park_update("loom:clarify", notes))
            .await?;
    }
    for (bead, reason) in outcome.blocked() {
        let notes = if reason.is_empty() {
            "agent-blocked".to_string()
        } else {
            format!("agent-blocked: {reason}")
        };
        bd_label
            .update(&bead, parallel_park_update("loom:blocked", Some(notes)))
            .await?;
    }

    // First-conflict integration failures (worktree preserved) get the
    // single-retry marker label so the bead stays ready and the next
    // `loom loop` re-dispatches it against the moved integration tip; a
    // second conflict is read off this label by `merge_back_one` and
    // escalates to `loom:clarify` (handled in the `clarified()` loop above).
    // This is the parallel-shaped home for the serial path's in-process
    // integration-conflict counter (`specs/harness.md` § Verdict Gate
    // phase 3).
    let conflict_ids = outcome.conflict_ids();
    for bead in &conflict_ids {
        tracing::warn!(
            bead = %bead,
            "loom loop: integration conflict — marking for single retry; rerun loom loop to re-dispatch against the moved tip",
        );
        bd_label
            .update(
                bead,
                UpdateOpts {
                    add_labels: vec![loom_workflow::r#loop::CONFLICT_RETRY_LABEL.to_string()],
                    ..UpdateOpts::default()
                },
            )
            .await?;
    }

    let merged = u32::try_from(outcome.merged_ids().len()).unwrap_or(u32::MAX);
    let clarified = u32::try_from(outcome.clarified().len()).unwrap_or(u32::MAX);
    let blocked_n = u32::try_from(outcome.blocked().len()).unwrap_or(u32::MAX);
    let conflict_n = u32::try_from(conflict_ids.len()).unwrap_or(u32::MAX);
    let processed = merged
        .saturating_add(clarified)
        .saturating_add(blocked_n)
        .saturating_add(u32::try_from(outcome.failure_ids().len()).unwrap_or(u32::MAX))
        .saturating_add(conflict_n);
    Ok(LoopOutcome {
        beads_processed: processed,
        beads_clarified: clarified,
        beads_blocked: blocked_n,
        outer_iterations: 0,
        gate: GateOutcome::NoGate {
            beads_processed: processed,
            reason: NoGateReason::OncePartial,
        },
    })
}

/// One slot's dispatch: build the per-bead [`SpawnConfig`] against the
/// slot's worktree and hand it to the same [`dispatch`] match the sequential
/// path uses. The pre-resolved [`AgentKind`] from `run_run` is threaded down
/// — this used to reload `LoomConfig` and re-resolve the backend per slot,
/// which let the sequential and parallel paths drift if the on-disk config
/// changed mid-run. A missing manifest entry surfaces as
/// [`ProfileError::UnknownProfile`] (via `LoopError::Profile`) so the caller
/// converts it to a typed [`AgentOutcome::Failure`] without falling back to
/// a silent default.
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
    launcher_env: Vec<(String, String)>,
) -> anyhow::Result<(SessionOutcome, Option<ExitSignal>)> {
    use loom_driver::scratch::ScratchSession;
    use loom_workflow::r#loop::{
        LoopContextInputs, build_spawn_config_from_manifest, dolt_socket_mount, render_loop_prompt,
        sccache_mount,
    };

    let banner = format!("loom loop @ {}", slot.bead.id);
    let key = resolve_scratch_key(Phase::Run, label, Some(&slot.bead.id));
    let scratchpad_path = ScratchSession::scratchpad_path_for(&slot.worktree.path, &key)
        .to_string_lossy()
        .into_owned();
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
        review_notes: None,
        attempt: 0,
        scratchpad_path,
        style_rules: style_rules.to_string(),
    })?;
    let scratch = ScratchSession::open(&slot.worktree.path, &key, &initial_prompt, &banner)?;
    let mut mounts: Vec<_> = dolt_socket_mount(loom_workspace).into_iter().collect();
    if let Some(spec) = sccache_mount(loom_cfg)? {
        mounts.push(spec);
    }
    let extra_env = loom_cfg.container_sccache_env();
    let spawn_config = build_spawn_config_from_manifest(
        manifest,
        &slot.bead,
        cli_profile,
        phase_default,
        slot.worktree.path.clone(),
        initial_prompt,
        scratch.path().to_path_buf(),
        extra_env,
        vec![],
        mounts,
        launcher_env,
    )?;

    let sink = open_bead_sink(logs_root, label, &slot.bead.id)?;
    let mut output = String::new();
    let result = dispatch(
        kind,
        spawn_config,
        shutdown_grace,
        Some(sink),
        Some(&mut output),
    )
    .await;
    drop(scratch);
    let outcome = result?;
    let marker = parse_exit_signal(&output);
    Ok((outcome, marker))
}

/// Backend-agnostic dispatcher. The match is the only place in the binary
/// that knows the concrete backend types — `run_agent` is monomorphized once
/// per arm at compile time, so the workflow modules never see them.
///
/// `sink` is consumed: ownership crosses into [`run_agent`], which finishes
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
    mut spawn: SpawnConfig,
    shutdown_grace: Option<Duration>,
    sink: Option<LogSink>,
    text_capture: Option<&mut String>,
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
    match kind {
        AgentKind::Pi => run_agent::<PiBackend>(&spawn, sink, text_capture).await,
        AgentKind::Claude => run_agent::<ClaudeBackend>(&spawn, sink, text_capture).await,
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
    }
}

/// Build a per-spawn [`loom_events::EnvelopeBuilder`] so every event the
/// session emits carries the live bead id, monotonic `seq`, and real
/// wall-clock `ts_ms`. The workflow layer joins each `ParsedAgentEvent`
/// with the builder's output via `AgentEvent::from_parsed` (RS-12).
/// `molecule_id` and `iteration` are zero until the driver threads them
/// through.
fn build_envelope_builder(bead_id: BeadId) -> loom_events::EnvelopeBuilder {
    let clock = SystemClock::new();
    loom_events::EnvelopeBuilder::new(bead_id, None, 0, loom_events::Source::Agent, move || {
        clock
            .wall_now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    })
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

/// Open the per-bead JSONL sink at the path the spec promises:
/// `<logs_root>/<spec>/<bead-id>-<utc>.jsonl`. Renderer is `None` because
/// the sequential and parallel run dispatchers run non-interactively (the
/// human-facing summary is written by the `loom loop` outer-loop print).
fn open_bead_sink(
    logs_root: &Path,
    label: &SpecLabel,
    bead_id: &BeadId,
) -> Result<LogSink, ProtocolError> {
    LogSink::open_in_at(
        logs_root,
        label,
        bead_id,
        None,
        SystemClock::new().wall_now(),
    )
    .map_err(|e| ProtocolError::Io(std::io::Error::other(e.to_string())))
}

/// Same as [`open_bead_sink`] but constructs a terminal renderer from
/// the resolved [`RenderMode`] and hands it to the sink so events tee
/// into both the on-disk JSONL and the user's terminal.
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

/// Resolve a [`loom_render::RenderMode`] from the CLI flag tuple and
/// the runtime TTY / `NO_COLOR` environment. Spec table:
/// `--raw` > `--json` > `--plain` or non-TTY or `NO_COLOR` > Pretty.
fn resolve_render_mode(flags: RenderFlags) -> loom_render::RenderMode {
    let tty = loom_render::in_place::stdout_supports_indicator();
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let base = loom_render::RenderMode::select(tty, no_color, flags.plain, flags.json, flags.raw);
    if flags.verbose
        && matches!(
            base,
            loom_render::RenderMode::Pretty | loom_render::RenderMode::Plain
        )
    {
        loom_render::RenderMode::Verbose
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
        loom_render::RenderMode::Verbose => Box::new(
            loom_render::TerminalRenderer::new(
                std::io::stdout(),
                loom_render::RenderMode::Verbose,
                bead_id.clone(),
                parallel,
                !parallel,
            )
            .with_osc8(osc8),
        ),
        loom_render::RenderMode::Pretty | loom_render::RenderMode::Default => Box::new(
            loom_render::TerminalRenderer::new(
                std::io::stdout(),
                loom_render::RenderMode::Default,
                bead_id.clone(),
                parallel,
                true,
            )
            .with_osc8(osc8),
        ),
        loom_render::RenderMode::Plain => Box::new(loom_render::TerminalRenderer::new(
            std::io::stdout(),
            loom_render::RenderMode::Default,
            bead_id.clone(),
            parallel,
            false,
        )),
        loom_render::RenderMode::Json => {
            Box::new(loom_render::JsonRenderer::new(std::io::stdout()))
        }
        loom_render::RenderMode::Raw => Box::new(loom_render::RawRenderer::new(std::io::stdout())),
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
            AgentKind::Pi => None,
        };
    }
    Ok(selection)
}

#[expect(
    dead_code,
    reason = "scope flags parsed by clap; consumed once per-bead/diff/tree scoping lands"
)]
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
    let lock_mgr = LockManager::new(workspace)?;
    let guard = lock_mgr.acquire_spec(&label)?;

    let config = LoomConfig::load(LoomConfig::resolve_path(workspace))?;
    let selection = resolved_agent_for(&config, agent_override, Phase::Review)?;
    let phase_default = selection.profile.clone();
    let kind = selection.kind;
    let shutdown_grace = resolve_shutdown_grace(&selection);

    let loom_bin = current_loom_bin()?;
    let state = std::sync::Arc::new(StateDb::open(workspace.join(".loom/state.db"))?);
    let runtime = tokio::runtime::Runtime::new()?;
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
                async move {
                    let sink =
                        LogSink::open_phase_at(&logs_root, &label, "review", None, phase_when)
                            .map_err(|e| ProtocolError::Io(std::io::Error::other(e.to_string())))?;
                    let mut output = String::new();
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
        )
        .with_handoff_lock(guard)
        .with_phase_log(logs_root, phase_when)
        .with_style_rules(style_rules_for_review)
        .with_integration_branch(integration_branch_for_review)
        .with_hook_timeout(hook_timeout_for_review)
        .with_push_range(opts.diff.clone())
        .with_lane(opts.lane)
        .with_dispatch_scope(dispatch_scope)
        .with_suppressions(suppressions_for_review);
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

#[expect(clippy::too_many_arguments, reason = "explicit dispatch surface")]
fn run_msg(
    workspace: &Path,
    spec: Option<String>,
    number: Option<u32>,
    bead: Option<String>,
    option: Option<u32>,
    reply: Option<String>,
    dismiss: bool,
    chat: bool,
    agent_override: Option<AgentKind>,
) -> anyhow::Result<()> {
    let spec_filter = spec.as_deref().map(SpecLabel::new);
    if chat {
        return run_msg_chat(workspace, spec_filter, agent_override);
    }
    let _manifest = ProfileImageManifest::from_env()?;
    if let Some(label) = &spec_filter {
        let lock_mgr = LockManager::new(workspace)?;
        let _guard = lock_mgr.acquire_spec(label)?;
        run_msg_inner(number, bead, option, reply, dismiss, spec_filter)
    } else {
        run_msg_inner(number, bead, option, reply, dismiss, None)
    }
}

/// `loom msg -c [-s <label>]` — interactive Drafter chat session.
///
/// Renders the `msg.md` template against the outstanding `loom:clarify`
/// (and `loom:blocked`) beads, spawns the agent via the same dispatch
/// surface `loom todo` uses, and parses the session's exit signal. Per
/// the spec, `LOOM_COMPLETE` is the only valid terminator — partial
/// progress is clean (remaining clarifies persist for the next call),
/// but `LOOM_BLOCKED`/`LOOM_CLARIFY` from inside the chat surfaces as
/// a hard error.
///
/// The agent writes the resolution note via `bd update <id> --notes "…"`;
/// the driver runs the canonical unblock (`--status=open` plus the
/// matching `--remove-label`) per resolved bead after the session exits,
/// per the persistence-boundary contract — the agent narrates, the
/// driver persists the terminal state transition.
fn run_msg_chat(
    workspace: &Path,
    spec_filter: Option<SpecLabel>,
    agent_override: Option<AgentKind>,
) -> anyhow::Result<()> {
    let manifest = ProfileImageManifest::from_env()?;
    let opts = loom_workflow::msg::chat::ChatOpts {
        spec_filter,
        cli_profile: None,
        agent_override,
        manifest,
        wrix_bin: std::env::var_os("LOOM_WRIX_BIN").map(PathBuf::from),
    };
    let report = loom_workflow::msg::chat::run(workspace, opts)?;
    if report.beads_surfaced == 0 {
        println!("(no outstanding clarify or blocked beads)");
    } else {
        let resolved = report.beads_surfaced.saturating_sub(report.beads_remaining);
        println!(
            "loom msg --chat: surfaced {}, resolved {}, remaining {}",
            report.beads_surfaced, resolved, report.beads_remaining,
        );
    }
    Ok(())
}

fn run_msg_inner(
    number: Option<u32>,
    bead: Option<String>,
    option: Option<u32>,
    reply: Option<String>,
    dismiss: bool,
    spec_filter: Option<SpecLabel>,
) -> anyhow::Result<()> {
    let has_action = option.is_some() || reply.is_some() || dismiss;

    let runtime = tokio::runtime::Runtime::new()?;
    let beads = runtime.block_on(async {
        let bd = BdClient::new();
        bd.list(ListOpts {
            label_any: vec!["loom:clarify".to_string(), "loom:blocked".to_string()],
            ..ListOpts::default()
        })
        .await
    })?;
    let kept = filter_msg_beads(&beads, spec_filter.as_ref());
    let has_target = number.is_some() || bead.is_some();

    if !has_action && !has_target {
        let rows = build_rows(&kept, spec_filter.as_ref());
        if rows.is_empty() {
            println!("(no outstanding clarify or blocked beads)");
            return Ok(());
        }
        for row in rows {
            match row.spec {
                Some(s) => println!(
                    "{:>3}. {} [{}] [spec:{}] {}",
                    row.index,
                    row.bead_id,
                    row.kind.tag(),
                    s,
                    row.summary
                ),
                None => println!(
                    "{:>3}. {} [{}] {}",
                    row.index,
                    row.bead_id,
                    row.kind.tag(),
                    row.summary
                ),
            }
        }
        return Ok(());
    }

    if !has_action {
        let (target, _pos) = resolve_target(&kept, number, bead.as_deref())?;
        let target_bead = kept
            .iter()
            .find(|b| b.id == target)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("bead {target} not in filtered list"))?;
        let kind = kind_of(target_bead).ok_or_else(|| {
            anyhow::anyhow!("bead {target} carries neither loom:clarify nor loom:blocked")
        })?;
        println!("{target} [{}]", kind.tag());
        println!("title: {}", target_bead.title);
        if let Some(label) = spec_label_of(target_bead) {
            println!("spec: {label}");
        }
        println!();
        println!("{}", target_bead.description);
        return Ok(());
    }

    let (target, _pos) = resolve_target(&kept, number, bead.as_deref())?;
    let target_bead = kept
        .iter()
        .find(|b| b.id == target)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("bead {target} not in filtered list"))?;
    let kind = kind_of(target_bead).ok_or_else(|| {
        anyhow::anyhow!("bead {target} carries neither loom:clarify nor loom:blocked")
    })?;
    let label_to_remove = kind.label().to_string();

    if let Some(opt_idx) = option {
        // `-o <int>` strict option lookup: parse the bead's description,
        // require `### Option <int>` to exist, compose the canonical
        // `"Chose option N — title: body"` note. Validation runs before
        // any bd state mutation.
        let note = compose_option_note(
            &target,
            opt_idx,
            target_bead.notes.as_deref(),
            &target_bead.description,
        )?;
        // Single `--notes` payload strips the originating `## Options`
        // block and records the resolution in one atomic update per
        // specs/gate.md § Resolution lifecycle.
        let new_notes = compose_resolved_notes(target_bead.notes.as_deref(), &note);
        let runtime = tokio::runtime::Runtime::new()?;
        let id_clone = target.clone();
        runtime.block_on(async move {
            let bd = BdClient::new();
            bd.update(
                &id_clone,
                UpdateOpts {
                    status: Some("open".to_string()),
                    remove_labels: vec![label_to_remove],
                    notes: Some(new_notes),
                    ..UpdateOpts::default()
                },
            )
            .await
        })?;
        println!("answered {target}: {note}");
        if let Some(label) = spec_label_of(target_bead) {
            println!("resume: loom loop -s {label}");
        }
        return Ok(());
    }

    if let Some(text) = reply {
        // `-r <text>` verbatim: store the raw text on the bead, drop the
        // loom:* label. Works on any bead kind regardless of Options.
        let new_notes = compose_resolved_notes(target_bead.notes.as_deref(), &text);
        let runtime = tokio::runtime::Runtime::new()?;
        let id_clone = target.clone();
        runtime.block_on(async move {
            let bd = BdClient::new();
            bd.update(
                &id_clone,
                UpdateOpts {
                    status: Some("open".to_string()),
                    remove_labels: vec![label_to_remove],
                    notes: Some(new_notes),
                    ..UpdateOpts::default()
                },
            )
            .await
        })?;
        println!("answered {target}: {text}");
        if let Some(label) = spec_label_of(target_bead) {
            println!("resume: loom loop -s {label}");
        }
        return Ok(());
    }

    if dismiss {
        let new_notes = compose_resolved_notes(target_bead.notes.as_deref(), DISMISS_NOTE);
        let runtime = tokio::runtime::Runtime::new()?;
        let id_clone = target.clone();
        runtime.block_on(async move {
            let bd = BdClient::new();
            bd.update(
                &id_clone,
                UpdateOpts {
                    status: Some("open".to_string()),
                    remove_labels: vec![label_to_remove],
                    notes: Some(new_notes),
                    ..UpdateOpts::default()
                },
            )
            .await
        })?;
        println!("dismissed {target}: {DISMISS_NOTE}");
        if let Some(label) = spec_label_of(target_bead) {
            println!("resume: loom loop -s {label}");
        }
    }
    Ok(())
}

fn run_todo(
    workspace: &Path,
    since: Option<String>,
    agent_override: Option<AgentKind>,
) -> anyhow::Result<()> {
    let manifest = Arc::new(ProfileImageManifest::from_env()?);
    let label = resolve_spec_label(workspace, None)?;
    let lock_mgr = LockManager::new(workspace)?;
    let _guard = lock_mgr.acquire_spec(&label)?;

    let config = LoomConfig::load(LoomConfig::resolve_path(workspace))?;
    let selection = resolved_agent_for(&config, agent_override, Phase::Todo)?;
    let phase_default = selection.profile.clone();
    let kind = selection.kind;
    let shutdown_grace = resolve_shutdown_grace(&selection);

    let state = Arc::new(StateDb::open(workspace.join(".loom/state.db"))?);
    let git = Arc::new(GitClient::open_with_integration_branch(
        workspace,
        config.loom.integration_branch.clone(),
    )?);
    let bd = Arc::new(BdClient::new());
    let runtime = tokio::runtime::Runtime::new()?;
    let workspace_buf = workspace.to_path_buf();
    let logs_root = workspace.join(".loom/logs");
    let label_for_sink = label.clone();
    let loom_cfg_for_todo = config.loom.clone();
    let result = runtime.block_on(async move {
        let mut controller = ProductionTodoController::new(
            label,
            workspace_buf,
            state,
            manifest,
            phase_default,
            git,
            bd,
            since,
        )
        .with_loom_config(loom_cfg_for_todo);
        run_todo_workflow(&mut controller, |spawn_cfg: SpawnConfig| async move {
            let sink = LogSink::open_phase_at(
                &logs_root,
                &label_for_sink,
                "todo",
                None,
                SystemClock::new().wall_now(),
            )
            .map_err(|e| ProtocolError::Io(std::io::Error::other(e.to_string())))?;
            let mut output = String::new();
            let outcome = dispatch(
                kind,
                spawn_cfg,
                shutdown_grace,
                Some(sink),
                Some(&mut output),
            )
            .await?;
            let marker = parse_exit_signal(&output);
            Ok((outcome, marker))
        })
        .await
    });
    match result {
        Ok(summary) => {
            println!(
                "loom todo: agent exited {}, cost_usd={:?}",
                summary.exit_code, summary.cost_usd
            );
            Ok(())
        }
        Err(TodoError::MultiSpecCollision { clarify_id }) => {
            println!(
                "loom todo: multi-spec collision detected; loom:clarify bead {clarify_id} created — resolve via `loom msg`",
            );
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

fn resolve_spec_label(workspace: &Path, spec: Option<String>) -> anyhow::Result<SpecLabel> {
    if let Some(s) = spec {
        return Ok(SpecLabel::new(s));
    }
    let db_path = workspace.join(".loom/state.db");
    if db_path.exists() {
        let db = StateDb::open(db_path)?;
        if let Some(label) = db.current_spec()? {
            return Ok(label);
        }
    }
    let labels = resolve_tree_mint_labels(workspace, None)?;
    labels
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no spec files found under specs/"))
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

fn run_spec(workspace: &std::path::Path, deps: bool) -> anyhow::Result<()> {
    let db = loom_driver::state::StateDb::open(workspace.join(".loom/state.db"))?;
    let label = db
        .current_spec()?
        .ok_or_else(|| anyhow::anyhow!("no active spec — run `loom use <label>`"))?;
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
            minted,
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
            minted: 0,
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
            "# Gate\n\n### Findings and Minting\n\n## Out of Scope\n\nloom-runs-podman is not part of this spec.\n",
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
        assert!(!validator.spec_label_is_known(&SpecLabel::new("harness")));
        assert!(!validator.criterion_anchor_resolves(&gate, "missing-anchor"));
        assert!(!validator.file_exists("tests/missing.rs::live"));
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
    /// `mint_findings_with_options`. Drives the helper seam with a
    /// recording [`MintWalker`]; the live-path subprocess verifier
    /// under `specs/gate.md` § *Production walker wiring* pins that
    /// `run_gate_mint` actually reaches the helper, closing off the
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

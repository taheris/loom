//! `loom` CLI binary entry point.
//!
//! Parses command-line arguments and dispatches to the workflow modules in
//! `loom-workflow`. The set of subcommands matches the harness specification:
//! `init`, `status`, `use`, `logs`, `spec`, plus the previously-implemented
//! `run`, `gate`, `msg`. There is no `sync` or `tune` — Askama compiled
//! templates make per-project sync unnecessary (see `specs/harness.md`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

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
    self, BuiltinParser, CacheRow, DispatchOptions, EmptyScope, FsCommandResolver, RunnerSpec,
    RustWorkspaceStubScanner, RustWorkspaceTestResolver, StatusCache, Tier, TierCwds, Verdict,
    render_report, row_for,
};
use loom_workflow::r#loop::{
    GateOutcome, LoopMode, LoopOutcome, NoGateReason, Parallelism, ProductionAgentLoopController,
    RetryPolicy, SessionResult, run_loop,
};
use loom_workflow::msg::{
    DISMISS_NOTE, build_rows, compose_option_note, compose_resolved_notes, filter_msg_beads,
    kind_of, resolve_target, spec_label_of,
};
use loom_workflow::review::{
    IterationCap, ProductionReviewController, ReviewLane, review_loop as run_review_loop,
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
    /// Run every deterministic verifier followed by the LLM rubric
    /// (`verify` then `review`). The full PR-gate path.
    Audit(GateScopeArgs),
    /// Run every `[check]` / `[test]` / `[system]` verifier — cheap
    /// relative to review, expensive relative to status.
    Verify(GateScopeArgs),
    /// Run only `[check]`-tier verifiers (static analysis). Fastest tier.
    Check(GateScopeArgs),
    /// Run only `[test]`-tier verifiers, batched into one runner
    /// subprocess.
    Test(GateScopeArgs),
    /// Run only `[system]`-tier verifiers (containers, packaging,
    /// end-to-end). Slow.
    System(GateScopeArgs),
    /// Run the LLM rubric — criterion-attached judges plus the rubric
    /// walk over the diff. Expensive.
    Review(GateReviewArgs),
    /// Run only criterion-attached `[judge]` verifiers — skips the
    /// rubric walk.
    Judge(GateScopeArgs),
    /// Run only the rubric walk over the diff — skips
    /// criterion-attached judges.
    Rubric(GateScopeArgs),
    /// Walk the rubric and mint a fix-up bead per finding (the act
    /// surface paired with the inspection-only `audit`).
    Mint(GateMintArgs),
}

/// `loom gate mint` arg surface. Extends [`GateScopeArgs`] with the
/// `--dry-run` flag that suppresses `bd create` calls while still
/// running the read-side dedup + lead-resolution queries.
#[derive(Debug, clap::Args)]
struct GateMintArgs {
    #[command(flatten)]
    scope: GateScopeArgs,
    /// Walk the rubric and print proposed bd writes to stdout without
    /// invoking `bd create`. Read-side queries (dedup + lead
    /// resolution) still run.
    #[arg(long)]
    dry_run: bool,
}

/// `loom gate review` arg surface. Extends [`GateScopeArgs`] with the
/// `--verify-exit` flag the molecule-completion handoff uses to thread
/// the parent `loom loop`'s `loom gate verify` exit code into the child
/// (FR9 production wiring requirement — the push gate's four-condition
/// AND must consume the actual verify exit, not the default `None`).
#[derive(Debug, clap::Args)]
struct GateReviewArgs {
    #[command(flatten)]
    scope: GateScopeArgs,
    /// Exit code of a prior `loom gate verify --diff <range>` run.
    /// Threaded from `loom loop`'s molecule-completion handoff so the
    /// push gate's four-condition AND can refuse on a non-zero verify
    /// exit per FR9 condition 2.
    #[arg(long, value_name = "CODE")]
    verify_exit: Option<i32>,
}

#[derive(Debug, clap::Args)]
#[command(group(
    ArgGroup::new("gate_scope")
        .args(["files", "bead", "diff", "tree"])
        .multiple(false)
        .required(false),
))]
struct GateScopeArgs {
    /// Scope to verifiers whose declared inputs intersect this file set.
    #[arg(long, value_name = "PATH", value_delimiter = ',')]
    files: Vec<PathBuf>,
    /// Filter to one spec's criteria. Defaults to `current_spec` when
    /// applicable to the subcommand.
    #[arg(long, short = 's', value_name = "LABEL")]
    spec: Option<String>,
    /// Run one specific verifier by its annotation target (e.g. a
    /// command string for `[check]` / `[system]`, a test path for
    /// `[test]`, a rubric path for `[judge]`).
    selector: Option<String>,
    /// Scope to one bead's success-criteria inputs and the bead's own diff.
    #[arg(long, short = 'b', value_name = "ID")]
    bead: Option<String>,
    /// Scope to a git diff range. Default when no scope flag is set:
    /// `<molecule.base_commit>..HEAD` for the active molecule, else `HEAD`.
    #[arg(long, value_name = "RANGE")]
    diff: Option<String>,
    /// Scope to every file in the workspace (nightly safety net).
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
    /// Initialize the workspace (create `.wrapix/loom/` config + state DB).
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
        /// `<workspace>/config.toml` (default `base`).
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
            // Bare `loom gate` (status read) and the deterministic tier
            // subcommands are read-only relative to workspace state —
            // they parse spec files, run verifiers, and write the local
            // status cache. The LLM-driven `review` / `judge` / `rubric`
            // / `audit` paths spawn agent containers, so they're refused.
            Command::Gate { subcommand: None } => false,
            Command::Gate {
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
        } => run_plan(&workspace, new, update, profile).map(|()| ExitCode::SUCCESS),
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
    let migration = runtime.block_on(async {
        let bd = BdClient::new();
        init::migrate_legacy_worktrees(workspace, &bd).await
    })?;
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
    if !migration.reaped.is_empty() {
        println!(
            "  migrated {} stale bead worktree(s) from pre-loom-workspace layout",
            migration.reaped.len(),
        );
    }
    if !migration.warned.is_empty() {
        println!(
            "  {} stale bead worktree(s) still open — resolve manually:",
            migration.warned.len(),
        );
        for entry in &migration.warned {
            println!(
                "    {} ({}) — `loom msg {id}` or `bd close {id}`",
                entry.path.display(),
                entry.bead_id,
                id = entry.bead_id,
            );
        }
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
    let db = loom_driver::state::StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
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
        None => run_gate_status(workspace),
        Some(GateSubcommand::Verify(mut args)) => {
            apply_default_scope(workspace, &mut args);
            run_gate_verify(workspace, &args)
        }
        Some(GateSubcommand::Check(mut args)) => {
            apply_default_scope(workspace, &mut args);
            run_gate_single_tier(workspace, &args, Tier::Check)
        }
        Some(GateSubcommand::Test(mut args)) => {
            apply_default_scope(workspace, &mut args);
            run_gate_single_tier(workspace, &args, Tier::Test)
        }
        Some(GateSubcommand::System(mut args)) => {
            apply_default_scope(workspace, &mut args);
            run_gate_single_tier(workspace, &args, Tier::System)
        }
        Some(GateSubcommand::Audit(mut args)) => {
            apply_default_scope(workspace, &mut args);
            run_gate_audit(workspace, args, agent_override)
        }
        Some(GateSubcommand::Review(mut args)) => {
            apply_default_scope(workspace, &mut args.scope);
            run_gate_review(
                workspace,
                args.scope,
                args.verify_exit,
                agent_override,
                ReviewLane::Both,
                Vec::new(),
            )
        }
        Some(GateSubcommand::Judge(mut args)) => {
            apply_default_scope(workspace, &mut args);
            run_gate_review(
                workspace,
                args,
                None,
                agent_override,
                ReviewLane::Judge,
                Vec::new(),
            )
        }
        Some(GateSubcommand::Rubric(mut args)) => {
            apply_default_scope(workspace, &mut args);
            run_gate_review(
                workspace,
                args,
                None,
                agent_override,
                ReviewLane::Rubric,
                Vec::new(),
            )
        }
        Some(GateSubcommand::Mint(mut args)) => {
            apply_default_scope(workspace, &mut args.scope);
            run_gate_mint(workspace, args)
        }
    }
}

/// Resolve the default scope for bare `loom gate <sub>` invocations per
/// `specs/gate.md` § *Default for bare invocation*. When the user
/// supplied none of `--files` / `--bead` / `--diff` / `--tree`, expand
/// to `--diff <molecule.base_commit>..HEAD` for the active molecule
/// bonded to `current_spec`, else `--diff HEAD`. Failure modes
/// (missing state db, missing molecule, `bd` subprocess error) degrade
/// to `HEAD` with a single-line warning so a bare gate invocation
/// remains usable on a fresh workspace.
fn apply_default_scope(workspace: &Path, args: &mut GateScopeArgs) {
    if !args.files.is_empty() || args.bead.is_some() || args.diff.is_some() || args.tree {
        return;
    }
    let base = match active_molecule_base_commit(workspace) {
        Ok(value) => value,
        Err(err) => {
            eprintln!(
                "loom gate: could not resolve active molecule ({err:#}); defaulting to --diff HEAD",
            );
            None
        }
    };
    args.diff = Some(match base {
        Some(commit) => format!("{commit}..HEAD"),
        None => "HEAD".to_owned(),
    });
}

/// Look up `loom.base_commit` for the open epic of `current_spec` via
/// `bd find --type=epic --label=spec:<X> --status=open`. Returns
/// `Ok(None)` for the unconfigured cases (no state db, no current_spec,
/// no open epic, missing metadata key) — those are expected on a fresh
/// workspace, not errors. Subprocess and parse failures propagate.
fn active_molecule_base_commit(workspace: &Path) -> anyhow::Result<Option<String>> {
    let db_path = workspace.join(".wrapix/loom/state.db");
    if !db_path.exists() {
        return Ok(None);
    }
    let db = StateDb::open(db_path)?;
    let Some(label) = db.current_spec()? else {
        return Ok(None);
    };
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        let bd = BdClient::new();
        let Some(epic) = loom_workflow::resolve::resolve_open_epic(&bd, &label).await? else {
            return Ok(None);
        };
        let bead_id = BeadId::new(epic.as_str())?;
        let detail = bd.show(&bead_id).await?;
        Ok(detail
            .metadata
            .get("loom.base_commit")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned))
    })
}

fn gate_dispatch_options(args: &GateScopeArgs) -> DispatchOptions {
    DispatchOptions {
        files: args.files.clone(),
        spec: args.spec.clone(),
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
            args.spec.as_deref().is_none_or(|label| {
                a.source_spec
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s == label)
            })
        })
        .filter(|a| args.selector.as_deref().is_none_or(|sel| a.target == sel))
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
    // `nix build .#wrapixSrc`, which cannot run inside the
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
    let cache_path = workspace.join(".wrapix/loom/gate-cache.sqlite");
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
    let mut combined: i32 = 0;
    for tier in verify_tiers_from_env() {
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

/// Tier loop for `loom gate verify`, scoped by the `LOOM_VERIFY_TIERS`
/// env var when set. The env var is a comma-separated list of tier
/// wire names (`check`, `test`, `system`); unset or empty restores the
/// default of all three. The Nix `tests` derivation uses this to
/// skip `[system]` whose verifiers (e.g. `nix build`, `podman`) are
/// unavailable inside the build sandbox.
fn verify_tiers_from_env() -> Vec<Tier> {
    parse_verify_tiers(std::env::var("LOOM_VERIFY_TIERS").ok().as_deref())
}

fn parse_verify_tiers(raw: Option<&str>) -> Vec<Tier> {
    let default = || vec![Tier::Check, Tier::Test, Tier::System];
    match raw {
        Some(s) if !s.trim().is_empty() => {
            let parsed: Vec<Tier> = s
                .split(',')
                .filter_map(|t| Tier::from_wire(t.trim()))
                .collect();
            if parsed.is_empty() { default() } else { parsed }
        }
        _ => default(),
    }
}

fn run_gate_single_tier(workspace: &Path, args: &GateScopeArgs, tier: Tier) -> anyhow::Result<()> {
    let code = dispatch_tier(workspace, args, tier)?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

/// Resolve the `[test]`-tier runner template. `[runner.test] command =
/// "..."` from `<workspace>/config.toml` (the consolidated `LoomConfig`)
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

/// Resolve the `[check]`-tier runner specs and tier-default cwds from
/// `<workspace>/config.toml`. Empty specs (no `[runner.check]` block,
/// or block with only `cwd`) yields the per-annotation fallback —
/// Returns the runner specs the `[check]`-tier dispatcher consults. A
/// built-in loom-walk batcher is always present so consumers don't have
/// to declare it — every annotation shaped `cargo run -p loom-walk --
/// <name>` collapses into one subprocess (bead `lm-6k4j`). Additional
/// or overriding runners can be layered via `[runner.check.<name>]`
/// entries in `config.toml`. Annotations whose target matches none of
/// the configured runners (grep, bash, misc shell-outs) fall through
/// to per-annotation spawn via `run_with_runners`'s unmatched path.
fn resolve_check_runner_context(workspace: &Path) -> anyhow::Result<(Vec<RunnerSpec>, TierCwds)> {
    let config = LoomConfig::load(LoomConfig::resolve_path(workspace))?;
    let tier_cwds = TierCwds {
        check: tier_cwd(&config, "check"),
        test: tier_cwd(&config, "test"),
        system: tier_cwd(&config, "system"),
        judge: tier_cwd(&config, "judge"),
    };
    let mut specs = vec![builtin_loom_walk_runner()?];
    if let Some(check_tier) = config.runner.tier("check") {
        for (name, entry) in &check_tier.runners {
            specs.push(compile_runner_entry(name, entry)?);
        }
        if let Some(default_entry) = check_tier.default_runner() {
            specs.push(compile_runner_entry("default", &default_entry)?);
        }
    }
    Ok((specs, tier_cwds))
}

/// Built-in batcher for `cargo run -p loom-walk -- <name>` `[check]`
/// annotations. Ships in code (not `config.toml`) so the batching is
/// the default behaviour; operators don't need to add a row to enable
/// it and can't accidentally remove it. Consumers can still layer
/// overrides via `[runner.check.<name>]` entries.
fn builtin_loom_walk_runner() -> anyhow::Result<RunnerSpec> {
    Ok(RunnerSpec::compile(
        "builtin-loom-walk",
        Some(r"^cargo run -p loom-walk -- (\S+)$"),
        "cargo run -p loom-walk -- {targets}",
        "{capture_1}",
        " ",
        BuiltinParser::JsonLines,
        None,
    )?)
}

fn tier_cwd(config: &LoomConfig, tier: &str) -> Option<PathBuf> {
    config
        .runner
        .tier(tier)
        .and_then(|t| t.cwd.clone())
        .map(PathBuf::from)
}

fn compile_runner_entry(
    name: &str,
    entry: &loom_driver::config::RunnerEntry,
) -> anyhow::Result<RunnerSpec> {
    let command = entry
        .command
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("runner `{name}` missing `command`"))?;
    let target = entry.target.as_deref().unwrap_or("{name}");
    let join = entry.join.as_deref().unwrap_or(" ");
    let parse = match entry.parse {
        Some(loom_driver::config::Parser::LibtestJson) => BuiltinParser::LibtestJson,
        Some(loom_driver::config::Parser::JunitXml) => BuiltinParser::JunitXml,
        Some(loom_driver::config::Parser::NixBuildStatus) => BuiltinParser::NixBuildStatus,
        Some(loom_driver::config::Parser::JsonLines) | None => BuiltinParser::JsonLines,
        Some(loom_driver::config::Parser::ExitCode) => BuiltinParser::ExitCode,
    };
    let cwd = entry.cwd.as_deref().map(PathBuf::from);
    Ok(RunnerSpec::compile(
        name,
        entry.match_regex.as_deref(),
        command,
        target,
        join,
        parse,
        cwd,
    )?)
}

fn dispatch_tier(workspace: &Path, args: &GateScopeArgs, tier: Tier) -> anyhow::Result<i32> {
    let specs_dir = workspace.join("specs");
    let parsed = loom_gate::annotation::parse(&specs_dir)?;
    let selected = filter_annotations(&parsed.annotations, tier, args);
    if selected.is_empty() {
        eprintln!("loom gate [{tier}]: no annotations matched");
        return Ok(0);
    }
    let options = gate_dispatch_options(args);
    let cache_path = workspace.join(".wrapix/loom/gate-cache.sqlite");
    let cache = StatusCache::open(&cache_path)?;
    let now_ms = SystemClock::new()
        .wall_now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let commit = current_commit(workspace).unwrap_or_default();

    let mut combined: i32 = 0;
    match tier {
        Tier::Check => {
            combined = combined.max(run_integrity_gate(workspace, args)?);
            let (specs, tier_cwds) = resolve_check_runner_context(workspace)?;
            combined = run_check_with_progress(
                &selected, &options, &specs, workspace, &tier_cwds, &cache, now_ms, &commit,
                combined,
            );
        }
        Tier::System => {
            combined = run_per_annotation_with_progress(
                &selected,
                tier,
                &options,
                &cache,
                now_ms,
                &commit,
                combined,
                loom_gate::run_system,
            );
        }
        Tier::Test => {
            let template = resolve_test_runner_template(workspace)?;
            match loom_gate::run_test(&selected, &options, &template, &EmptyScope) {
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

/// Run the annotation integrity gate. The gate is itself a `[check]`-tier
/// verifier per `specs/gate.md` § Integrity gate — its findings
/// surface alongside every `loom gate check` (and therefore every `loom
/// gate verify`) run. Honours the `--spec <label>` filter; the integrity
/// pass is workspace-scoped and does not narrow by `--diff` / `--files` /
/// `--bead` / `--tree`.
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
    let annotations: Vec<loom_gate::Annotation> = parsed
        .annotations
        .into_iter()
        .filter(|a| {
            args.spec.as_deref().is_none_or(|label| {
                a.source_spec
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s == label)
            })
        })
        .collect();
    if annotations.is_empty() {
        return Ok(0);
    }
    let cmd_resolver = FsCommandResolver::new(workspace);
    let test_resolver = RustWorkspaceTestResolver::scan(workspace)?;
    let stub_scanner = RustWorkspaceStubScanner::scan(workspace)?;
    let findings = loom_gate::integrity::check(
        &annotations,
        workspace,
        &cmd_resolver,
        &test_resolver,
        &stub_scanner,
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

/// Per-annotation dispatch loop for the System tier with fail-eager,
/// pass-silent output. On a TTY, an overwriting status line tracks the
/// currently-running verifier; on a pipe the line is omitted entirely.
/// Each failing verdict and each dispatch error is printed to stderr
/// as soon as the verifier returns.
///
/// `[check]` shares the same output shape (skip / pass-silent /
/// fail-loud), but routes through [`run_check_with_progress`] so the
/// matched-runner batching from `specs/gate.md` § Runners can collapse
/// N walk shell-outs into one subprocess. Function pointer matching
/// the per-tier dispatch runner: takes the selected annotations +
/// dispatch options, returns one result per annotation in the same
/// order.
type DispatchRunner = fn(
    &[loom_gate::Annotation],
    &loom_gate::DispatchOptions,
) -> Vec<Result<loom_gate::DispatchOutcome, loom_gate::DispatchError>>;

/// Batched-aware dispatch loop for the `[check]` tier. Delegates to
/// [`loom_gate::run_check`], which routes matched annotations through
/// [`loom_gate::run_with_runners`] (one subprocess per runner group,
/// per-annotation fallback for unmatched targets) per
/// `specs/gate.md` § Runners. Pass / fail / skip handling mirrors
/// [`run_per_annotation_with_progress`].
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

#[expect(
    clippy::too_many_arguments,
    reason = "progress-driving dispatch surface threads cache + commit + tier together"
)]
fn run_per_annotation_with_progress(
    selected: &[loom_gate::Annotation],
    tier: Tier,
    options: &loom_gate::DispatchOptions,
    cache: &StatusCache,
    now_ms: i64,
    commit: &str,
    mut combined: i32,
    runner: DispatchRunner,
) -> i32 {
    use std::io::{IsTerminal, Write};
    let mut stderr = std::io::stderr();
    let is_tty = stderr.is_terminal();
    let total = selected.len();
    for (i, ann) in selected.iter().enumerate() {
        if is_tty {
            let target = truncate_for_progress(&ann.target, 60);
            let _ = write!(stderr, "\x1b[2K\rrunning [{}/{total}]: {target}", i + 1);
            let _ = stderr.flush();
        }
        let results = runner(std::slice::from_ref(ann), options);
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
    Ok(runtime.block_on(async { git.head_commit_sha().await })?)
}

/// Resolve (or mint) the bonding target for each spec in scope at
/// `--tree` audit time, per `specs/gate.md` § *Standing-safety-net
/// bonding*: zero open epics → mint molecule + epic; one → reuse it;
/// more than one → refuse with the conflicting IDs. Returns `Ok(empty)`
/// when the args are not `--tree`-scoped so the non-tree audit paths
/// stay unchanged. The single-spec form (`--tree --spec <X>`) resolves
/// just that spec; the all-specs sweep (bare `--tree`) walks every
/// `specs/<label>.md` markdown file in the workspace.
fn resolve_tree_scope_bonding_targets(
    workspace: &Path,
    args: &GateScopeArgs,
) -> anyhow::Result<Vec<loom_workflow::resolve::ResolvedEpic>> {
    if !args.tree {
        return Ok(Vec::new());
    }
    let labels = match args.spec.as_deref() {
        Some(label) => vec![SpecLabel::new(label)],
        None => spec_labels_in_workspace(workspace)?,
    };
    if labels.is_empty() {
        return Ok(Vec::new());
    }
    let head = current_commit(workspace).unwrap_or_default();
    let runtime = tokio::runtime::Runtime::new()?;
    let resolved = runtime.block_on(async move {
        let bd = BdClient::new();
        loom_workflow::resolve::resolve_or_mint_open_epics(&bd, &labels, &head).await
    })?;
    for entry in &resolved {
        if entry.was_minted {
            println!(
                "loom gate audit: minted recovery epic {epic} for spec {label} (no open epic existed)",
                epic = entry.molecule_id.as_str(),
                label = entry.label.as_str(),
            );
        }
    }
    Ok(resolved)
}

/// Enumerate every `specs/<label>.md` in the workspace as a [`SpecLabel`].
/// Drives the all-specs sweep of `loom gate audit --tree`.
fn spec_labels_in_workspace(workspace: &Path) -> anyhow::Result<Vec<SpecLabel>> {
    let specs_dir = workspace.join("specs");
    if !specs_dir.exists() {
        return Ok(Vec::new());
    }
    let mut labels = Vec::new();
    for entry in std::fs::read_dir(&specs_dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "md")
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            labels.push(SpecLabel::new(stem));
        }
    }
    labels.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    Ok(labels)
}

/// `loom gate mint` CLI arm. Owns arg parsing, scope resolution
/// (delegated to [`apply_default_scope`] in `run_gate`), the
/// `LOOM_INSIDE` guard (delegated to the top-level main check via
/// [`Command::refused_inside_loom`]), filter passthrough, and exit-code
/// mapping. The walk that produces [`loom_workflow::review::Finding`]
/// records is built by the mint-walk-orchestration bead; this arm
/// plumbs the surface so the orchestration can land on top without
/// touching the CLI again.
fn run_gate_mint(workspace: &Path, args: GateMintArgs) -> anyhow::Result<()> {
    let head_commit = current_commit(workspace).unwrap_or_default();
    let spec_filter = args.scope.spec.as_deref().map(SpecLabel::new);
    let opts = loom_workflow::mint::MintOptions {
        dry_run: args.dry_run,
        spec_filter,
    };
    let findings: Vec<loom_workflow::review::Finding> = Vec::new();
    let runtime = tokio::runtime::Runtime::new()?;
    let summary = runtime.block_on(async move {
        let bd = BdClient::new();
        loom_workflow::mint::mint_findings_with_options(&bd, &findings, &head_commit, &opts).await
    });
    print!("{}", summary.render());
    if summary.refused > 0 || summary.errors > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn run_gate_audit(
    workspace: &Path,
    args: GateScopeArgs,
    agent_override: Option<AgentKind>,
) -> anyhow::Result<()> {
    let tree_scope_epics = resolve_tree_scope_bonding_targets(workspace, &args)?;
    let verify_result = run_gate_verify(workspace, &args);
    // Audit fuses verify+review in one process; pass the verify subcommand's
    // exit code into the review step so the push gate's four-condition AND
    // consumes condition 2 even on the human-invoked audit path.
    let verify_exit = match verify_result.as_ref() {
        Ok(()) => Some(0),
        Err(_) => Some(1),
    };
    let review_result = run_gate_review(
        workspace,
        args,
        verify_exit,
        agent_override,
        ReviewLane::Both,
        tree_scope_epics,
    );
    verify_result.and(review_result)
}

fn run_gate_review(
    workspace: &Path,
    args: GateScopeArgs,
    verify_exit: Option<i32>,
    agent_override: Option<AgentKind>,
    lane: ReviewLane,
    tree_scope_epics: Vec<loom_workflow::resolve::ResolvedEpic>,
) -> anyhow::Result<()> {
    run_review(
        workspace,
        args.spec,
        agent_override,
        ReviewOpts {
            bead: args.bead,
            diff: args.diff,
            tree: args.tree,
            verify_exit,
            lane,
            tree_scope_epics,
        },
    )
}

fn run_status(workspace: &std::path::Path) -> anyhow::Result<()> {
    let db = loom_driver::state::StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    let config = LoomConfig::load(LoomConfig::resolve_path(workspace))?;
    let report = status::load(&db, config.loom.integration_branch)?;
    print!("{}", status::render(&report));
    Ok(())
}

fn run_use(workspace: &std::path::Path, label: &str) -> anyhow::Result<()> {
    let label = SpecLabel::new(label);
    let db_path = workspace.join(".wrapix/loom/state.db");
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
    let logs_root = workspace.join(".wrapix/loom/logs");
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
) -> anyhow::Result<()> {
    let manifest = ProfileImageManifest::from_env()?;
    let mode = plan::parse_mode(new, update)?;
    let report = plan::run(
        workspace,
        plan::PlanOpts {
            mode,
            wrapix_bin: std::env::var_os("LOOM_WRAPIX_BIN").map(PathBuf::from),
            cli_profile: profile.map(ProfileName::new),
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
        &workspace.join(".wrapix/loom/logs"),
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
    // reap bead workspaces under `.wrapix/loom/beads/` whose bead is
    // `closed`. Workspace-global — closed beads cannot be in flight, so
    // the sweep is safe regardless of which spec is being loop'd. Errors
    // are log-and-continue; a stuck sweep must not block dispatch.
    let gc_git = GitClient::open(workspace)?;
    let gc_workspace = workspace.to_path_buf();
    runtime.block_on(async move {
        let bd = BdClient::new();
        match gc_git.sweep_orphan_bead_clones(&bd).await {
            Ok(removed) if !removed.is_empty() => tracing::info!(
                count = removed.len(),
                workspace = %gc_workspace.display(),
                "loom loop startup: reaped closed bead workspaces",
            ),
            Ok(_) => {}
            Err(error) => tracing::warn!(
                %error,
                workspace = %gc_workspace.display(),
                "loom loop startup: orphan-clone sweep failed — continuing",
            ),
        }
    });

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
    let logs_root = workspace.join(".wrapix/loom/logs");
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
        GitClient::open_with_integration_branch(workspace, config.loom.integration_branch.clone())?;
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
                                    error: format!("open log sink: {err}"),
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
        .with_beads_push_program(resolve_beads_push_program())
        .with_phase_log_root(logs_root_for_controller);
        run_loop(&mut controller, mode, retry_policy, max_iterations).await
    })?;
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
/// Resolve the `beads-push` program path. Defaults to `beads-push` on
/// `PATH`; `LOOM_BEADS_PUSH_PROGRAM` overrides it, letting integration
/// tests stub the sync so they don't need a real beads remote in the
/// tempdir.
fn resolve_beads_push_program() -> PathBuf {
    match std::env::var_os("LOOM_BEADS_PUSH_PROGRAM") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => loom_workflow::r#loop::default_beads_push_program(),
    }
}

fn gate_label(gate: &GateOutcome) -> &'static str {
    match gate {
        GateOutcome::Success(_) => "success",
        GateOutcome::Fail(_) => "fail",
        GateOutcome::NoGate { .. } => "no-gate",
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
    )?;
    let logs_root = workspace.join(".wrapix/loom/logs");
    let logs_root_for_merge = logs_root.clone();
    let label_for_closure = label.clone();
    let beads_push_program = resolve_beads_push_program();
    let workspace_for_closure = workspace.clone();
    let outcome = loom_workflow::r#loop::run_parallel_batch_with_logs(
        &git,
        &label,
        beads,
        &beads_push_program,
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
                        error: format!("{e}"),
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
            .update(
                &bead,
                UpdateOpts {
                    add_labels: vec!["loom:clarify".to_string()],
                    notes,
                    ..UpdateOpts::default()
                },
            )
            .await?;
    }
    for (bead, reason) in outcome.blocked() {
        let notes = if reason.is_empty() {
            "agent-blocked".to_string()
        } else {
            format!("agent-blocked: {reason}")
        };
        bd_label
            .update(
                &bead,
                UpdateOpts {
                    add_labels: vec!["loom:blocked".to_string()],
                    notes: Some(notes),
                    ..UpdateOpts::default()
                },
            )
            .await?;
    }

    // Per-bead push failures surface as `PushFailed` (worktree preserved
    // for retry). Log them so the operator sees the divergence between
    // local `main` and the GitHub mirror; a fresh `loom loop` invocation
    // is expected to retry the push.
    for (bead, error) in outcome.push_failed() {
        tracing::warn!(
            bead = %bead,
            %error,
            "loom loop: per-bead push failed — worktree preserved; rerun loom loop to retry the push",
        );
    }

    let merged = u32::try_from(outcome.merged_ids().len()).unwrap_or(u32::MAX);
    let clarified = u32::try_from(outcome.clarified().len()).unwrap_or(u32::MAX);
    let blocked_n = u32::try_from(outcome.blocked().len()).unwrap_or(u32::MAX);
    let push_failed_n = u32::try_from(outcome.push_failed().len()).unwrap_or(u32::MAX);
    let processed = merged
        .saturating_add(clarified)
        .saturating_add(blocked_n)
        .saturating_add(u32::try_from(outcome.failure_ids().len()).unwrap_or(u32::MAX))
        .saturating_add(push_failed_n);
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
    if let Some(spec) = sccache_mount(loom_cfg) {
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
    /// Exit code threaded from `loom loop`'s molecule-completion handoff
    /// (via `loom gate review --verify-exit <CODE>`). `None` when the
    /// gate is invoked standalone; the push gate's other three
    /// conditions still gate the push.
    verify_exit: Option<i32>,
    /// Which lane(s) of the review to run — `Both` for `loom gate review`,
    /// `Judge`/`Rubric` for the focused single-lane re-runs surfaced by
    /// `loom gate judge` / `loom gate rubric`.
    lane: ReviewLane,
    /// Per-spec bonding targets the audit orchestrator pre-resolved (or
    /// minted) before this controller ran. Threaded into the reviewer
    /// prompt at `--tree` scope; empty otherwise.
    tree_scope_epics: Vec<loom_workflow::resolve::ResolvedEpic>,
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
    let state = std::sync::Arc::new(StateDb::open(workspace.join(".wrapix/loom/state.db"))?);
    let runtime = tokio::runtime::Runtime::new()?;
    let workspace_buf = workspace.to_path_buf();
    let logs_root = workspace.join(".wrapix/loom/logs");
    let label_for_sink = label.clone();
    // Pin one phase timestamp so the verdict gate's `push_gate_*`
    // driver events and the reviewer agent's events land in the same
    // JSONL log file. Both writers compute the path from
    // `(logs_root, label, "review", phase_when)`.
    let phase_when = SystemClock::new().wall_now();
    let logs_root_for_spawn = logs_root.clone();
    let style_rules_for_review = config.style_rules.clone();
    let integration_branch_for_review = config.loom.integration_branch.clone();
    let tree_scope_epics = opts
        .tree_scope_epics
        .iter()
        .map(loom_workflow::resolve::tree_scope_epic_from_resolved)
        .collect::<Vec<_>>();
    let _ = opts.tree;
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
                    Ok((outcome, marker, output))
                }
            },
        )
        .with_handoff_lock(guard)
        .with_phase_log(logs_root, phase_when)
        .with_style_rules(style_rules_for_review)
        .with_integration_branch(integration_branch_for_review)
        .with_verify_exit(opts.verify_exit)
        .with_lane(opts.lane)
        .with_tree_scope_epics(tree_scope_epics);
        run_review_loop(&mut controller, IterationCap::default()).await
    })?;
    println!("loom review: {result:?}");
    Ok(())
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
    _agent_override: Option<AgentKind>,
) -> anyhow::Result<()> {
    let manifest = ProfileImageManifest::from_env()?;
    let opts = loom_workflow::msg::chat::ChatOpts {
        spec_filter,
        cli_profile: None,
        manifest,
        wrapix_bin: std::env::var_os("LOOM_WRAPIX_BIN").map(PathBuf::from),
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

    let state = Arc::new(StateDb::open(workspace.join(".wrapix/loom/state.db"))?);
    let git = Arc::new(GitClient::open_with_integration_branch(
        workspace,
        config.loom.integration_branch.clone(),
    )?);
    let bd = Arc::new(BdClient::new());
    let runtime = tokio::runtime::Runtime::new()?;
    let workspace_buf = workspace.to_path_buf();
    let logs_root = workspace.join(".wrapix/loom/logs");
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
    let db = StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    db.current_spec()?.ok_or_else(|| {
        anyhow::anyhow!("no active spec — pass -s <label> or run `loom use <label>`")
    })
}

fn current_loom_bin() -> anyhow::Result<PathBuf> {
    if let Some(bin) = std::env::var_os("LOOM_BIN") {
        return Ok(PathBuf::from(bin));
    }
    Ok(std::env::current_exe()?)
}

fn run_spec(workspace: &std::path::Path, deps: bool) -> anyhow::Result<()> {
    let db = loom_driver::state::StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
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

    #[test]
    fn parse_verify_tiers_defaults_to_all_three_when_unset() {
        assert_eq!(
            parse_verify_tiers(None),
            vec![Tier::Check, Tier::Test, Tier::System]
        );
    }

    #[test]
    fn parse_verify_tiers_defaults_to_all_three_when_empty() {
        assert_eq!(
            parse_verify_tiers(Some("")),
            vec![Tier::Check, Tier::Test, Tier::System]
        );
        assert_eq!(
            parse_verify_tiers(Some("   ")),
            vec![Tier::Check, Tier::Test, Tier::System]
        );
    }

    #[test]
    fn parse_verify_tiers_scopes_to_named_tiers() {
        assert_eq!(
            parse_verify_tiers(Some("check,test")),
            vec![Tier::Check, Tier::Test]
        );
        assert_eq!(parse_verify_tiers(Some("system")), vec![Tier::System]);
    }

    #[test]
    fn parse_verify_tiers_ignores_whitespace_around_names() {
        assert_eq!(
            parse_verify_tiers(Some("  check , test ")),
            vec![Tier::Check, Tier::Test]
        );
    }

    #[test]
    fn parse_verify_tiers_falls_back_to_default_when_all_names_unknown() {
        assert_eq!(
            parse_verify_tiers(Some("Check,Bogus")),
            vec![Tier::Check, Tier::Test, Tier::System]
        );
    }

    fn empty_scope_args() -> GateScopeArgs {
        GateScopeArgs {
            files: Vec::new(),
            spec: None,
            selector: None,
            bead: None,
            diff: None,
            tree: false,
        }
    }

    #[test]
    fn apply_default_scope_falls_back_to_head_on_fresh_workspace() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut args = empty_scope_args();
        apply_default_scope(tmp.path(), &mut args);
        assert_eq!(args.diff.as_deref(), Some("HEAD"));
        assert!(args.bead.is_none());
        assert!(!args.tree);
        assert!(args.files.is_empty());
    }

    #[test]
    fn apply_default_scope_is_noop_when_bead_set() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut args = empty_scope_args();
        args.bead = Some("lm-1".into());
        apply_default_scope(tmp.path(), &mut args);
        assert!(args.diff.is_none());
        assert_eq!(args.bead.as_deref(), Some("lm-1"));
    }

    #[test]
    fn apply_default_scope_is_noop_when_diff_set() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut args = empty_scope_args();
        args.diff = Some("HEAD~3..HEAD".into());
        apply_default_scope(tmp.path(), &mut args);
        assert_eq!(args.diff.as_deref(), Some("HEAD~3..HEAD"));
    }

    #[test]
    fn apply_default_scope_is_noop_when_tree_set() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut args = empty_scope_args();
        args.tree = true;
        apply_default_scope(tmp.path(), &mut args);
        assert!(args.diff.is_none());
        assert!(args.tree);
    }

    #[test]
    fn apply_default_scope_is_noop_when_files_set() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut args = empty_scope_args();
        args.files = vec![PathBuf::from("src/lib.rs")];
        apply_default_scope(tmp.path(), &mut args);
        assert!(args.diff.is_none());
        assert_eq!(args.files, vec![PathBuf::from("src/lib.rs")]);
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
            scope: empty_scope_args(),
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

    /// Spec contract `specs/gate.md` § *Findings and Minting* (criterion
    /// `mint_bare_invocation_defaults_to_active_molecule_diff`): bare
    /// `loom gate mint` (no scope flag) defaults to
    /// `--diff <molecule.base_commit>..HEAD` when the active spec has
    /// an open epic, else `--diff HEAD`. This pins the bare-invocation
    /// branch on a fresh workspace (no state db, no current spec) →
    /// `--diff HEAD`, mirroring `apply_default_scope`'s fallback for
    /// the other gate subcommands.
    #[test]
    fn mint_bare_invocation_defaults_to_active_molecule_diff() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut args = GateMintArgs {
            scope: empty_scope_args(),
            dry_run: false,
        };
        apply_default_scope(tmp.path(), &mut args.scope);
        assert_eq!(args.scope.diff.as_deref(), Some("HEAD"));
        assert!(args.scope.bead.is_none());
        assert!(!args.scope.tree);
        assert!(args.scope.files.is_empty());
    }

    /// Spec contract `specs/gate.md` § *Standing-safety-net bonding*
    /// (criterion `audit_tree_scope_makes_no_bd_writes`): `loom gate
    /// audit --tree` is inspection-only. Pins the property at the
    /// resolver layer the audit path consumes: when every scoped spec
    /// already has exactly one open epic, [`resolve_or_mint_open_epics`]
    /// issues only `bd list` calls — never `bd create`. The audit arm
    /// in `run_gate_audit` calls this resolver, so locking its read-only
    /// behavior here is the regression test for the no-bd-writes
    /// invariant under the steady-state operator scenario.
    #[tokio::test]
    async fn audit_tree_scope_makes_no_bd_writes() {
        use loom_driver::bd::{BdClient, BdError, CommandRunner, RunOutput};
        use std::ffi::OsString;
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        struct ScriptedRunner {
            responses: Mutex<Vec<RunOutput>>,
            invocations: Arc<Mutex<Vec<Vec<OsString>>>>,
        }
        impl CommandRunner for ScriptedRunner {
            async fn run(&self, args: Vec<OsString>, _t: Duration) -> Result<RunOutput, BdError> {
                self.invocations.lock().expect("not poisoned").push(args);
                let mut responses = self.responses.lock().expect("not poisoned");
                assert!(!responses.is_empty(), "no scripted response left");
                Ok(responses.remove(0))
            }
        }

        let epic_row = |id: &str, label: &str| -> String {
            format!(
                r#"{{"id":"{id}","title":"{label}","status":"open","priority":2,"issue_type":"epic","labels":["spec:{label}"]}}"#,
            )
        };

        let labels = vec![SpecLabel::new("alpha"), SpecLabel::new("beta")];
        let invocations = Arc::new(Mutex::new(Vec::new()));
        let runner = ScriptedRunner {
            responses: Mutex::new(vec![
                RunOutput {
                    status: 0,
                    stdout: format!("[{}]", epic_row("lm-alpha", "alpha")).into_bytes(),
                    stderr: Vec::new(),
                },
                RunOutput {
                    status: 0,
                    stdout: format!("[{}]", epic_row("lm-beta", "beta")).into_bytes(),
                    stderr: Vec::new(),
                },
            ]),
            invocations: Arc::clone(&invocations),
        };
        let bd = BdClient::with_runner(runner);
        let resolved = loom_workflow::resolve::resolve_or_mint_open_epics(&bd, &labels, "head-sha")
            .await
            .expect("resolve ok");
        assert_eq!(resolved.len(), 2);
        for entry in &resolved {
            assert!(
                !entry.was_minted,
                "no epic should be minted when one already exists: {entry:?}",
            );
        }
        let calls = invocations.lock().expect("not poisoned");
        for argv in calls.iter() {
            let rendered: Vec<String> = argv
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect();
            assert!(
                !rendered.iter().any(|a| a == "create"),
                "audit --tree bonding-target resolution must NOT invoke bd create: {rendered:?}",
            );
            assert!(
                rendered.iter().any(|a| a == "list"),
                "every recorded bd call must be a read (list): {rendered:?}",
            );
        }
    }
}

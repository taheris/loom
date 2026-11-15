mod catalogue;
mod compiler;
mod fixture;
mod kernel;
mod layer;
mod mutation;
mod native;
mod preparation;
mod process;
mod storage;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::{
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

#[derive(Parser)]
#[command(about = "Run repository verification tooling.")]
struct Arguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check constructor and legacy catalogue equivalence.
    CatalogueReuse {
        candidate: PathBuf,
        baseline: PathBuf,
        source: PathBuf,
        output: PathBuf,
        owned_script: PathBuf,
    },
    /// Check layer planning against the pinned upstream campaign.
    LayerConformance { upstream: PathBuf },
    /// Inject a bridge model fault.
    BridgeFault {
        file: PathBuf,
        #[arg(value_enum)]
        fault: fixture::BridgeFault,
    },
    /// Launch the PID-only procfs diagnostic with explicit groups.
    PidOnlyProcfs {
        #[arg(last = true)]
        arguments: Vec<String>,
    },
    /// Report logical Nix storage without deleting data.
    Storage {
        #[arg(long)]
        dead: bool,
    },
    /// Exercise compact Cargo targets and incremental controls.
    CargoFootprint { manifest: PathBuf },
    /// Exercise writable workspace artifacts without mutating dependencies.
    WritableArtifacts { script: PathBuf, rustc: PathBuf },
    /// Exercise compiler compression and source-line backtraces.
    DebugCompression { hook: PathBuf, rustc: PathBuf },
    /// Exercise cold consumer evaluation and manifest provenance.
    ConsumerApi {
        expression: PathBuf,
        manifest: PathBuf,
        native: PathBuf,
    },
    /// Reject seven type-preserving model mutations using real witnesses.
    QuintMutations {
        ir: PathBuf,
        backend: PathBuf,
        failure: PathBuf,
    },
    /// Reevaluate exact native source partitions.
    NativeResources {
        #[arg(default_value = ".")]
        root: PathBuf,
    },
    /// Plan image layers using the pinned popularity recurrence.
    LayerPlan { attributes: PathBuf },
    /// Check generated kernel object identifiers.
    KernelOid { header: PathBuf, generated: PathBuf },
    /// Prepare or inspect an immutable verification SDK.
    Prepare {
        derivation: String,
        sdk: String,
        #[arg(value_enum)]
        mode: preparation::Mode,
    },
}

fn run(command: Command) -> Result<()> {
    match command {
        Command::CatalogueReuse {
            candidate,
            baseline,
            source,
            output,
            owned_script,
        } => catalogue::run(&candidate, &baseline, &source, &output, &owned_script),
        Command::LayerConformance { upstream } => layer::conformance(&upstream),
        Command::BridgeFault { file, fault } => fixture::bridge(&file, fault),
        Command::PidOnlyProcfs { arguments } => fixture::pid_only_procfs(&arguments),
        Command::Storage { dead } => storage::run(dead),
        Command::CargoFootprint { manifest } => compiler::footprint(&manifest),
        Command::WritableArtifacts { script, rustc } => compiler::writable(&script, &rustc),
        Command::DebugCompression { hook, rustc } => compiler::compression(&hook, &rustc),
        Command::ConsumerApi {
            expression,
            manifest,
            native,
        } => compiler::consumer(&expression, &manifest, &native),
        Command::QuintMutations {
            ir,
            backend,
            failure,
        } => mutation::run(&ir, &backend, &failure),
        Command::NativeResources { root } => native::run(&root),
        Command::LayerPlan { attributes } => {
            let plan = layer::read_plan(&attributes)?;
            serde_json::to_writer(io::stdout().lock(), &plan)?;
            writeln!(io::stdout().lock())?;
            Ok(())
        }
        Command::KernelOid { header, generated } => kernel::check(&header, &generated),
        Command::Prepare {
            derivation,
            sdk,
            mode,
        } => preparation::run(&derivation, &sdk, mode),
    }
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    let preparation = matches!(arguments.command, Command::Prepare { .. });
    match run(arguments.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if writeln!(io::stderr().lock(), "loom-verification: {error:#}").is_err() {
                return ExitCode::FAILURE;
            }
            ExitCode::from(if preparation { 78 } else { 1 })
        }
    }
}

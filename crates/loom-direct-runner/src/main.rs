//! `loom-direct-runner` binary entry point.
//!
//! Parses argv (`--spawn-config <path>`), reads the JSON file into a
//! [`SpawnConfig`], constructs a multi-provider [`Client`], and hands
//! off to [`loom_direct_runner::run_session`].

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use loom_direct_runner::{RunnerError, run_session};
use loom_driver::agent::SpawnConfig;
use loom_llm::client::Client;
use tokio::io::{self, BufReader};
use tracing::{error, info};

/// Env var the host wrapper sets to point at the serialized
/// [`SpawnConfig`] file. Used as the fallback when `--spawn-config` is
/// omitted on the command line.
const ENV_SPAWN_CONFIG: &str = "LOOM_SPAWN_CONFIG";

#[derive(Parser, Debug)]
#[command(
    name = "loom-direct-runner",
    version,
    about = "In-container Direct backend entrypoint."
)]
struct Cli {
    /// Path to the JSON-serialised [`SpawnConfig`] the host wrote at
    /// dispatch time. Falls back to `$LOOM_SPAWN_CONFIG` when omitted.
    #[arg(long)]
    spawn_config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("LOOM_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!(error = %err, "runner exited with error");
            ExitCode::from(1)
        }
    }
}

async fn run(cli: Cli) -> Result<(), RunnerError> {
    let path = cli
        .spawn_config
        .or_else(|| std::env::var_os(ENV_SPAWN_CONFIG).map(PathBuf::from))
        .ok_or_else(|| {
            RunnerError::Io(std::io::Error::other(format!(
                "missing --spawn-config or {ENV_SPAWN_CONFIG}",
            )))
        })?;

    let bytes = std::fs::read(&path).map_err(RunnerError::Io)?;
    let config: SpawnConfig = serde_json::from_slice(&bytes).map_err(RunnerError::EncodeJson)?;
    info!(
        spawn_config = %path.display(),
        workspace = %config.workspace.display(),
        "loom-direct-runner starting",
    );

    let stdin = BufReader::new(io::stdin());
    let stdout = io::stdout();
    let client = Client::new();
    run_session(client, config, stdin, stdout).await
}

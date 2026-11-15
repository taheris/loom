//! Narrow process-side fixture operations for CLI boundary tests.

use anyhow::{Context, Result, ensure};
use std::{
    env,
    io::{self, Write},
    path::Path,
    process::ExitCode,
    time::{Duration, UNIX_EPOCH},
};

pub fn dispatch(arguments: &[String]) -> Result<Option<ExitCode>> {
    let Some(command) = arguments.first().map(String::as_str) else {
        return Ok(None);
    };
    match command {
        "--tune-replay" => return super::replay::run(&arguments[1..]).map(Some),
        "--tune-replay-hold" => {
            ensure!(arguments.len() == 2, "replay hold requires record path");
            super::replay::hold(Path::new(&arguments[1]))?;
        }
        "--review-log-path" => {
            ensure!(arguments.len() == 1, "review log path accepts no arguments");
            let milliseconds = env::var("LOOM_REVIEW_PHASE_WHEN_MILLIS")?.parse::<u64>()?;
            let when = UNIX_EPOCH
                .checked_add(Duration::from_millis(milliseconds))
                .context("review timestamp out of range")?;
            let path =
                loom_driver::logging::phase_log_path(Path::new(".loom/logs"), "review", when);
            writeln!(io::stdout().lock(), "{}", path.display())?;
        }
        "--reject-publication-receipt" => {
            ensure!(
                arguments.len() == 2,
                "receipt failure injection requires database path"
            );
            let connection = rusqlite::Connection::open(&arguments[1])?;
            connection.execute_batch("CREATE TRIGGER reject_receipt BEFORE INSERT ON workflow_records WHEN NEW.kind='publication-receipt' BEGIN SELECT RAISE(FAIL, 'injected receipt failure'); END;")?;
        }
        _ => return Ok(None),
    }
    Ok(Some(ExitCode::SUCCESS))
}

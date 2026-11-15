use anyhow::{Context, Result, ensure};
use std::process::{Command, Output};

pub fn capture(command: &Command, seconds: u32) -> Result<Output> {
    let mut bounded = Command::new("timeout");
    bounded.args(["--kill-after=5s", &format!("{seconds}s")]);
    bounded.arg(command.get_program()).args(command.get_args());
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => {
                bounded.env(key, value);
            }
            None => {
                bounded.env_remove(key);
            }
        }
    }
    if let Some(directory) = command.get_current_dir() {
        bounded.current_dir(directory);
    }
    bounded
        .output()
        .with_context(|| format!("executing {command:?}"))
}

pub fn success(command: &Command, seconds: u32) -> Result<Output> {
    let output = capture(command, seconds)?;
    ensure!(
        output.status.success(),
        "{command:?} failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output)
}

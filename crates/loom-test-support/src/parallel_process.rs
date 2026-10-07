//! Synchronized libtest subprocesses for shared-file concurrency regressions.

use std::io::{self, BufRead, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};

const TEST: &str = "LOOM_PARALLEL_PROCESS_TEST";
const ROOT: &str = "LOOM_PARALLEL_PROCESS_ROOT";
const WORKER: &str = "LOOM_PARALLEL_PROCESS_WORKER";
const READY: &str = "loom-parallel-process-ready";

/// Runs the callback in independent processes after every worker is ready.
/// Returns true in the parent and false in workers; all workers must succeed.
///
/// # Errors
/// Returns an error for failed process setup, synchronization, or callbacks.
pub fn run(
    test_name: &str,
    root: &Path,
    workers: usize,
    operation: impl FnOnce(&Path, usize) -> io::Result<()>,
) -> io::Result<bool> {
    if std::env::var(TEST).as_deref() == Ok(test_name) {
        let root = std::env::var_os(ROOT)
            .ok_or_else(|| io::Error::other("missing parallel-process root"))?;
        let worker = std::env::var(WORKER)
            .map_err(io::Error::other)?
            .parse()
            .map_err(io::Error::other)?;
        writeln!(io::stdout(), "{READY}")?;
        io::stdout().flush()?;
        let mut start = [0];
        io::stdin().read_exact(&mut start)?;
        operation(Path::new(&root), worker)?;
        return Ok(false);
    }

    let mut children = Children(Vec::new());
    for worker in 0..workers {
        children.0.push(
            Command::new(std::env::current_exe()?)
                .args(["--exact", test_name, "--nocapture"])
                .env(TEST, test_name)
                .env(ROOT, root)
                .env(WORKER, worker.to_string())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
        );
    }
    for child in &mut children.0 {
        let stdout = child
            .stdout
            .as_mut()
            .ok_or_else(|| io::Error::other("missing worker stdout"))?;
        let mut stdout = io::BufReader::new(stdout);
        loop {
            let mut line = String::new();
            if stdout.read_line(&mut line)? == 0 {
                return Err(io::Error::other("worker exited before readiness"));
            }
            if line.trim_end().ends_with(READY) {
                break;
            }
        }
    }
    for child in &mut children.0 {
        child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("missing worker stdin"))?
            .write_all(&[1])?;
    }
    while let Some(child) = children.0.pop() {
        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "parallel worker failed ({}):\n{}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            )));
        }
    }
    Ok(true)
}

struct Children(Vec<Child>);

impl Drop for Children {
    #[expect(
        clippy::print_stderr,
        reason = "destructor cleanup failures need diagnostics because Drop cannot return errors"
    )]
    fn drop(&mut self) {
        for child in &mut self.0 {
            if let Err(error) = child.kill() {
                eprintln!("parallel-process cleanup: cannot kill worker: {error}");
            }
            if let Err(error) = child.wait() {
                eprintln!("parallel-process cleanup: cannot reap worker: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    /// Independent PIDs and the stdin barrier are the fixture's observable contract.
    #[test]
    fn parallel_process_fixture_executes_every_worker() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        if !super::run(
            "parallel_process::tests::parallel_process_fixture_executes_every_worker",
            dir.path(),
            4,
            |root, worker| {
                std::fs::write(
                    root.join(worker.to_string()),
                    std::process::id().to_string(),
                )
            },
        )? {
            return Ok(());
        }
        let pids = (0..4)
            .map(|worker| std::fs::read_to_string(dir.path().join(worker.to_string())))
            .collect::<std::io::Result<std::collections::BTreeSet<_>>>()?;
        assert_eq!(pids.len(), 4);
        assert!(!pids.contains(&std::process::id().to_string()));
        Ok(())
    }
}

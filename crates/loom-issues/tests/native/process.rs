use loom_issues::beads::{BdError, RunOutput};
use std::{
    ffi::OsString,
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::Stdio,
    time::Duration,
};
use tokio::process::{Child, Command};

const DIAGNOSTIC_BYTES: usize = 64 * 1024;
#[cfg(unix)]
const DIAGNOSTIC_GRACE: Duration = Duration::from_secs(2);

pub async fn run(
    mut command: Command,
    args: &[OsString],
    directory: &Path,
    timeout: Duration,
) -> Result<RunOutput, BdError> {
    let stdout = tempfile::NamedTempFile::new_in(directory).map_err(BdError::Spawn)?;
    let stderr = tempfile::NamedTempFile::new_in(directory).map_err(BdError::Spawn)?;
    command
        .args(args)
        .kill_on_drop(true)
        .stdout(Stdio::from(stdout.reopen().map_err(BdError::Spawn)?))
        .stderr(Stdio::from(stderr.reopen().map_err(BdError::Spawn)?));
    let mut child = command.spawn().map_err(BdError::Spawn)?;
    let status = if let Ok(status) = tokio::time::timeout(timeout, child.wait()).await {
        status.map_err(BdError::Spawn)?
    } else {
        terminate(&mut child).await.map_err(BdError::Cleanup)?;
        return Err(BdError::Timeout {
            args: format!(
                "{args:?}\nstdout:\n{}\nstderr:\n{}",
                excerpt(&capture(stdout).map_err(BdError::Spawn)?),
                excerpt(&capture(stderr).map_err(BdError::Spawn)?)
            ),
        });
    };
    Ok(RunOutput {
        status: status.code().unwrap_or(-1),
        stdout: capture(stdout).map_err(BdError::Spawn)?,
        stderr: capture(stderr).map_err(BdError::Spawn)?,
    })
}

fn capture(mut file: tempfile::NamedTempFile) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn excerpt(bytes: &[u8]) -> String {
    if bytes.len() <= DIAGNOSTIC_BYTES {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let half = DIAGNOSTIC_BYTES / 2;
    format!(
        "{}\n... {} bytes omitted ...\n{}",
        String::from_utf8_lossy(&bytes[..half]),
        bytes.len() - DIAGNOSTIC_BYTES,
        String::from_utf8_lossy(&bytes[bytes.len() - half..])
    )
}

#[cfg(unix)]
async fn terminate(child: &mut Child) -> std::io::Result<()> {
    if let Err(error) = request_diagnostics(child) {
        child.kill().await?;
        return Err(error);
    }
    if let Ok(status) = tokio::time::timeout(DIAGNOSTIC_GRACE, child.wait()).await {
        status.map(|_| ())
    } else {
        child.kill().await
    }
}

#[cfg(unix)]
fn request_diagnostics(child: &Child) -> std::io::Result<()> {
    use rustix::process::{Pid, Signal, kill_process};

    let Some(id) = child.id() else {
        return Ok(());
    };
    let pid = Pid::from_raw(i32::try_from(id).expect("Unix process IDs fit in i32"))
        .expect("a running child has a positive process ID");
    // SIGQUIT asks the pinned Go backend for goroutine stacks before forced termination.
    match kill_process(pid, Signal::QUIT) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(not(unix))]
async fn terminate(child: &mut Child) -> std::io::Result<()> {
    child.kill().await
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use rustix::process::{Pid, WaitOptions, waitpid};
    use tokio::signal::unix::{SignalKind, signal};

    fn probe(directory: &Path, hang: bool) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--ignored",
                "--exact",
                "process::tests::native_process_probe",
                "--nocapture",
            ])
            .env("LOOM_NATIVE_PROBE", directory)
            .env("LOOM_NATIVE_PROBE_HANG", if hang { "1" } else { "0" });
        command
    }

    #[tokio::test]
    #[ignore = "subprocess entrypoint exercised by native runner tests"]
    async fn native_process_probe() {
        let directory = std::env::var_os("LOOM_NATIVE_PROBE").unwrap();
        let directory = Path::new(&directory);
        let mut quit = signal(SignalKind::quit()).unwrap();
        println!("native probe stdout");
        eprintln!("native probe stderr");
        std::fs::write(directory.join("pid"), std::process::id().to_string()).unwrap();
        std::fs::rename(directory.join("pid"), directory.join("ready")).unwrap();
        if std::env::var_os("LOOM_NATIVE_PROBE_HOLD_CHILD").is_some() {
            std::fs::write(directory.join("held"), b"ready").unwrap();
            let started = std::time::Instant::now();
            while !directory.join("release").exists() {
                assert!(started.elapsed() < Duration::from_secs(120));
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            std::fs::write(directory.join("released"), b"ready").unwrap();
            return;
        }
        if std::env::var_os("LOOM_NATIVE_PROBE_HOLD_PARENT").is_some() {
            let _child = probe(directory, false)
                .env_remove("LOOM_NATIVE_PROBE_HOLD_PARENT")
                .env("LOOM_NATIVE_PROBE_HOLD_CHILD", "1")
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            while !directory.join("held").exists() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
        if std::env::var_os("LOOM_NATIVE_PROBE_HANG").unwrap() == "1" {
            quit.recv().await.unwrap();
            eprintln!("native probe diagnostic signal");
            std::fs::write(directory.join("diagnosed"), b"ready").unwrap();
            std::future::pending::<()>().await;
        }
    }

    async fn wait_for(path: &Path, task: &tokio::task::JoinHandle<Result<RunOutput, BdError>>) {
        let started = std::time::Instant::now();
        while !path.exists() {
            assert!(
                !task.is_finished(),
                "probe exited before {}",
                path.display()
            );
            assert!(started.elapsed() < Duration::from_secs(30));
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn native_runner_retains_successful_output_and_status() {
        let directory = tempfile::tempdir().unwrap();
        let output = run(
            probe(directory.path(), false),
            &[],
            directory.path(),
            Duration::from_secs(60),
        )
        .await
        .unwrap();
        assert!(output.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("native probe stdout"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("native probe stderr"));
    }

    #[tokio::test]
    async fn native_runner_does_not_wait_for_descendant_capture_writers() {
        let directory = tempfile::tempdir().unwrap();
        let mut command = probe(directory.path(), false);
        command.env("LOOM_NATIVE_PROBE_HOLD_PARENT", "1");
        let output = run(command, &[], directory.path(), Duration::from_secs(60)).await;
        std::fs::write(directory.path().join("release"), b"ready").unwrap();
        tokio::time::timeout(Duration::from_secs(60), async {
            while !directory.path().join("released").exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(output.unwrap().success());
    }

    #[tokio::test(start_paused = true)]
    async fn native_timeout_keeps_diagnostics_and_reaps_an_unresponsive_child() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_path_buf();
        let task = tokio::spawn(async move {
            run(probe(&path, true), &[], &path, Duration::from_secs(60)).await
        });
        wait_for(&directory.path().join("ready"), &task).await;
        let pid = std::fs::read_to_string(directory.path().join("ready"))
            .unwrap()
            .parse::<i32>()
            .unwrap();
        tokio::time::advance(Duration::from_secs(61)).await;
        wait_for(&directory.path().join("diagnosed"), &task).await;
        assert!(!task.is_finished());
        tokio::time::advance(DIAGNOSTIC_GRACE + Duration::from_secs(1)).await;
        let Err(BdError::Timeout { args }) = task.await.unwrap() else {
            panic!("expected a native timeout");
        };
        assert!(args.contains("native probe stdout"));
        assert!(args.contains("native probe stderr"));
        assert!(args.contains("native probe diagnostic signal"));
        assert!(matches!(
            waitpid(Pid::from_raw(pid), WaitOptions::NOHANG),
            Err(rustix::io::Errno::CHILD)
        ));
    }

    #[test]
    fn native_timeout_diagnostics_bound_output_and_preserve_both_ends() {
        let mut bytes = vec![b'x'; DIAGNOSTIC_BYTES * 3];
        bytes[..5].copy_from_slice(b"start");
        let end = bytes.len();
        bytes[end - 3..].copy_from_slice(b"end");
        let output = excerpt(&bytes);
        assert!(output.starts_with("start"));
        assert!(output.ends_with("end"));
        assert!(output.contains("bytes omitted"));
        assert!(output.len() < DIAGNOSTIC_BYTES + 100);
    }
}

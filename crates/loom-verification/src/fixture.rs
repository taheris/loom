use anyhow::{Result, ensure};
use clap::ValueEnum;
use std::{fs, path::Path};

#[derive(Clone, Copy, ValueEnum)]
pub enum BridgeFault {
    Accepted,
    Unsupported,
    Incomplete,
}

pub fn bridge(path: &Path, fault: BridgeFault) -> Result<()> {
    let (from, to) = match fault {
        BridgeFault::Accepted => ("accepted' = false", "accepted' = true"),
        BridgeFault::Unsupported => ("replayObserve", "unsupported"),
        BridgeFault::Incomplete => (
            "action replayStep = any { replayObserve }",
            "action replayStep = all { false, count' = count, accepted' = accepted }",
        ),
    };
    let original = fs::read_to_string(path)?;
    ensure!(
        original.contains(from),
        "bridge fixture mutation target is absent"
    );
    fs::write(path, original.replace(from, to))?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn pid_only_procfs(arguments: &[String]) -> Result<()> {
    use std::{env, os::unix::process::CommandExt, process::Command};
    let groups = if env::var("NIX_TEST_NONEMPTY_GROUPS").as_deref() == Ok("1") {
        vec![rustix::process::getgid()]
    } else {
        vec![]
    };
    if !groups.is_empty() || !rustix::process::getgroups()?.is_empty() {
        rustix::thread::set_thread_groups(&groups)?;
    }
    Err(Command::new("unshare")
        .args([
            "--user",
            "--map-root-user",
            "--mount",
            "--pid",
            "--fork",
            "bash",
        ])
        .args(arguments)
        .arg("inside")
        .exec()
        .into())
}
#[cfg(not(target_os = "linux"))]
pub fn pid_only_procfs(_: &[String]) -> Result<()> {
    anyhow::bail!("PID-only procfs qualification is Linux-only")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bridge_faults_change_only_the_named_model_surface() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("bridge.qnt");
        let source = "accepted' = false\naction replayStep = any { replayObserve }\n";
        for fault in [
            BridgeFault::Accepted,
            BridgeFault::Unsupported,
            BridgeFault::Incomplete,
        ] {
            fs::write(&path, source).unwrap();
            bridge(&path, fault).unwrap();
            assert_ne!(fs::read_to_string(&path).unwrap(), source);
        }
        fs::write(&path, "missing").unwrap();
        assert!(bridge(&path, BridgeFault::Accepted).is_err());
    }
}

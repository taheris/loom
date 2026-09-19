//! Isolated Wrix command fixtures with real Git and SSH signatures.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

const CHILD_TEST: &str = "LOOM_GIT_POLICY_TEST";
const FIXTURE_ROOT: &str = "LOOM_GIT_POLICY_ROOT";
const IDENTITY: &str = "loom-policy@example.invalid";

pub struct Fixture {
    root: PathBuf,
}

impl Fixture {
    /// Run one libtest case with isolated keys, Git configuration, and Wrix executables.
    /// Returns `Some` only in the child; the parent returns `None` after it passes.
    ///
    /// # Errors
    /// Returns an error if fixture setup, subprocess execution, or the selected test fails.
    pub fn enter(test_name: &str) -> io::Result<Option<Self>> {
        if std::env::var(CHILD_TEST).as_deref() == Ok(test_name) {
            let root = std::env::var_os(FIXTURE_ROOT)
                .ok_or_else(|| io::Error::other("missing Git-policy fixture root"))?;
            let fixture = Self { root: root.into() };
            std::fs::write(fixture.root.join("entered"), test_name)?;
            return Ok(Some(fixture));
        }

        let directory = tempfile::tempdir()?;
        let fixture = Self::create(directory.path())?;
        let mut command = Command::new(std::env::current_exe()?);
        crate::scrub_git_local_env(&mut command);
        let output = command
            .args(["--exact", test_name, "--nocapture"])
            .env(CHILD_TEST, test_name)
            .env(FIXTURE_ROOT, &fixture.root)
            .env("PATH", fixture.path()?)
            .env("HOME", &fixture.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Loom Policy Test")
            .env("GIT_COMMITTER_NAME", "Loom Policy Test")
            .env("GIT_AUTHOR_EMAIL", IDENTITY)
            .env("GIT_COMMITTER_EMAIL", IDENTITY)
            .env("WRIX_DEPLOY_KEY", fixture.deploy_key())
            .env("WRIX_SIGNING_KEY", fixture.signing_key())
            .env_remove("GIT_SSH")
            .env_remove("GIT_SSH_COMMAND")
            .env_remove("SSH_AUTH_SOCK")
            .output()?;
        if !output.status.success() || !fixture.root.join("entered").is_file() {
            return Err(io::Error::other(format!(
                "isolated test {test_name} failed or did not run ({}):\n{}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            )));
        }
        Ok(None)
    }

    fn create(root: &Path) -> io::Result<Self> {
        let fixture = Self { root: root.into() };
        let bin = root.join("bin");
        std::fs::create_dir(&bin)?;
        crate::write_executable_bash_script(fixture.wrix(), include_str!("wrix.sh"))?;
        crate::write_executable_bash_script(bin.join("wrix-git-sign"), include_str!("sign.sh"))?;
        for key in [
            fixture.deploy_key(),
            fixture.signing_key(),
            fixture.untrusted_key(),
        ] {
            generate_key(&key)?;
        }
        Ok(fixture)
    }

    fn path(&self) -> io::Result<std::ffi::OsString> {
        let mut entries = vec![self.root.join("bin")];
        if let Some(path) = std::env::var_os("PATH") {
            entries.extend(std::env::split_paths(&path));
        }
        std::env::join_paths(entries).map_err(io::Error::other)
    }

    pub fn wrix(&self) -> PathBuf {
        self.root.join("bin/wrix")
    }

    pub fn deploy_key(&self) -> PathBuf {
        self.root.join("repo-key")
    }

    pub fn signing_key(&self) -> PathBuf {
        self.root.join("repo-key-signing")
    }

    pub fn untrusted_key(&self) -> PathBuf {
        self.root.join("untrusted-key")
    }
}

fn generate_key(path: &Path) -> io::Result<()> {
    let output = Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-N", "", "-q", "-C", "", "-f"])
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "ssh-keygen failed: {}",
            String::from_utf8_lossy(&output.stderr),
        )));
    }
    Ok(())
}

use std::path::Path;
use std::time::Duration;

use crate::clock::Clock;

use super::{GitClient, GitError, GitOid};

/// Initialize an independent, deterministic replay repository from already-copied files.
///
/// The directory must not contain Git metadata. No remotes, templates, signing,
/// or operator Git configuration are inherited; replay commits are never published.
///
/// # Errors
/// Returns an error if metadata already exists or repository initialization fails.
pub async fn initialize_snapshot(workspace: &Path, clock: &dyn Clock) -> Result<GitOid, GitError> {
    std::fs::create_dir(workspace.join(".git"))?;
    let commands: &[&[&str]] = &[
        &["init", "--quiet", "--template=", "--initial-branch=main"],
        &["config", "core.hooksPath", ".git/hooks"],
        &["config", "commit.gpgSign", "false"],
        &["config", "user.name", "Loom Replay"],
        &["config", "user.email", "loom-replay@example.invalid"],
        &["add", "--all", "--force"],
        &["commit", "--quiet", "--allow-empty", "-m", "Replay fixture"],
    ];
    for args in commands {
        let mut command = super::environment::tokio_git_command();
        command
            .current_dir(workspace)
            .args(*args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                workspace.join(".git/absent-global-config"),
            )
            .env("GIT_AUTHOR_NAME", "Loom Replay")
            .env("GIT_AUTHOR_EMAIL", "loom-replay@example.invalid")
            .env("GIT_COMMITTER_NAME", "Loom Replay")
            .env("GIT_COMMITTER_EMAIL", "loom-replay@example.invalid")
            .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z");
        let output = crate::process::output(&mut command, clock, Duration::from_mins(1))
            .await
            .map_err(|error| match error {
                crate::process::Error::Io(source) | crate::process::Error::Cleanup(source) => {
                    GitError::Io(source)
                }
                crate::process::Error::Timeout => GitError::GitTimeout {
                    args: args.join(" "),
                    timeout_secs: 60,
                    workdir: workspace.to_path_buf(),
                },
            })?;
        if !output.status.success() {
            return Err(GitError::GitCli {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
    }
    GitClient::open(workspace)?.head_commit_sha().await
}

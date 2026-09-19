use std::path::Path;
use std::process::{Command, Output};

use anyhow::{Result, ensure};
use loom_test_support::git_policy::Fixture;
use tempfile::TempDir;

fn git(repo: &Path, args: &[&str]) -> Result<Output> {
    let mut command = Command::new("git");
    loom_test_support::scrub_git_local_env(&mut command);
    Ok(command.arg("-C").arg(repo).args(args).output()?)
}

fn successful_git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = git(repo, args)?;
    ensure!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    Ok(String::from_utf8(output.stdout)?.trim().into())
}

fn repository(fixture: &Fixture) -> Result<TempDir> {
    let repo = tempfile::tempdir()?;
    successful_git(repo.path(), &["init", "-q", "-b", "main"])?;
    let output = Command::new(fixture.wrix())
        .current_dir(repo.path())
        .args(["init", "--offline", "--no-hooks", "--key", "repo-key"])
        .output()?;
    ensure!(output.status.success(), "fixture init: {output:?}");
    Ok(repo)
}

#[test]
fn wrix_fixture_installs_context_stable_policy() -> Result<()> {
    let Some(fixture) = Fixture::enter("wrix_fixture_installs_context_stable_policy")? else {
        return Ok(());
    };
    let repo = repository(&fixture)?;
    for (key, expected) in [
        ("gpg.format", "ssh"),
        ("gpg.ssh.program", "wrix-git-sign"),
        ("gpg.ssh.allowedSignersFile", "wrix/allowed_signers"),
        ("user.signingkey", "wrix/signing-key/repo-key-signing"),
        ("commit.gpgsign", "true"),
        ("core.sshCommand", ".git/wrix/git-ssh"),
    ] {
        assert_eq!(
            successful_git(repo.path(), &["config", "--local", "--get", key])?,
            expected,
        );
    }
    let public_key = std::fs::read_to_string(fixture.signing_key().with_extension("pub"))?;
    let allowed = std::fs::read_to_string(repo.path().join(".git/wrix/allowed_signers"))?;
    assert_eq!(
        allowed.trim(),
        format!("loom-policy@example.invalid {}", public_key.trim())
    );
    let config = std::fs::read_to_string(repo.path().join(".git/config"))?;
    assert!(!config.contains(fixture.signing_key().to_string_lossy().as_ref()));
    assert!(!config.contains(fixture.deploy_key().to_string_lossy().as_ref()));
    Ok(())
}

#[test]
fn wrix_fixture_verifies_only_trusted_signatures() -> Result<()> {
    let Some(fixture) = Fixture::enter("wrix_fixture_verifies_only_trusted_signatures")? else {
        return Ok(());
    };
    let repo = repository(&fixture)?;
    successful_git(
        repo.path(),
        &["commit", "--allow-empty", "-q", "-m", "trusted"],
    )?;
    successful_git(repo.path(), &["verify-commit", "HEAD"])?;

    let key_arg = format!("user.signingkey={}", fixture.untrusted_key().display());
    successful_git(
        repo.path(),
        &[
            "-c",
            &key_arg,
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "untrusted",
        ],
    )?;
    assert!(
        !git(repo.path(), &["verify-commit", "HEAD"])?
            .status
            .success()
    );
    Ok(())
}

#[test]
fn isolated_policy_test_rejects_a_missing_case() {
    assert!(Fixture::enter("missing_git_policy_test_case").is_err());
}

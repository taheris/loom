//! Commit-signing + rerere gitconfig writes for loom-materialized
//! workspaces.
//!
//! Mirrors wrapix's host-side signing rule (see `lib/sandbox/linux/default.nix`
//! and `scripts/setup-deploy-key` in the wrapix flake): a two-tier
//! signing-key resolver feeds a local `.git/config` block that makes commit
//! signing non-interactive in the loom workspace and every bead clone. The
//! key resolution and gitconfig writes live here, alongside the rest of the
//! `git` CLI surface, so the `git_client_encapsulation` rule stays satisfied.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use super::client::read_origin_url;
use super::error::GitError;

/// Wrapix convention: the signing identity stamped into the
/// allowed_signers file when `$GIT_AUTHOR_EMAIL` is unset.
const DEFAULT_SIGNING_IDENTITY: &str = "sandbox@wrapix.dev";

/// Basename of the derived allowed_signers file under a workspace's
/// `.git/` directory.
const ALLOWED_SIGNERS_FILE: &str = "loom-allowed-signers";

/// Launcher env var naming the host deploy-key path. Loom sets it on the
/// `wrapix spawn` child process so the wrapper mounts the key into the bead
/// container; it is also the first tier [`resolve_deploy_key`] consults.
pub const WRAPIX_DEPLOY_KEY_ENV: &str = "WRAPIX_DEPLOY_KEY";

/// Launcher env var naming the host signing-key path — the signing-key
/// analogue of [`WRAPIX_DEPLOY_KEY_ENV`].
pub const WRAPIX_SIGNING_KEY_ENV: &str = "WRAPIX_SIGNING_KEY";

/// Resolve the wrapix signing key for loom-materialized workspaces, using
/// the same two-tier precedence wrapix applies host-side:
///
/// 1. `$WRAPIX_SIGNING_KEY` pointing at an existing file. Set-but-missing
///    is a hard error ([`GitError::SigningKeyMissing`]) — a silent fallback
///    would mask a parent-process misconfiguration.
/// 2. `$HOME/.ssh/deploy_keys/<repo>-<host>-signing` when the env var is
///    unset and the file exists. `<repo>` is the repo segment of
///    `origin_dir`'s origin URL (parsed as `github.com[:/]<user>/<repo>`);
///    `<host>` is `hostname -s` (short form, falling back to `hostname`).
///    A non-GitHub origin URL skips the fallback.
/// 3. Otherwise `Ok(None)` — the operator's global `~/.gitconfig` governs.
///
/// `origin_dir` is the loom workspace whose `origin` points at GitHub: for
/// `loom init` that is the freshly-cloned `.loom/integration`; for
/// `GitClient::create_worktree` it is the loom workspace (a bead clone's
/// own `origin` points back at the loom workspace path, not GitHub).
pub fn resolve_signing_key(origin_dir: &Path) -> Result<Option<PathBuf>, GitError> {
    resolve_from(
        &ResolveInputs::for_kind(KeyKind::Signing, origin_dir)?,
        KeyKind::Signing,
    )
}

/// Resolve the wrapix deploy key for the launcher environment, mirroring
/// [`resolve_signing_key`] with the `-signing` suffix dropped: the keyname
/// fallback is `<repo>-<host>` and the env var is `$WRAPIX_DEPLOY_KEY`.
/// Set-but-missing is a hard error ([`GitError::DeployKeyMissing`]). Loom
/// passes the resolved host path to `wrapix spawn` as `$WRAPIX_DEPLOY_KEY`
/// so the launcher mounts the key into the bead container; loom's own git
/// invocations never read it (`specs/harness.md` § Commit signing).
pub fn resolve_deploy_key(origin_dir: &Path) -> Result<Option<PathBuf>, GitError> {
    resolve_from(
        &ResolveInputs::for_kind(KeyKind::Deploy, origin_dir)?,
        KeyKind::Deploy,
    )
}

/// Which wrapix key a resolution targets — selects the env var, the keyname
/// suffix, and the set-but-missing error variant.
#[derive(Clone, Copy)]
enum KeyKind {
    Deploy,
    Signing,
}

impl KeyKind {
    fn env_var(self) -> &'static str {
        match self {
            KeyKind::Deploy => WRAPIX_DEPLOY_KEY_ENV,
            KeyKind::Signing => WRAPIX_SIGNING_KEY_ENV,
        }
    }

    /// Suffix appended to `<repo>-<host>` for the `$HOME/.ssh/deploy_keys`
    /// fallback. Signing keys carry `-signing`; deploy keys carry nothing.
    fn keyname_suffix(self) -> &'static str {
        match self {
            KeyKind::Deploy => "",
            KeyKind::Signing => "-signing",
        }
    }

    fn missing(self, path: PathBuf) -> GitError {
        match self {
            KeyKind::Deploy => GitError::DeployKeyMissing { path },
            KeyKind::Signing => GitError::SigningKeyMissing { path },
        }
    }
}

struct ResolveInputs {
    env_key: Option<OsString>,
    home: Option<PathBuf>,
    origin_url: Option<String>,
    hostname: Option<String>,
}

impl ResolveInputs {
    fn for_kind(kind: KeyKind, origin_dir: &Path) -> Result<Self, GitError> {
        Ok(ResolveInputs {
            env_key: std::env::var_os(kind.env_var()),
            home: std::env::var_os("HOME").map(PathBuf::from),
            origin_url: read_origin_url(origin_dir)?,
            hostname: resolve_hostname(),
        })
    }
}

fn resolve_from(inputs: &ResolveInputs, kind: KeyKind) -> Result<Option<PathBuf>, GitError> {
    if let Some(raw) = &inputs.env_key {
        let path = PathBuf::from(raw);
        if path.is_file() {
            return Ok(Some(path));
        }
        return Err(kind.missing(path));
    }
    let (Some(home), Some(origin_url), Some(host)) =
        (&inputs.home, &inputs.origin_url, &inputs.hostname)
    else {
        return Ok(None);
    };
    let Some(repo) = parse_github_repo(origin_url) else {
        return Ok(None);
    };
    let keyname = format!("{repo}-{host}{}", kind.keyname_suffix());
    let path = home.join(".ssh/deploy_keys").join(keyname);
    if path.is_file() {
        Ok(Some(path))
    } else {
        Ok(None)
    }
}

/// Parse the `<repo>` segment from a GitHub origin URL matching
/// `github.com[:/]<user>/<repo>`, stripping a trailing `.git`. Returns
/// `None` for any URL that does not match the GitHub shape, which the
/// caller treats as "skip the deploy-key fallback".
fn parse_github_repo(url: &str) -> Option<String> {
    let idx = url.find("github.com")?;
    let after = &url[idx + "github.com".len()..];
    let sep = after.chars().next()?;
    if sep != ':' && sep != '/' {
        return None;
    }
    let rest = &after[sep.len_utf8()..];
    let mut parts = rest.splitn(3, '/');
    let user = parts.next()?;
    let repo = parts.next()?;
    if user.is_empty() {
        return None;
    }
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    if repo.is_empty() {
        return None;
    }
    Some(repo.to_string())
}

/// `hostname -s` (short form) with a fallback to bare `hostname`. Returns
/// `None` when neither invocation yields a non-empty name.
fn resolve_hostname() -> Option<String> {
    for args in [&["-s"][..], &[][..]] {
        let Ok(output) = StdCommand::new("hostname").args(args).output() else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

/// Write the signing block into `target_dir`'s local `.git/config` and
/// derive the allowed_signers file at `target_dir/.git/loom-allowed-signers`.
///
/// The block declares `gpg.format=ssh`, `user.signingkey=<signing_key>`,
/// `gpg.ssh.allowedSignersFile=<target_dir>/.git/loom-allowed-signers`, and
/// `commit.gpgsign=true`. Local config beats the operator's `~/.gitconfig`,
/// so this is the sole authority on signing inside the workspace.
///
/// The allowed_signers file is derived with `ssh-keygen -y -f <signing_key>`
/// and prefixed with the wrapix signing identity (`$GIT_AUTHOR_EMAIL`, or
/// `sandbox@wrapix.dev`). It lives under `.git/` so workspace removal cleans
/// it up automatically.
pub fn write_signing_config(target_dir: &Path, signing_key: &Path) -> Result<(), GitError> {
    let allowed_signers_file = target_dir.join(".git").join(ALLOWED_SIGNERS_FILE);
    // `.ok()` discards VarError (unset or non-UTF-8): either case means the
    // wrapix author identity isn't available here, so fall back to the
    // default signing identity — the env-var-default intent, not a swallow.
    let identity = std::env::var("GIT_AUTHOR_EMAIL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_SIGNING_IDENTITY.to_string());
    let pubkey = derive_public_key(signing_key)?;
    std::fs::write(&allowed_signers_file, format!("{identity} {pubkey}\n"))?;

    sync_git_config(target_dir, "gpg.format", "ssh")?;
    sync_git_config(
        target_dir,
        "user.signingkey",
        &signing_key.to_string_lossy(),
    )?;
    sync_git_config(
        target_dir,
        "gpg.ssh.allowedSignersFile",
        &allowed_signers_file.to_string_lossy(),
    )?;
    sync_git_config(target_dir, "commit.gpgsign", "true")?;
    Ok(())
}

/// Enable rerere in `target_dir`'s local `.git/config`
/// (`rerere.enabled=true`, `rerere.autoupdate=true`) so the driver-side
/// rebase replays previously-recorded conflict resolutions from
/// `<target_dir>/.git/rr-cache/` before falling through to
/// `integration-conflict` recovery. Written only into the loom workspace
/// (bead clones are reaped on `bd close`, so their rerere cache would never
/// transfer).
pub fn enable_rerere(target_dir: &Path) -> Result<(), GitError> {
    sync_git_config(target_dir, "rerere.enabled", "true")?;
    sync_git_config(target_dir, "rerere.autoupdate", "true")?;
    Ok(())
}

/// `ssh-keygen -y -f <signing_key>` — the public half of the signing pair.
/// The wrapix signing key is passphrase-less, so this is non-interactive.
fn derive_public_key(signing_key: &Path) -> Result<String, GitError> {
    let output = StdCommand::new("ssh-keygen")
        .arg("-y")
        .arg("-f")
        .arg(signing_key)
        .output()
        .map_err(GitError::Spawn)?;
    if !output.status.success() {
        return Err(GitError::SshKeygen {
            key: signing_key.to_path_buf(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

/// `git -C <target_dir> config <key> <value>` — writes a local config
/// value. Synchronous: the callers (`loom init`, bead-clone materialization)
/// are one-shot bootstrap paths.
fn sync_git_config(target_dir: &Path, key: &str, value: &str) -> Result<(), GitError> {
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(target_dir)
        .args(["config", key, value])
        .output()
        .map_err(GitError::Spawn)?;
    if output.status.success() {
        return Ok(());
    }
    Err(GitError::GitCli {
        status: output.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_github_repo_handles_ssh_https_and_dot_git() {
        assert_eq!(
            parse_github_repo("git@github.com:wrapix/loom.git").as_deref(),
            Some("loom"),
        );
        assert_eq!(
            parse_github_repo("https://github.com/wrapix/loom").as_deref(),
            Some("loom"),
        );
        assert_eq!(
            parse_github_repo("https://github.com/wrapix/loom.git").as_deref(),
            Some("loom"),
        );
        assert_eq!(
            parse_github_repo("ssh://git@github.com/acme/my-repo.git").as_deref(),
            Some("my-repo"),
        );
    }

    #[test]
    fn parse_github_repo_rejects_non_github() {
        assert_eq!(parse_github_repo("/srv/git/local-bare.git"), None);
        assert_eq!(parse_github_repo("git@gitlab.com:wrapix/loom.git"), None);
        assert_eq!(parse_github_repo("https://example.com/wrapix/loom"), None);
        assert_eq!(parse_github_repo("github.com-no-separator/x/y"), None);
    }

    /// Spec contract `[test]` annotation (`specs/harness.md` § Success
    /// Criteria · Commit signing): the fallback keyname is derived as
    /// `<repo>-<host>-signing` where `<repo>` comes from the origin URL and
    /// `<host>` is the short hostname.
    #[test]
    fn signing_key_fallback_uses_wrapix_repo_host_derivation() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let deploy = home.join(".ssh/deploy_keys");
        std::fs::create_dir_all(&deploy).unwrap();
        let key = deploy.join("loom-buildbox-signing");
        std::fs::write(&key, "PRIVATE KEY\n").unwrap();

        let inputs = ResolveInputs {
            env_key: None,
            home: Some(home.to_path_buf()),
            origin_url: Some("git@github.com:wrapix/loom.git".to_string()),
            hostname: Some("buildbox".to_string()),
        };
        let resolved = resolve_from(&inputs, KeyKind::Signing).unwrap();
        assert_eq!(resolved.as_deref(), Some(key.as_path()));
    }

    #[test]
    fn fallback_skipped_for_non_github_origin() {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("local-bare.git");
        let inputs = ResolveInputs {
            env_key: None,
            home: Some(tmp.path().to_path_buf()),
            origin_url: Some(origin.to_string_lossy().into_owned()),
            hostname: Some("buildbox".to_string()),
        };
        assert_eq!(resolve_from(&inputs, KeyKind::Signing).unwrap(), None);
    }

    #[test]
    fn fallback_none_when_keyfile_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let inputs = ResolveInputs {
            env_key: None,
            home: Some(tmp.path().to_path_buf()),
            origin_url: Some("git@github.com:wrapix/loom.git".to_string()),
            hostname: Some("buildbox".to_string()),
        };
        assert_eq!(resolve_from(&inputs, KeyKind::Signing).unwrap(), None);
    }

    /// Spec contract `[test]` annotation: `$WRAPIX_SIGNING_KEY` pointing at
    /// a non-existent file is a hard error naming the missing path.
    #[test]
    fn wrapix_signing_key_missing_file_fails_loud() {
        let inputs = ResolveInputs {
            env_key: Some(OsString::from("/nonexistent/wrapix-signing-key")),
            home: None,
            origin_url: None,
            hostname: None,
        };
        match resolve_from(&inputs, KeyKind::Signing) {
            Err(GitError::SigningKeyMissing { path }) => {
                assert_eq!(path, PathBuf::from("/nonexistent/wrapix-signing-key"));
            }
            other => panic!("expected SigningKeyMissing, got {other:?}"),
        }
    }

    fn gen_ssh_key(dir: &Path) -> PathBuf {
        let key = dir.join("signing-key");
        let status = StdCommand::new("ssh-keygen")
            .args(["-t", "ed25519", "-N", "", "-q", "-C", "", "-f"])
            .arg(&key)
            .status()
            .unwrap();
        assert!(status.success(), "ssh-keygen must succeed");
        key
    }

    fn init_git_repo(dir: &Path) {
        let status = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(["init", "-q"])
            .status()
            .unwrap();
        assert!(status.success(), "git init must succeed");
    }

    fn git_config_get(dir: &Path, key: &str) -> Option<String> {
        let out = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(["config", "--local", "--get", key])
            .output()
            .unwrap();
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Spec contract `[test]` annotation (`specs/harness.md` § Success
    /// Criteria · Commit signing): `write_signing_config` writes the
    /// `gpg.format=ssh` / `user.signingkey` / `commit.gpgsign=true` /
    /// `gpg.ssh.allowedSignersFile` block into the target's local
    /// `.git/config`.
    #[test]
    fn write_signing_config_writes_expected_block() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path();
        init_git_repo(target);
        let key = gen_ssh_key(tmp.path());

        write_signing_config(target, &key).unwrap();

        assert_eq!(git_config_get(target, "gpg.format").as_deref(), Some("ssh"));
        assert_eq!(
            git_config_get(target, "user.signingkey").as_deref(),
            Some(key.to_string_lossy().as_ref()),
        );
        assert_eq!(
            git_config_get(target, "commit.gpgsign").as_deref(),
            Some("true"),
        );
        let expected_signers = target
            .join(".git")
            .join(ALLOWED_SIGNERS_FILE)
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            git_config_get(target, "gpg.ssh.allowedSignersFile").as_deref(),
            Some(expected_signers.as_str()),
        );
    }

    /// Spec contract `[test]` annotation (`specs/harness.md` § Success
    /// Criteria · Commit signing): the allowed_signers file is derived via
    /// `ssh-keygen -y -f <signing-key>` and prefixed with the wrapix
    /// signing identity.
    #[test]
    fn allowed_signers_derived_from_signing_key() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path();
        init_git_repo(target);
        let key = gen_ssh_key(tmp.path());

        write_signing_config(target, &key).unwrap();

        let signers_path = target.join(".git").join(ALLOWED_SIGNERS_FILE);
        let contents = std::fs::read_to_string(&signers_path).unwrap();

        // The derived public half must match `ssh-keygen -y -f <key>`.
        let pubkey = String::from_utf8(
            StdCommand::new("ssh-keygen")
                .arg("-y")
                .arg("-f")
                .arg(&key)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let pubkey = pubkey.trim();

        let line = contents.trim();
        let (identity, key_field) = line.split_once(' ').unwrap();
        // Default identity when $GIT_AUTHOR_EMAIL is unset (set in some CI
        // envs — accept either the wrapix default or the configured email).
        let expected_identity = std::env::var("GIT_AUTHOR_EMAIL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_SIGNING_IDENTITY.to_string());
        assert_eq!(identity, expected_identity);
        assert_eq!(key_field, pubkey);
    }

    #[test]
    fn wrapix_signing_key_present_takes_precedence() {
        let tmp = tempfile::tempdir().unwrap();
        let key = tmp.path().join("explicit-signing-key");
        std::fs::write(&key, "PRIVATE KEY\n").unwrap();
        let inputs = ResolveInputs {
            // A GitHub origin + present fallback must be ignored when the
            // env var resolves.
            env_key: Some(key.clone().into_os_string()),
            home: Some(tmp.path().to_path_buf()),
            origin_url: Some("git@github.com:wrapix/loom.git".to_string()),
            hostname: Some("buildbox".to_string()),
        };
        assert_eq!(
            resolve_from(&inputs, KeyKind::Signing).unwrap().as_deref(),
            Some(key.as_path())
        );
    }

    /// The deploy-key fallback drops the `-signing` suffix: keyname is
    /// `<repo>-<host>`, mirroring [`signing_key_fallback_uses_wrapix_repo_host_derivation`].
    #[test]
    fn deploy_key_fallback_omits_signing_suffix() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let deploy = home.join(".ssh/deploy_keys");
        std::fs::create_dir_all(&deploy).unwrap();
        let key = deploy.join("loom-buildbox");
        std::fs::write(&key, "PRIVATE KEY\n").unwrap();

        let inputs = ResolveInputs {
            env_key: None,
            home: Some(home.to_path_buf()),
            origin_url: Some("git@github.com:wrapix/loom.git".to_string()),
            hostname: Some("buildbox".to_string()),
        };
        let resolved = resolve_from(&inputs, KeyKind::Deploy).unwrap();
        assert_eq!(resolved.as_deref(), Some(key.as_path()));
    }

    /// `$WRAPIX_DEPLOY_KEY` pointing at a non-existent file is a hard error
    /// naming the missing path — the deploy-key analogue of
    /// [`wrapix_signing_key_missing_file_fails_loud`].
    #[test]
    fn wrapix_deploy_key_missing_file_fails_loud() {
        let inputs = ResolveInputs {
            env_key: Some(OsString::from("/nonexistent/wrapix-deploy-key")),
            home: None,
            origin_url: None,
            hostname: None,
        };
        match resolve_from(&inputs, KeyKind::Deploy) {
            Err(GitError::DeployKeyMissing { path }) => {
                assert_eq!(path, PathBuf::from("/nonexistent/wrapix-deploy-key"));
            }
            other => panic!("expected DeployKeyMissing, got {other:?}"),
        }
    }
}

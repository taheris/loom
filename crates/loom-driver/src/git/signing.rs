//! Repository Git isolation and rerere configuration.
//!
//! `loom loop` resolves repository keys before bead selection, then delegates
//! repository-local transport and signing policy to `wrix init`. Wrix helper
//! tokens are stable across the host and container contexts in which bead
//! clones run; private host paths are passed only to Wrix child processes.
//!
//! Legacy absolute-path config is only removed during migration; host-only
//! signing writers are test fixtures. Keeping all Git CLI access here
//! preserves the `git_client_encapsulation` rule.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};

use super::client::read_origin_url;
use super::error::GitError;

/// Wrix convention used by host-only signing test fixtures.
#[cfg(any(test, feature = "test-support"))]
const DEFAULT_SIGNING_IDENTITY: &str = "sandbox@wrix.dev";

/// Basename of the derived allowed_signers file under a workspace's
/// `.git/` directory.
const ALLOWED_SIGNERS_FILE: &str = "loom-allowed-signers";
const GIT_CONFIG_KEY_NOT_FOUND_STATUS: i32 = 5;

/// Launcher env var naming the host deploy-key path. Loom sets it on the
/// `wrix spawn` child process so the wrapper mounts the key into the bead
/// container; it is also the first tier [`resolve_deploy_key`] consults.
pub const WRIX_DEPLOY_KEY_ENV: &str = "WRIX_DEPLOY_KEY";

/// Launcher env var naming the host signing-key path — the signing-key
/// analogue of [`WRIX_DEPLOY_KEY_ENV`].
pub const WRIX_SIGNING_KEY_ENV: &str = "WRIX_SIGNING_KEY";

/// Signing authority for Loom-managed Git processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyMode {
    /// Require and use repository-scoped Wrix deploy and signing keys.
    Repository,
    /// Explicitly permit the operator's ambient Git signing configuration.
    Host,
}

/// Resolved Git policy installed into Loom's integration and bead clones.
#[derive(Debug, Clone)]
pub struct RepoGitPolicy {
    mode: KeyMode,
    wrix_bin: PathBuf,
    key_name: Option<String>,
    deploy_key: Option<PathBuf>,
    signing_key: Option<PathBuf>,
}

impl RepoGitPolicy {
    /// Resolve the repository keys required by `loom loop`.
    pub fn resolve(origin_dir: &Path, wrix_bin: PathBuf, mode: KeyMode) -> Result<Self, GitError> {
        if mode == KeyMode::Host {
            return Ok(Self {
                mode,
                wrix_bin,
                key_name: None,
                deploy_key: None,
                signing_key: None,
            });
        }

        reject_ambient_transport_overrides([
            ("GIT_SSH_COMMAND", std::env::var_os("GIT_SSH_COMMAND")),
            ("GIT_SSH", std::env::var_os("GIT_SSH")),
        ])?;
        let deploy_key = require_absolute_key_path(
            resolve_deploy_key(origin_dir)?.ok_or(GitError::RepositoryDeployKeyRequired)?,
        )?;
        let signing_key = require_absolute_key_path(
            resolve_signing_key(origin_dir)?.ok_or(GitError::RepositorySigningKeyRequired)?,
        )?;
        let key_name = deploy_key
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| GitError::RepositoryKeyName {
                path: deploy_key.clone(),
            })?
            .to_string();

        Ok(Self {
            mode,
            wrix_bin,
            key_name: Some(key_name),
            deploy_key: Some(deploy_key),
            signing_key: Some(signing_key),
        })
    }

    /// Build an explicit repository policy for cross-crate tests.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn for_test(
        wrix_bin: PathBuf,
        key_name: String,
        deploy_key: PathBuf,
        signing_key: PathBuf,
    ) -> Self {
        Self {
            mode: KeyMode::Repository,
            wrix_bin,
            key_name: Some(key_name),
            deploy_key: Some(deploy_key),
            signing_key: Some(signing_key),
        }
    }

    /// Install context-stable Wrix Git policy or explicitly restore host policy.
    pub fn apply(&self, workspace: &Path) -> Result<(), GitError> {
        if self.mode == KeyMode::Host {
            return if workspace.join(".git").exists() {
                clear_managed_git_policy(workspace)
            } else {
                Ok(())
            };
        }
        let (Some(key_name), Some(deploy_key), Some(signing_key)) = (
            self.key_name.as_deref(),
            self.deploy_key.as_deref(),
            self.signing_key.as_deref(),
        ) else {
            return Err(GitError::RepositoryPolicyIncomplete);
        };

        remove_legacy_allowed_signers(workspace)?;
        let mut command = StdCommand::new(&self.wrix_bin);
        super::environment::scrub_std_command(&mut command);
        let output = command
            .args(["init", "--offline", "--no-hooks", "--key", key_name])
            .env(WRIX_DEPLOY_KEY_ENV, deploy_key)
            .env(WRIX_SIGNING_KEY_ENV, signing_key)
            .current_dir(workspace)
            .stdin(Stdio::null())
            .output()
            .map_err(|source| GitError::WrixSpawn {
                executable: self.wrix_bin.clone(),
                source,
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            return Err(GitError::WrixInit {
                workdir: workspace.to_path_buf(),
                status: output.status.code().unwrap_or(-1),
                detail: if stderr.is_empty() { stdout } else { stderr },
            });
        }
        validate_wrix_policy(workspace, key_name)
    }

    /// Whether this policy explicitly permits ambient host Git authority.
    pub fn uses_host_key(&self) -> bool {
        self.mode == KeyMode::Host
    }

    /// Host key paths passed only to the Wrix launcher process.
    pub fn launcher_env(&self) -> Vec<(String, String)> {
        let mut env = Vec::new();
        if let Some(key) = &self.signing_key {
            env.push((
                WRIX_SIGNING_KEY_ENV.to_string(),
                key.to_string_lossy().into_owned(),
            ));
        }
        if let Some(key) = &self.deploy_key {
            env.push((
                WRIX_DEPLOY_KEY_ENV.to_string(),
                key.to_string_lossy().into_owned(),
            ));
        }
        env
    }

    /// Allowed-signers file created by `wrix init` for repository mode.
    pub fn allowed_signers_path(&self, workspace: &Path) -> Option<PathBuf> {
        (self.mode == KeyMode::Repository).then(|| workspace.join(".git/wrix/allowed_signers"))
    }
}

fn reject_ambient_transport_overrides(
    overrides: [(&str, Option<OsString>); 2],
) -> Result<(), GitError> {
    for (variable, value) in overrides {
        if value.is_some_and(|value| !value.is_empty()) {
            return Err(GitError::AmbientGitTransportOverride {
                variable: variable.to_string(),
            });
        }
    }
    Ok(())
}

fn require_absolute_key_path(path: PathBuf) -> Result<PathBuf, GitError> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(GitError::RepositoryKeyPathNotAbsolute { path })
    }
}

fn validate_wrix_policy(workspace: &Path, key_name: &str) -> Result<(), GitError> {
    let expected = [
        ("gpg.format", "ssh".to_string()),
        ("gpg.ssh.program", "wrix-git-sign".to_string()),
        (
            "gpg.ssh.allowedSignersFile",
            "wrix/allowed_signers".to_string(),
        ),
        (
            "user.signingkey",
            format!("wrix/signing-key/{key_name}-signing"),
        ),
        ("commit.gpgsign", "true".to_string()),
    ];
    for (key, expected_value) in expected {
        let actual = local_git_config(workspace, key)?;
        if actual.as_deref() != Some(expected_value.as_str()) {
            return Err(invalid_wrix_policy(
                workspace,
                format!("{key} must be `{expected_value}`, found {actual:?}"),
            ));
        }
    }
    let ssh_command = local_git_config(workspace, "core.sshCommand")?;
    if !ssh_command
        .as_deref()
        .is_some_and(|value| value.contains("wrix/git-ssh"))
    {
        return Err(invalid_wrix_policy(
            workspace,
            format!("core.sshCommand does not invoke wrix/git-ssh: {ssh_command:?}"),
        ));
    }
    for relative in [".git/wrix/allowed_signers", ".git/wrix/git-ssh"] {
        if !workspace.join(relative).is_file() {
            return Err(invalid_wrix_policy(
                workspace,
                format!("required policy file is missing: {relative}"),
            ));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = std::fs::metadata(workspace.join(".git/wrix/git-ssh"))?
            .permissions()
            .mode();
        if mode & 0o111 == 0 {
            return Err(invalid_wrix_policy(
                workspace,
                ".git/wrix/git-ssh is not executable".to_string(),
            ));
        }
    }
    Ok(())
}

fn local_git_config(workspace: &Path, key: &str) -> Result<Option<String>, GitError> {
    let output = crate::git::environment::std_git_command()
        .arg("-C")
        .arg(workspace)
        .args(["config", "--local", "--get", key])
        .output()
        .map_err(GitError::Spawn)?;
    if output.status.success() {
        Ok(Some(String::from_utf8(output.stdout)?.trim().to_string()))
    } else {
        Ok(None)
    }
}

fn invalid_wrix_policy(workspace: &Path, detail: String) -> GitError {
    GitError::WrixPolicyInvalid {
        workdir: workspace.to_path_buf(),
        detail,
    }
}

/// Resolve the wrix signing key for loom-materialized workspaces, using
/// the same two-tier precedence wrix applies host-side:
///
/// 1. `$WRIX_SIGNING_KEY` pointing at an existing file. Set-but-missing
///    is a hard error ([`GitError::SigningKeyMissing`]) — a silent fallback
///    would mask a parent-process misconfiguration.
/// 2. `$HOME/.ssh/deploy_keys/<repo>-<host>-signing` when the env var is
///    unset and the file exists. `<repo>` is the repo segment of
///    `origin_dir`'s origin URL (parsed as `github.com[:/]<user>/<repo>`);
///    `<host>` is `hostname -s` (short form, falling back to `hostname`).
///    A non-GitHub origin URL skips the fallback.
/// 3. Otherwise `Ok(None)`. Default loop startup converts this into a hard
///    error; only explicit host-key mode permits ambient Git policy.
///
/// `origin_dir` is normally `.loom/integration`, whose `origin` points at the
/// repository remote (a bead clone's origin is only a local workspace path).
pub fn resolve_signing_key(origin_dir: &Path) -> Result<Option<PathBuf>, GitError> {
    resolve_from(
        &ResolveInputs::for_kind(KeyKind::Signing, origin_dir)?,
        KeyKind::Signing,
    )
}

/// Resolve the wrix deploy key for the launcher environment, mirroring
/// [`resolve_signing_key`] with the `-signing` suffix dropped: the keyname
/// fallback is `<repo>-<host>` and the env var is `$WRIX_DEPLOY_KEY`.
/// Set-but-missing is a hard error ([`GitError::DeployKeyMissing`]). Loom
/// passes the resolved host path only to Wrix child processes; Loom never
/// reads the private material (`specs/harness.md` § Repository Git isolation).
pub fn resolve_deploy_key(origin_dir: &Path) -> Result<Option<PathBuf>, GitError> {
    resolve_from(
        &ResolveInputs::for_kind(KeyKind::Deploy, origin_dir)?,
        KeyKind::Deploy,
    )
}

/// Which wrix key a resolution targets — selects the env var, the keyname
/// suffix, and the set-but-missing error variant.
#[derive(Clone, Copy)]
enum KeyKind {
    Deploy,
    Signing,
}

impl KeyKind {
    fn env_var(self) -> &'static str {
        match self {
            KeyKind::Deploy => WRIX_DEPLOY_KEY_ENV,
            KeyKind::Signing => WRIX_SIGNING_KEY_ENV,
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

/// Configure a legacy host-only signing fixture for integration tests.
#[cfg(any(test, feature = "test-support"))]
pub fn reconcile_signing_config(
    target_dir: &Path,
    signing_key: Option<&Path>,
) -> Result<(), GitError> {
    match signing_key {
        Some(key) => write_signing_config(target_dir, key),
        None => clear_signing_config(target_dir),
    }
}

/// Write a host-only signing fixture for integration tests.
#[cfg(any(test, feature = "test-support"))]
pub fn write_signing_config(target_dir: &Path, signing_key: &Path) -> Result<(), GitError> {
    let allowed_signers_file = target_dir.join(".git").join(ALLOWED_SIGNERS_FILE);
    // `.ok()` discards VarError (unset or non-UTF-8): either case means the
    // wrix author identity isn't available here, so fall back to the
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

fn clear_signing_config(target_dir: &Path) -> Result<(), GitError> {
    for key in [
        "gpg.format",
        "gpg.ssh.program",
        "user.signingkey",
        "gpg.ssh.allowedSignersFile",
        "commit.gpgsign",
    ] {
        unset_git_config(target_dir, key)?;
    }
    remove_legacy_allowed_signers(target_dir)
}

fn clear_managed_git_policy(target_dir: &Path) -> Result<(), GitError> {
    clear_signing_config(target_dir)?;
    unset_git_config(target_dir, "core.sshCommand")?;
    let wrix_dir = target_dir.join(".git/wrix");
    match std::fs::remove_dir_all(wrix_dir) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(GitError::Io(err)),
    }
}

fn remove_legacy_allowed_signers(target_dir: &Path) -> Result<(), GitError> {
    let allowed_signers_file = target_dir.join(".git").join(ALLOWED_SIGNERS_FILE);
    match std::fs::remove_file(allowed_signers_file) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(GitError::Io(err)),
    }
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

/// `ssh-keygen -y -f <signing_key>` — test-fixture public-key derivation.
#[cfg(any(test, feature = "test-support"))]
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
    let output = crate::git::environment::std_git_command()
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

fn unset_git_config(target_dir: &Path, key: &str) -> Result<(), GitError> {
    let output = crate::git::environment::std_git_command()
        .arg("-C")
        .arg(target_dir)
        .args(["config", "--unset-all", key])
        .output()
        .map_err(GitError::Spawn)?;
    if output.status.success() || output.status.code() == Some(GIT_CONFIG_KEY_NOT_FOUND_STATUS) {
        return Ok(());
    }
    Err(GitError::GitCli {
        status: output.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn repository_mode_rejects_ambient_git_transport_override() {
        let error = reject_ambient_transport_overrides([
            ("GIT_SSH_COMMAND", Some(OsString::from("ssh -i host-key"))),
            ("GIT_SSH", None),
        ])
        .expect_err("ambient transport must not shadow repository policy");
        assert!(matches!(
            error,
            GitError::AmbientGitTransportOverride { variable }
                if variable == "GIT_SSH_COMMAND"
        ));
    }

    #[test]
    fn parse_github_repo_handles_ssh_https_and_dot_git() {
        assert_eq!(
            parse_github_repo("git@github.com:wrix/loom.git").as_deref(),
            Some("loom"),
        );
        assert_eq!(
            parse_github_repo("https://github.com/wrix/loom").as_deref(),
            Some("loom"),
        );
        assert_eq!(
            parse_github_repo("https://github.com/wrix/loom.git").as_deref(),
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
        assert_eq!(parse_github_repo("git@gitlab.com:wrix/loom.git"), None);
        assert_eq!(parse_github_repo("https://example.com/wrix/loom"), None);
        assert_eq!(parse_github_repo("github.com-no-separator/x/y"), None);
    }

    /// Repository Git isolation derives the fallback keyname as
    /// `<repo>-<host>-signing` where `<repo>` comes from the origin URL and
    /// `<host>` is the short hostname.
    #[test]
    fn signing_key_fallback_uses_wrix_repo_host_derivation() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let deploy = home.join(".ssh/deploy_keys");
        std::fs::create_dir_all(&deploy).unwrap();
        let key = deploy.join("loom-buildbox-signing");
        std::fs::write(&key, "PRIVATE KEY\n").unwrap();

        let inputs = ResolveInputs {
            env_key: None,
            home: Some(home.to_path_buf()),
            origin_url: Some("git@github.com:wrix/loom.git".to_string()),
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
            origin_url: Some("git@github.com:wrix/loom.git".to_string()),
            hostname: Some("buildbox".to_string()),
        };
        assert_eq!(resolve_from(&inputs, KeyKind::Signing).unwrap(), None);
    }

    /// Spec contract `[test]` annotation: `$WRIX_SIGNING_KEY` pointing at
    /// a non-existent file is a hard error naming the missing path.
    #[test]
    fn wrix_signing_key_missing_file_fails_loud() {
        let inputs = ResolveInputs {
            env_key: Some(OsString::from("/nonexistent/wrix-signing-key")),
            home: None,
            origin_url: None,
            hostname: None,
        };
        match resolve_from(&inputs, KeyKind::Signing) {
            Err(GitError::SigningKeyMissing { path }) => {
                assert_eq!(path, PathBuf::from("/nonexistent/wrix-signing-key"));
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
        let status = crate::git::environment::std_git_command()
            .arg("-C")
            .arg(dir)
            .args(["init", "-q"])
            .status()
            .unwrap();
        assert!(status.success(), "git init must succeed");
    }

    fn git_config_get(dir: &Path, key: &str) -> Option<String> {
        let out = crate::git::environment::std_git_command()
            .arg("-C")
            .arg(dir)
            .args(["config", "--local", "--get", key])
            .output()
            .unwrap();
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// The host-only signing fixture writes a complete local SSH block.
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

    #[test]
    fn repository_policy_rejects_success_without_wrix_config() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("repo");
        std::fs::create_dir(&target).unwrap();
        init_git_repo(&target);
        let deploy = tmp.path().join("repo-key");
        let signing = tmp.path().join("repo-key-signing");
        std::fs::write(&deploy, "deploy").unwrap();
        std::fs::write(&signing, "signing").unwrap();
        let wrix = tmp.path().join("wrix");
        std::fs::write(&wrix, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&wrix, std::fs::Permissions::from_mode(0o755)).unwrap();
        let policy = RepoGitPolicy::for_test(wrix, "repo-key".into(), deploy, signing);

        let error = policy
            .apply(&target)
            .expect_err("a successful no-op must fail closed");
        assert!(matches!(error, GitError::WrixPolicyInvalid { .. }));
    }

    #[test]
    fn host_key_policy_clears_managed_repo_config() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path();
        init_git_repo(target);
        for (key, value) in [
            ("gpg.format", "ssh"),
            ("gpg.ssh.program", "wrix-git-sign"),
            ("gpg.ssh.allowedSignersFile", "wrix/allowed_signers"),
            ("user.signingkey", "wrix/signing-key/repo-signing"),
            ("commit.gpgsign", "true"),
            ("core.sshCommand", "wrix/git-ssh"),
        ] {
            sync_git_config(target, key, value).unwrap();
        }
        std::fs::create_dir_all(target.join(".git/wrix")).unwrap();
        std::fs::write(target.join(".git/wrix/allowed_signers"), "key\n").unwrap();
        let policy =
            RepoGitPolicy::resolve(target, PathBuf::from("unused-wrix"), KeyMode::Host).unwrap();

        policy.apply(target).unwrap();

        for key in [
            "gpg.format",
            "gpg.ssh.program",
            "gpg.ssh.allowedSignersFile",
            "user.signingkey",
            "commit.gpgsign",
            "core.sshCommand",
        ] {
            assert_eq!(git_config_get(target, key), None, "stale {key}");
        }
        assert!(!target.join(".git/wrix").exists());
    }

    /// Host-only test fixtures derive the expected allowed-signers identity.
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
        // envs — accept either the wrix default or the configured email).
        let expected_identity = std::env::var("GIT_AUTHOR_EMAIL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_SIGNING_IDENTITY.to_string());
        assert_eq!(identity, expected_identity);
        assert_eq!(key_field, pubkey);
    }

    #[test]
    fn wrix_signing_key_present_takes_precedence() {
        let tmp = tempfile::tempdir().unwrap();
        let key = tmp.path().join("explicit-signing-key");
        std::fs::write(&key, "PRIVATE KEY\n").unwrap();
        let inputs = ResolveInputs {
            // A GitHub origin + present fallback must be ignored when the
            // env var resolves.
            env_key: Some(key.clone().into_os_string()),
            home: Some(tmp.path().to_path_buf()),
            origin_url: Some("git@github.com:wrix/loom.git".to_string()),
            hostname: Some("buildbox".to_string()),
        };
        assert_eq!(
            resolve_from(&inputs, KeyKind::Signing).unwrap().as_deref(),
            Some(key.as_path())
        );
    }

    /// The deploy-key fallback drops the `-signing` suffix: keyname is
    /// `<repo>-<host>`, mirroring [`signing_key_fallback_uses_wrix_repo_host_derivation`].
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
            origin_url: Some("git@github.com:wrix/loom.git".to_string()),
            hostname: Some("buildbox".to_string()),
        };
        let resolved = resolve_from(&inputs, KeyKind::Deploy).unwrap();
        assert_eq!(resolved.as_deref(), Some(key.as_path()));
    }

    /// `$WRIX_DEPLOY_KEY` pointing at a non-existent file is a hard error
    /// naming the missing path — the deploy-key analogue of
    /// [`wrix_signing_key_missing_file_fails_loud`].
    #[test]
    fn wrix_deploy_key_missing_file_fails_loud() {
        let inputs = ResolveInputs {
            env_key: Some(OsString::from("/nonexistent/wrix-deploy-key")),
            home: None,
            origin_url: None,
            hostname: None,
        };
        match resolve_from(&inputs, KeyKind::Deploy) {
            Err(GitError::DeployKeyMissing { path }) => {
                assert_eq!(path, PathBuf::from("/nonexistent/wrix-deploy-key"));
            }
            other => panic!("expected DeployKeyMissing, got {other:?}"),
        }
    }
}

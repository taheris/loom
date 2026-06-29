//! `loom gate verify-marker` consumer-side implementation + the mint
//! authority that produces the marker on audit-pass.
//!
//! `MarkerProof` is the content-addressed trust artifact the driver-side
//! verdict gate mints on audit-pass. prek's pre-push hook chain consumes
//! it through `loom gate verify-marker` to short-circuit redundant work
//! on driver-loop integration pushes. See `specs/gate.md` § Marker.
//!
//! The mint path ([`MarkerProof::from_gate_success`] +
//! [`MarkerProof::write_to`], both `pub(crate)`) is wrapped by the public
//! [`MarkerProof::mint`] helper that the push-gate calls inside its
//! `index.lock` critical section. Acceptance of a sealed [`GateSuccess`]
//! is the structural defense against forgery: only `loom-gate` can mint
//! a marker, and only valid evidence + the four-condition AND can
//! produce a `GateSuccess`.
//!
//! The verify path ([`MarkerProof::read_and_validate`]) deserialises the
//! marker, asserts the workspace porcelain is clean, and matches the
//! marker's tree OID against `HEAD`'s tree OID. A returned `Ok` value
//! corresponds to "the gate ran AND the workspace still matches at the
//! moment this value was constructed" by construction.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use displaydoc::Display;
use loom_driver::clock::Clock;
use loom_driver::git::{self, GitError as DriverGitError, GitOid};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::gate_outcome::{GateSuccess, HookCoverage};

/// Marker schema version this binary understands. Higher versions are
/// rejected with `MarkerError::UnsupportedSchema`.
const CURRENT_VERSION: u32 = 2;

/// Canonical marker location relative to the workspace root, per
/// `specs/gate.md` § Marker — *File location and lifecycle*.
pub const MARKER_PATH: &str = ".loom/marker.json";

/// Sealed content-addressed receipt that the gate ran cleanly at a
/// specific workspace tree.
///
/// Validated construction routes through [`MarkerProof::read_and_validate`];
/// the deserialiser cannot yield a `MarkerProof` for a stale or
/// mismatched state — it returns `Err`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkerProof {
    version: u32,
    commit_sha: GitOid,
    tree_oid: GitOid,
    pre_commit_config_digest: String,
    push_range: String,
    covered_hooks: Vec<HookCoverage>,
    verified_scope_fingerprint: String,
    reviewed_scope_fingerprint: String,
    minted_at_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerValidationRequest {
    pub hook_id: String,
    pub hook_entry: String,
    pub push_range: String,
}

impl MarkerProof {
    /// Mint a sealed marker for the integrated tree at `workspace`.
    ///
    /// `pub(crate)`: callers outside `loom-gate` route through
    /// [`MarkerProof::mint`], which pairs construction with the atomic
    /// write. Acceptance of `GateSuccess` (itself sealed by the
    /// `_private: ()` field on [`GateSuccess`]) is the load-bearing
    /// forgery defense per `specs/gate.md` § *Forgery resistance*.
    pub(crate) fn from_gate_success(
        success: GateSuccess,
        workspace: &Path,
        clock: &dyn Clock,
    ) -> Result<Self, MintError> {
        let commit_sha = git::sync_head_commit_sha(workspace)?;
        if !git::status_porcelain_sync(workspace)?.is_empty() {
            return Err(MintError::PorcelainDirty);
        }
        let tree_oid = git_tree_oid_of_head(workspace)?;
        if tree_oid.to_string() != success.tree_oid {
            return Err(MintError::TreeMismatch {
                marker_tree: success.tree_oid,
                head_tree: tree_oid,
            });
        }
        let pre_commit_config_digest = pre_commit_config_digest(workspace)
            .map_err(|source| MintError::ConfigDigestRead { source })?;
        if pre_commit_config_digest != success.pre_push.config_digest {
            return Err(MintError::ConfigDigestMismatch {
                marker_digest: success.pre_push.config_digest,
                current_digest: pre_commit_config_digest,
            });
        }
        let minted_at_ms = clock
            .wall_now()
            .duration_since(UNIX_EPOCH)
            .map_err(|source| MintError::Clock { source })?
            .as_millis();
        Ok(MarkerProof {
            version: CURRENT_VERSION,
            commit_sha,
            tree_oid,
            pre_commit_config_digest,
            push_range: success.push_range,
            covered_hooks: success.pre_push.hooks,
            verified_scope_fingerprint: success.pre_push.verified_scope_fingerprint,
            reviewed_scope_fingerprint: success.pre_push.reviewed_scope_fingerprint,
            minted_at_ms,
        })
    }

    /// Atomic write to `path` via `<path>.tmp` + rename, per
    /// `specs/gate.md` § *File location and lifecycle*.
    ///
    /// `pub(crate)`: callers outside `loom-gate` route through
    /// [`MarkerProof::mint`].
    pub(crate) fn write_to(&self, path: &Path) -> Result<(), io::Error> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let tmp = tmp_sibling(path);
        fs::write(&tmp, &bytes)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Public mint wrapper the driver-side push gate calls inside its
    /// `index.lock` critical section.
    ///
    /// Sequences [`MarkerProof::from_gate_success`] +
    /// [`MarkerProof::write_to`] so the `pub(crate)` mint primitives stay
    /// crate-private while the push-gate (in `loom-workflow`) has a
    /// single typed entry point. The mint sequence per `specs/gate.md`
    /// § *Mint trigger* (audit-pass → construct → write → push) routes
    /// audit + push outside this function; we own only the construct +
    /// write atom.
    pub fn mint(
        success: GateSuccess,
        workspace: &Path,
        clock: &dyn Clock,
    ) -> Result<Self, MintError> {
        let marker = MarkerProof::from_gate_success(success, workspace, clock)?;
        let path = workspace.join(MARKER_PATH);
        marker.write_to(&path).map_err(|source| MintError::Write {
            path: path.clone(),
            source,
        })?;
        Ok(marker)
    }

    /// Read the marker at `path` and validate it against the workspace's
    /// current tree fingerprint.
    ///
    /// Returns `Ok` iff the marker's tree OID matches `HEAD`'s tree OID,
    /// porcelain is clean, and the schema version is supported.
    pub fn read_and_validate(path: &Path, workspace: &Path) -> Result<Self, MarkerError> {
        let bytes = fs::read(path).map_err(|source| match source.kind() {
            io::ErrorKind::NotFound => MarkerError::MissingMarker {
                path: path.to_path_buf(),
            },
            _ => MarkerError::ReadMarker {
                path: path.to_path_buf(),
                source,
            },
        })?;
        let marker: MarkerProof =
            serde_json::from_slice(&bytes).map_err(|source| MarkerError::ParseMarker {
                path: path.to_path_buf(),
                source,
            })?;
        if marker.version > CURRENT_VERSION {
            return Err(MarkerError::UnsupportedSchema {
                found: marker.version,
                current: CURRENT_VERSION,
            });
        }
        assert_porcelain_clean(workspace)?;
        let current_tree = git_tree_oid_of_head(workspace)?;
        if current_tree != marker.tree_oid {
            return Err(MarkerError::FingerprintMismatch {
                marker_tree: marker.tree_oid.clone(),
                head_tree: current_tree,
            });
        }
        let current_digest = pre_commit_config_digest(workspace)
            .map_err(|source| MarkerError::ConfigDigestRead { source })?;
        if current_digest != marker.pre_commit_config_digest {
            return Err(MarkerError::ConfigDigestMismatch {
                marker_digest: marker.pre_commit_config_digest.clone(),
                current_digest,
            });
        }
        if marker.covered_hooks.is_empty()
            || marker.verified_scope_fingerprint.is_empty()
            || marker.reviewed_scope_fingerprint.is_empty()
        {
            return Err(MarkerError::ScopeEvidenceMissing);
        }
        Ok(marker)
    }

    pub fn read_and_validate_for_hook(
        path: &Path,
        workspace: &Path,
        request: &MarkerValidationRequest,
    ) -> Result<Self, MarkerError> {
        let marker = Self::read_and_validate(path, workspace)?;
        if marker.push_range != request.push_range {
            return Err(MarkerError::CoverageMismatch {
                reason: "push range".to_owned(),
            });
        }
        if !marker
            .covered_hooks
            .iter()
            .any(|hook| hook.id == request.hook_id && hook.entry == request.hook_entry)
        {
            return Err(MarkerError::CoverageMismatch {
                reason: "hook id/entry".to_owned(),
            });
        }
        Ok(marker)
    }

    /// Schema version this marker was minted under.
    pub fn version(&self) -> u32 {
        self.version
    }

    /// `HEAD` commit SHA recorded at mint time — informational only;
    /// the trust-bearing field is `tree_oid`.
    pub fn commit_sha(&self) -> &GitOid {
        &self.commit_sha
    }

    /// Tree OID the marker binds to — the load-bearing fingerprint.
    pub fn tree_oid(&self) -> &GitOid {
        &self.tree_oid
    }

    pub fn pre_commit_config_digest(&self) -> &str {
        &self.pre_commit_config_digest
    }

    pub fn push_range(&self) -> &str {
        &self.push_range
    }

    pub fn covered_hooks(&self) -> &[HookCoverage] {
        &self.covered_hooks
    }
}

/// Validate the marker file at `<workspace>/.loom/marker.json` against
/// the workspace's current `HEAD` tree. The `loom gate verify-marker`
/// CLI subcommand maps `Ok` to exit code 0 and `Err` to non-zero.
pub fn verify_marker(workspace: &Path) -> Result<MarkerProof, MarkerError> {
    let path = workspace.join(MARKER_PATH);
    MarkerProof::read_and_validate(&path, workspace)
}

pub fn verify_marker_for_hook(
    workspace: &Path,
    request: &MarkerValidationRequest,
) -> Result<MarkerProof, MarkerError> {
    let path = workspace.join(MARKER_PATH);
    MarkerProof::read_and_validate_for_hook(&path, workspace, request)
}

/// Failure modes of [`MarkerProof::mint`] / [`MarkerProof::from_gate_success`].
///
/// Each variant points at the smallest unit of work that failed so the
/// push-gate log can name the specific failure without scanning the
/// inner error string.
#[derive(Debug, Display, Error)]
pub enum MintError {
    /// failed to read workspace git state
    Git(#[from] DriverGitError),
    /// system clock returned a time before the unix epoch: {source}
    Clock {
        #[source]
        source: std::time::SystemTimeError,
    },
    /// workspace porcelain is not clean at marker mint time
    PorcelainDirty,
    /// gate success tree `{marker_tree}` does not match workspace tree `{head_tree}`
    TreeMismatch {
        marker_tree: String,
        head_tree: GitOid,
    },
    /// failed to read `.pre-commit-config.yaml` for marker minting: {source}
    ConfigDigestRead {
        #[source]
        source: io::Error,
    },
    /// gate success config digest `{marker_digest}` does not match workspace digest `{current_digest}`
    ConfigDigestMismatch {
        marker_digest: String,
        current_digest: String,
    },
    /// failed to atomically write marker to `{path}`: {source}
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

fn tmp_sibling(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(std::ffi::OsString::from)
        .unwrap_or_default();
    name.push(".tmp");
    match path.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

/// Failure modes of [`MarkerProof::read_and_validate`].
#[derive(Debug, Display, Error)]
pub enum MarkerError {
    /// marker file not present at `{path}`
    MissingMarker { path: PathBuf },
    /// failed to read marker at `{path}`: {source}
    ReadMarker {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// marker at `{path}` is malformed JSON: {source}
    ParseMarker {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// marker schema version {found} is newer than the supported version {current}
    UnsupportedSchema { found: u32, current: u32 },
    /// workspace porcelain is not clean — uncommitted changes invalidate the marker
    PorcelainDirty,
    /// workspace tree OID `{head_tree}` does not match marker's `{marker_tree}`
    FingerprintMismatch {
        marker_tree: GitOid,
        head_tree: GitOid,
    },
    /// failed to read `.pre-commit-config.yaml` for marker validation: {source}
    ConfigDigestRead {
        #[source]
        source: io::Error,
    },
    /// `.pre-commit-config.yaml` digest `{current_digest}` does not match marker's `{marker_digest}`
    ConfigDigestMismatch {
        marker_digest: String,
        current_digest: String,
    },
    /// marker is missing verify/review scope evidence
    ScopeEvidenceMissing,
    /// marker does not cover the current hook invocation: {reason}
    CoverageMismatch { reason: String },
    /// failed to read workspace git state
    Git(#[from] DriverGitError),
}

fn assert_porcelain_clean(workspace: &Path) -> Result<(), MarkerError> {
    let porcelain = git::status_porcelain_sync(workspace)?;
    if !porcelain.is_empty() {
        return Err(MarkerError::PorcelainDirty);
    }
    Ok(())
}

fn git_tree_oid_of_head(workspace: &Path) -> Result<GitOid, DriverGitError> {
    git::head_tree_oid_sync(workspace)
}

fn pre_commit_config_digest(workspace: &Path) -> Result<String, io::Error> {
    let bytes = fs::read(workspace.join(".pre-commit-config.yaml"))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command as Cmd;
    use tempfile::TempDir;

    const HOOK_ID: &str = "loom-gate-verify-diff";
    const HOOK_ENTRY: &str = "loom gate verify --diff @{u}..HEAD";
    const PUSH_RANGE: &str = "origin/main..HEAD";

    static LOOM_CLI_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("workspace root")
            .to_path_buf()
    }

    fn run_verify_marker_cli(workspace: &Path) -> std::process::Output {
        let _guard = LOOM_CLI_LOCK.lock().expect("loom cli lock");
        match std::env::var_os("CARGO_BIN_EXE_loom") {
            Some(bin) => Cmd::new(bin)
                .args(["gate", "verify-marker"])
                .current_dir(workspace)
                .output()
                .expect("spawn loom gate verify-marker"),
            None => Cmd::new("cargo")
                .args(["run", "--quiet", "--manifest-path"])
                .arg(repo_root().join("Cargo.toml"))
                .args(["-p", "loom", "--bin", "loom", "--", "gate", "verify-marker"])
                .current_dir(workspace)
                .output()
                .expect("spawn cargo run -p loom gate verify-marker"),
        }
    }

    fn assert_cli_failed_with(output: &std::process::Output, expected: &str) {
        assert!(
            !output.status.success(),
            "verify-marker CLI must fail. stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(expected),
            "verify-marker CLI stderr must contain {expected:?}. stderr={stderr:?}",
        );
    }

    fn init_test_workspace() -> TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        loom_driver::git::init_test_repo(dir.path()).expect("init test repo");
        std::fs::write(
            dir.path().join(".pre-commit-config.yaml"),
            "repos: []
",
        )
        .expect("write pre-commit config");
        loom_driver::git::commit_all_in(dir.path(), "add pre-commit config")
            .expect("commit config");
        dir
    }

    fn head_tree(dir: &Path) -> GitOid {
        git_tree_oid_of_head(dir).expect("tree oid")
    }

    fn head_sha(_dir: &Path) -> GitOid {
        GitOid::new("0000000000000000000000000000000000000000").expect("placeholder sha")
    }

    fn write_marker_at(workspace: &Path, marker: &MarkerProof) {
        let path = workspace.join(MARKER_PATH);
        fs::create_dir_all(path.parent().expect("marker parent")).expect("mkdir .loom");
        let bytes = serde_json::to_vec_pretty(marker).expect("serialise marker");
        fs::write(&path, bytes).expect("write marker");
    }

    fn hook() -> HookCoverage {
        HookCoverage {
            id: HOOK_ID.to_owned(),
            entry: HOOK_ENTRY.to_owned(),
        }
    }

    fn request() -> MarkerValidationRequest {
        MarkerValidationRequest {
            hook_id: HOOK_ID.to_owned(),
            hook_entry: HOOK_ENTRY.to_owned(),
            push_range: PUSH_RANGE.to_owned(),
        }
    }

    fn marker_for_workspace(workspace: &Path) -> MarkerProof {
        MarkerProof {
            version: CURRENT_VERSION,
            commit_sha: head_sha(workspace),
            tree_oid: head_tree(workspace),
            pre_commit_config_digest: pre_commit_config_digest(workspace).expect("config digest"),
            push_range: PUSH_RANGE.to_owned(),
            covered_hooks: vec![hook()],
            verified_scope_fingerprint: "verified-scope".to_owned(),
            reviewed_scope_fingerprint: "reviewed-scope".to_owned(),
            minted_at_ms: 0,
        }
    }

    #[test]
    fn verify_marker_exits_zero_on_match() {
        let dir = init_test_workspace();
        let workspace = dir.path();
        let marker = marker_for_workspace(workspace);
        write_marker_at(workspace, &marker);

        let output = run_verify_marker_cli(workspace);

        assert!(
            output.status.success(),
            "verify-marker CLI must succeed for matching marker. stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    #[test]
    fn verify_marker_exits_nonzero_on_missing() {
        let dir = init_test_workspace();
        let workspace = dir.path();

        let output = run_verify_marker_cli(workspace);

        assert_cli_failed_with(&output, "marker file not present");
    }

    #[test]
    fn verify_marker_exits_nonzero_on_tree_mismatch() {
        let dir = init_test_workspace();
        let workspace = dir.path();
        let mut marker = marker_for_workspace(workspace);
        marker.tree_oid =
            GitOid::new("deadbeefcafe1234567890abcdef0123456789ab").expect("tree oid");
        write_marker_at(workspace, &marker);

        let output = run_verify_marker_cli(workspace);

        assert_cli_failed_with(&output, "does not match marker");
    }

    #[test]
    fn verify_marker_exits_nonzero_on_dirty_tree() {
        let dir = init_test_workspace();
        let workspace = dir.path();
        let marker = marker_for_workspace(workspace);
        write_marker_at(workspace, &marker);
        std::fs::write(workspace.join("README.md"), "dirty contents\n").expect("dirty edit");

        let output = run_verify_marker_cli(workspace);

        assert_cli_failed_with(&output, "workspace porcelain is not clean");
    }

    #[test]
    fn verify_marker_rejects_same_tree_wrong_config_digest() {
        let dir = init_test_workspace();
        let workspace = dir.path();
        let mut marker = marker_for_workspace(workspace);
        marker.pre_commit_config_digest = "wrong-config".to_owned();
        write_marker_at(workspace, &marker);
        match verify_marker_for_hook(workspace, &request()) {
            Err(MarkerError::ConfigDigestMismatch { .. }) => {}
            other => panic!("expected ConfigDigestMismatch, got {other:?}"),
        }
    }

    #[test]
    fn verify_marker_rejects_wrong_push_range() {
        let dir = init_test_workspace();
        let workspace = dir.path();
        write_marker_at(workspace, &marker_for_workspace(workspace));
        let mut req = request();
        req.push_range = "origin/other..HEAD".to_owned();
        match verify_marker_for_hook(workspace, &req) {
            Err(MarkerError::CoverageMismatch { reason }) if reason == "push range" => {}
            other => panic!("expected push range mismatch, got {other:?}"),
        }
    }

    #[test]
    fn verify_marker_rejects_wrong_hook_id_or_entry() {
        let dir = init_test_workspace();
        let workspace = dir.path();
        write_marker_at(workspace, &marker_for_workspace(workspace));
        let mut wrong_id = request();
        wrong_id.hook_id = "cargo-clippy".to_owned();
        match verify_marker_for_hook(workspace, &wrong_id) {
            Err(MarkerError::CoverageMismatch { reason }) if reason == "hook id/entry" => {}
            other => panic!("expected hook id mismatch, got {other:?}"),
        }
        let mut wrong_entry = request();
        wrong_entry.hook_entry = "loom gate verify --diff origin/main..HEAD".to_owned();
        match verify_marker_for_hook(workspace, &wrong_entry) {
            Err(MarkerError::CoverageMismatch { reason }) if reason == "hook id/entry" => {}
            other => panic!("expected hook entry mismatch, got {other:?}"),
        }
    }

    #[test]
    fn verify_marker_rejects_missing_review_evidence() {
        let dir = init_test_workspace();
        let workspace = dir.path();
        let mut marker = marker_for_workspace(workspace);
        marker.reviewed_scope_fingerprint.clear();
        write_marker_at(workspace, &marker);
        match verify_marker_for_hook(workspace, &request()) {
            Err(MarkerError::ScopeEvidenceMissing) => {}
            other => panic!("expected ScopeEvidenceMissing, got {other:?}"),
        }
    }

    #[test]
    fn verify_marker_accepts_valid_covered_hook() {
        let dir = init_test_workspace();
        let workspace = dir.path();
        write_marker_at(workspace, &marker_for_workspace(workspace));
        let validated = verify_marker_for_hook(workspace, &request()).expect("covered hook");
        assert_eq!(validated.push_range(), PUSH_RANGE);
    }

    fn install_executable(bin_dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(bin_dir).expect("mkdir bin");
        let path = bin_dir.join(name);
        std::fs::write(&path, body).expect("write script");
        let mut perm = std::fs::metadata(&path).expect("stat").permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&path, perm).expect("chmod");
    }

    fn install_repo_pre_push_checks(workspace: &Path) {
        let source = repo_root().join("bin/pre-push-checks");
        let dest = workspace.join("bin/pre-push-checks");
        std::fs::create_dir_all(dest.parent().expect("wrapper parent")).expect("mkdir bin");
        std::fs::copy(&source, &dest).expect("copy repo-local pre-push-checks");
        let mut perm = std::fs::metadata(&dest)
            .expect("stat wrapper")
            .permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&dest, perm).expect("chmod wrapper");
    }

    fn seed_marker(workspace: &Path) {
        let path = workspace.join(MARKER_PATH);
        std::fs::create_dir_all(path.parent().expect("marker parent")).expect("mkdir marker dir");
        std::fs::write(&path, "{}").expect("write marker");
    }

    fn run_pre_push_checks(workspace: &Path, bin_dir: &Path) -> std::process::Output {
        run_pre_push_checks_with_env(workspace, bin_dir, &[])
    }

    fn run_pre_push_checks_with_env(
        workspace: &Path,
        bin_dir: &Path,
        envs: &[(&str, String)],
    ) -> std::process::Output {
        let path_var = std::env::var_os("PATH").unwrap_or_default();
        let mut entries = vec![bin_dir.to_path_buf()];
        entries.extend(std::env::split_paths(&path_var));
        let new_path = std::env::join_paths(entries).expect("join PATH");
        install_repo_pre_push_checks(workspace);
        let mut command = Cmd::new("bin/pre-push-checks");
        command
            .args([
                "--hook-id",
                HOOK_ID,
                "--hook-entry",
                HOOK_ENTRY,
                "--push-range",
                PUSH_RANGE,
                "--",
                "sentinel",
            ])
            .current_dir(workspace)
            .env("PATH", new_path);
        for (name, value) in envs {
            command.env(*name, value);
        }
        command.output().expect("spawn pre-push-checks")
    }

    #[test]
    fn pre_push_checks_short_circuits_on_valid_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path();
        seed_marker(workspace);

        let bin_dir = workspace.join("bin");
        install_executable(&bin_dir, "loom", "#!/bin/sh\nexit 0\n");
        let sentinel_marker = workspace.join("sentinel.flag");
        install_executable(
            &bin_dir,
            "sentinel",
            &format!("#!/bin/sh\ntouch {}\nexit 0\n", sentinel_marker.display()),
        );

        let output = run_pre_push_checks(workspace, &bin_dir);
        assert!(
            output.status.success(),
            "wrapper must exit 0 on valid marker. stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(
            !sentinel_marker.exists(),
            "sentinel must NOT execute when marker validates"
        );
    }

    /// Operator-direct invariant: when no `.loom/marker.json` exists in
    /// the workspace (the normal state of an operator's clone, which the
    /// driver-side verdict gate never wrote into), the `pre-push-checks`
    /// wrapper MUST exec the argument command rather than fail the push.
    /// A regression here forces operators to `--no-verify` to land work.
    #[test]
    fn pre_push_checks_falls_through_on_missing_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path();
        assert!(
            !workspace.join(MARKER_PATH).exists(),
            "precondition: canonical marker path must be absent",
        );
        let bin_dir = workspace.join("bin");
        let sentinel_marker = workspace.join("sentinel.flag");
        install_executable(
            &bin_dir,
            "sentinel",
            &format!("#!/bin/sh\ntouch {}\nexit 0\n", sentinel_marker.display()),
        );

        let output = run_pre_push_checks(workspace, &bin_dir);
        assert!(
            output.status.success(),
            "wrapper must exit 0 when marker is missing (operator-direct fall-through). \
             stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(
            sentinel_marker.exists(),
            "missing marker must fall through: sentinel must execute (not short-circuit)",
        );
    }

    #[test]
    fn pre_push_checks_scrubs_git_hook_env_before_fallthrough() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path();
        let bin_dir = workspace.join("bin");
        let sentinel_marker = workspace.join("sentinel.flag");
        install_executable(
            &bin_dir,
            "sentinel",
            &format!(
                "#!/bin/sh\n\
                 if [ \"${{GIT_DIR+x}}\" = x ]; then exit 42; fi\n\
                 if [ \"${{GIT_WORK_TREE+x}}\" = x ]; then exit 43; fi\n\
                 if [ \"${{GIT_CONFIG_COUNT+x}}\" = x ]; then exit 44; fi\n\
                 touch {}\n\
                 exit 0\n",
                sentinel_marker.display(),
            ),
        );
        let envs = [
            ("GIT_DIR", workspace.join(".git").display().to_string()),
            ("GIT_WORK_TREE", workspace.display().to_string()),
            ("GIT_CONFIG_COUNT", "0".to_string()),
        ];

        let output = run_pre_push_checks_with_env(workspace, &bin_dir, &envs);

        assert!(
            output.status.success(),
            "wrapper must scrub hook-local git env before fall-through. stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(
            sentinel_marker.exists(),
            "sentinel must execute after env scrub"
        );
    }

    #[test]
    fn pre_push_checks_falls_through_on_invalid_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path();
        seed_marker(workspace);

        let bin_dir = workspace.join("bin");
        install_executable(&bin_dir, "loom", "#!/bin/sh\nexit 1\n");
        let sentinel_marker = workspace.join("sentinel.flag");
        install_executable(
            &bin_dir,
            "sentinel",
            &format!("#!/bin/sh\ntouch {}\nexit 0\n", sentinel_marker.display()),
        );

        let output = run_pre_push_checks(workspace, &bin_dir);
        assert!(
            output.status.success(),
            "wrapper must propagate sentinel exit (0). stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(
            sentinel_marker.exists(),
            "sentinel must execute when marker absent or invalid"
        );
    }

    /// Joint-path closure: [`MarkerProof::mint`] writes to a path the
    /// `pre-push-checks` wrapper actually reads. Hand-seeded fixtures
    /// in the two tests above pin wrapper behaviour in isolation —
    /// they don't pin that mint's output is what the wrapper observes.
    /// Drives the end-to-end chain (mint → wrapper short-circuit) the
    /// `specs/pre-commit.md` § *Marker integration* contract commits
    /// to, against a real git workspace and the real mint code path.
    #[test]
    fn mint_and_pre_push_checks_short_circuit_joint_path() {
        let dir = init_test_workspace();
        let workspace = dir.path();
        let (_log, success) = good_gate_success(workspace);
        let clock = loom_driver::clock::SystemClock::new();
        MarkerProof::mint(success, workspace, &clock).expect("mint succeeds");
        assert!(
            workspace.join(MARKER_PATH).exists(),
            "mint must write to the canonical {MARKER_PATH}",
        );

        let bin_dir = workspace.join("bin");
        install_executable(&bin_dir, "loom", "#!/bin/sh\nexit 0\n");
        let sentinel_marker = workspace.join("sentinel.flag");
        install_executable(
            &bin_dir,
            "sentinel",
            &format!("#!/bin/sh\ntouch {}\nexit 0\n", sentinel_marker.display()),
        );

        let output = run_pre_push_checks(workspace, &bin_dir);
        assert!(
            output.status.success(),
            "wrapper must exit 0 after MarkerProof::mint. stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(
            !sentinel_marker.exists(),
            "mint's write path must match the wrapper's read path: \
             sentinel must NOT execute",
        );
    }

    fn good_gate_success(workspace: &Path) -> (tempfile::NamedTempFile, GateSuccess) {
        let log = tempfile::NamedTempFile::new().expect("tempfile");
        let tree = head_tree(workspace).to_string();
        let config_digest = pre_commit_config_digest(workspace).expect("config digest");
        let verify = crate::gate_outcome::GateRun::successful_verify(
            PUSH_RANGE.to_string(),
            tree.clone(),
            config_digest.clone(),
            log.path().to_path_buf(),
            vec![hook()],
        );
        crate::gate_outcome::append_gate_run_lifecycle_events(log.path(), &verify)
            .expect("write verify gate events");
        let review = crate::gate_outcome::GateRun::successful_review(
            PUSH_RANGE.to_string(),
            tree,
            config_digest,
            log.path().to_path_buf(),
            loom_protocol::gate::ExitSignal::Complete,
        );
        crate::gate_outcome::append_gate_run_lifecycle_events(log.path(), &review)
            .expect("write review gate events");
        let runs = crate::gate_outcome::parse_gate_runs_from_jsonl(log.path());
        let evidence = crate::gate_outcome::HandoffEvidence::from_runs(runs);
        let success = GateSuccess::new(&evidence, 1).expect("good evidence mints success");
        (log, success)
    }

    /// Mint a marker against a real git workspace, then validate it
    /// round-trips through `read_and_validate`. This exercises the
    /// `from_gate_success → write_to → read_and_validate` chain that
    /// gate.md § *Mint trigger* / *Consumer contract* commits to.
    #[test]
    fn mint_round_trips_through_read_and_validate() {
        let dir = init_test_workspace();
        let workspace = dir.path();
        let (_log, success) = good_gate_success(workspace);
        let clock = loom_driver::clock::SystemClock::new();
        let minted = MarkerProof::mint(success, workspace, &clock).expect("mint succeeds");
        assert_eq!(minted.version, CURRENT_VERSION);
        assert_eq!(minted.tree_oid, head_tree(workspace));
        let validated = verify_marker(workspace).expect("read_and_validate round-trips");
        assert_eq!(validated, minted);
    }

    /// The mint path uses `<path>.tmp` + rename, so an external observer
    /// can never see a partially-written marker file. Crash the write at
    /// the rename boundary by leaving a pre-existing tmp file in place;
    /// the rename must overwrite it cleanly and the final marker bytes
    /// must match the JSON we serialised.
    #[test]
    fn mint_atomic_write_replaces_existing_tmp() {
        let dir = init_test_workspace();
        let workspace = dir.path();
        let loom_dir = workspace.join(".loom");
        std::fs::create_dir_all(&loom_dir).expect("mkdir .loom");
        std::fs::write(loom_dir.join("marker.json.tmp"), b"stale\n").expect("seed stale tmp");

        let (_log, success) = good_gate_success(workspace);
        let clock = loom_driver::clock::SystemClock::new();
        let minted = MarkerProof::mint(success, workspace, &clock).expect("mint succeeds");

        let final_bytes = std::fs::read(workspace.join(MARKER_PATH)).expect("marker file readable");
        let parsed: MarkerProof =
            serde_json::from_slice(&final_bytes).expect("marker file is valid JSON");
        assert_eq!(parsed, minted);
        assert!(
            !loom_dir.join("marker.json.tmp").exists(),
            "tmp sibling must be renamed away, not left behind",
        );
    }

    /// `mint` must refuse to fingerprint a workspace that is not a git
    /// repository — without git we cannot compute `HEAD`'s tree OID.
    #[test]
    fn mint_errors_when_workspace_has_no_git() {
        let evidence_dir = init_test_workspace();
        let dir = tempfile::tempdir().expect("tempdir");
        let (_log, success) = good_gate_success(evidence_dir.path());
        let clock = loom_driver::clock::SystemClock::new();
        let result = MarkerProof::mint(success, dir.path(), &clock);
        assert!(matches!(result, Err(MintError::Git(_))), "got {result:?}");
    }
}

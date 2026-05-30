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
use loom_driver::git::{self, GitError as DriverGitError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::gate_outcome::GateSuccess;

/// Marker schema version this binary understands. Higher versions are
/// rejected with `MarkerError::UnsupportedSchema`.
const CURRENT_VERSION: u32 = 1;

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
    commit_sha: String,
    tree_oid: String,
    minted_at_ms: u128,
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
        _success: GateSuccess,
        workspace: &Path,
        clock: &dyn Clock,
    ) -> Result<Self, MintError> {
        let commit_sha = git::sync_head_commit_sha(workspace)?;
        let tree_oid = git_tree_oid_of_head(workspace)?;
        let minted_at_ms = clock
            .wall_now()
            .duration_since(UNIX_EPOCH)
            .map_err(|source| MintError::Clock { source })?
            .as_millis();
        Ok(MarkerProof {
            version: CURRENT_VERSION,
            commit_sha,
            tree_oid,
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
        Ok(marker)
    }

    /// Schema version this marker was minted under.
    pub fn version(&self) -> u32 {
        self.version
    }

    /// `HEAD` commit SHA recorded at mint time — informational only;
    /// the trust-bearing field is `tree_oid`.
    pub fn commit_sha(&self) -> &str {
        &self.commit_sha
    }

    /// Tree OID the marker binds to — the load-bearing fingerprint.
    pub fn tree_oid(&self) -> &str {
        &self.tree_oid
    }
}

/// Validate the marker file at `<workspace>/.loom/marker.json` against
/// the workspace's current `HEAD` tree. The `loom gate verify-marker`
/// CLI subcommand maps `Ok` to exit code 0 and `Err` to non-zero.
pub fn verify_marker(workspace: &Path) -> Result<MarkerProof, MarkerError> {
    let path = workspace.join(MARKER_PATH);
    MarkerProof::read_and_validate(&path, workspace)
}

/// Failure modes of [`MarkerProof::mint`] / [`MarkerProof::from_gate_success`].
///
/// Each variant points at the smallest unit of work that failed so the
/// push-gate log can name the specific failure without scanning the
/// inner error string.
#[derive(Debug, Display, Error)]
pub enum MintError {
    /// failed to read workspace git state: {0}
    Git(#[from] DriverGitError),
    /// system clock returned a time before the unix epoch: {source}
    Clock {
        #[source]
        source: std::time::SystemTimeError,
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
        marker_tree: String,
        head_tree: String,
    },
    /// failed to read workspace git state: {0}
    Git(#[from] DriverGitError),
}

fn assert_porcelain_clean(workspace: &Path) -> Result<(), MarkerError> {
    let porcelain = git::status_porcelain_sync(workspace)?;
    if !porcelain.is_empty() {
        return Err(MarkerError::PorcelainDirty);
    }
    Ok(())
}

fn git_tree_oid_of_head(workspace: &Path) -> Result<String, DriverGitError> {
    git::head_tree_oid_sync(workspace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command as Cmd;
    use tempfile::TempDir;

    fn init_test_workspace() -> TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        loom_driver::git::init_test_repo(dir.path()).expect("init test repo");
        dir
    }

    fn head_tree(dir: &Path) -> String {
        git_tree_oid_of_head(dir).expect("tree oid")
    }

    fn head_sha(_dir: &Path) -> String {
        "0000000000000000000000000000000000000000".to_string()
    }

    fn write_marker_at(workspace: &Path, marker: &MarkerProof) {
        let path = workspace.join(MARKER_PATH);
        fs::create_dir_all(path.parent().expect("marker parent")).expect("mkdir .loom");
        let bytes = serde_json::to_vec_pretty(marker).expect("serialise marker");
        fs::write(&path, bytes).expect("write marker");
    }

    #[test]
    fn verify_marker_exits_zero_on_match() {
        let dir = init_test_workspace();
        let workspace = dir.path();
        let marker = MarkerProof {
            version: 1,
            commit_sha: head_sha(workspace),
            tree_oid: head_tree(workspace),
            minted_at_ms: 0,
        };
        write_marker_at(workspace, &marker);
        let result = verify_marker(workspace);
        assert!(
            result.is_ok(),
            "verify-marker must succeed for matching marker: {result:?}",
        );
    }

    #[test]
    fn verify_marker_exits_nonzero_on_missing() {
        let dir = init_test_workspace();
        let workspace = dir.path();
        let result = verify_marker(workspace);
        match result {
            Err(MarkerError::MissingMarker { .. }) => {}
            other => panic!("expected MissingMarker, got {other:?}"),
        }
    }

    #[test]
    fn verify_marker_exits_nonzero_on_tree_mismatch() {
        let dir = init_test_workspace();
        let workspace = dir.path();
        let marker = MarkerProof {
            version: 1,
            commit_sha: head_sha(workspace),
            tree_oid: "deadbeefcafe1234567890abcdef0123456789ab".to_string(),
            minted_at_ms: 0,
        };
        write_marker_at(workspace, &marker);
        let result = verify_marker(workspace);
        match result {
            Err(MarkerError::FingerprintMismatch { .. }) => {}
            other => panic!("expected FingerprintMismatch, got {other:?}"),
        }
    }

    #[test]
    fn verify_marker_exits_nonzero_on_dirty_tree() {
        let dir = init_test_workspace();
        let workspace = dir.path();
        let marker = MarkerProof {
            version: 1,
            commit_sha: head_sha(workspace),
            tree_oid: head_tree(workspace),
            minted_at_ms: 0,
        };
        write_marker_at(workspace, &marker);
        std::fs::write(workspace.join("README.md"), "dirty contents\n").expect("dirty edit");
        let result = verify_marker(workspace);
        match result {
            Err(MarkerError::PorcelainDirty) => {}
            other => panic!("expected PorcelainDirty, got {other:?}"),
        }
    }

    fn install_executable(bin_dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(bin_dir).expect("mkdir bin");
        let path = bin_dir.join(name);
        std::fs::write(&path, body).expect("write script");
        let mut perm = std::fs::metadata(&path).expect("stat").permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&path, perm).expect("chmod");
    }

    fn pre_push_checks_available() -> bool {
        Cmd::new("pre-push-checks")
            .arg("nonexistent-command-zzz")
            .output()
            .is_ok()
    }

    fn run_pre_push_checks(workspace: &Path, bin_dir: &Path) -> std::process::Output {
        let path_var = std::env::var_os("PATH").unwrap_or_default();
        let mut entries = vec![bin_dir.to_path_buf()];
        entries.extend(std::env::split_paths(&path_var));
        let new_path = std::env::join_paths(entries).expect("join PATH");
        Cmd::new("pre-push-checks")
            .arg("sentinel")
            .current_dir(workspace)
            .env("PATH", new_path)
            .output()
            .expect("spawn pre-push-checks")
    }

    #[test]
    fn pre_push_checks_short_circuits_on_valid_marker() {
        if !pre_push_checks_available() {
            eprintln!("pre-push-checks not on PATH; skipping");
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path();
        std::fs::create_dir_all(workspace.join(".wrapix/loom")).expect("mkdir .wrapix/loom");
        std::fs::write(workspace.join(".wrapix/loom/marker.json"), "{}").expect("write marker");

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

    #[test]
    fn pre_push_checks_falls_through_on_invalid_marker() {
        if !pre_push_checks_available() {
            eprintln!("pre-push-checks not on PATH; skipping");
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path();

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

    fn good_gate_success() -> (tempfile::NamedTempFile, GateSuccess) {
        use std::io::Write;
        let mut log = tempfile::NamedTempFile::new().expect("tempfile");
        log.write_all(b"event-1\nLOOM_COMPLETE\n").expect("write");
        let evidence = crate::gate_outcome::HandoffEvidence {
            verify_exit: Some(0),
            review_exit: Some(0),
            review_marker: Some(loom_protocol::gate::ExitSignal::Complete),
            review_log_path: Some(log.path().to_path_buf()),
        };
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
        let (_log, success) = good_gate_success();
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

        let (_log, success) = good_gate_success();
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
        let dir = tempfile::tempdir().expect("tempdir");
        let (_log, success) = good_gate_success();
        let clock = loom_driver::clock::SystemClock::new();
        let result = MarkerProof::mint(success, dir.path(), &clock);
        assert!(matches!(result, Err(MintError::Git(_))), "got {result:?}");
    }
}

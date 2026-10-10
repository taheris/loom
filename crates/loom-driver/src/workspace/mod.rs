//! Host-owned association of local checkouts with Evidence's canonical history.

mod launch;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use displaydoc::Display;
use rustix::fs::{Mode, Stat};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::scratch::directory::Directory;

pub use launch::validate_spawn;

const BINDING: &str = "loom-workspace.json";
const RECORD: &str = "association.json";

#[derive(Debug, Display, Error)]
pub enum Error {
    /// workspace authority filesystem operation failed
    Io(#[from] std::io::Error),
    /// workspace authority record is malformed
    Json(#[from] serde_json::Error),
    /// workspace association is unavailable or changed: {reason}; restore the host binding and retained history before proceeding
    Association { reason: &'static str },
    /// worker mount would expose host authority or override workspace isolation: {path}
    Mount { path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    const fn from_stat(stat: &Stat) -> Self {
        Self {
            device: stat.st_dev,
            inode: stat.st_ino,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkout {
    path: PathBuf,
    directory: Identity,
    git: Identity,
}

impl Checkout {
    fn read(path: &Path) -> Result<Self, Error> {
        let path = std::path::absolute(path)?;
        let directory = Directory::open(&path)?;
        if disposable(&path) {
            return Err(Error::Association {
                reason: "disposable checkouts cannot own safety authority",
            });
        }
        let git = directory.child(Path::new(".git"))?;
        Ok(Self {
            path,
            directory: Identity::from_stat(&directory.stat()?),
            git: Identity::from_stat(&git.stat()?),
        })
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    registry: PathBuf,
    identity: Identity,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    operator: Checkout,
    integration: Checkout,
    history: Identity,
}

/// A checked host capability, never constructible from a worker report or digest.
#[derive(Debug)]
pub struct Association {
    binding: Binding,
    record: Record,
    history: PathBuf,
}

impl Association {
    /// Establish once during trusted workspace initialization; existing authority must resolve.
    ///
    /// # Errors
    /// Missing, replaced, partial, or unsafe existing authority is not reinitialized.
    pub fn establish(operator: &Path) -> Result<Self, Error> {
        if operator.join(".git").join(BINDING).try_exists()? {
            return Self::resolve(operator);
        }
        let state_home = if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
            PathBuf::from(path)
        } else {
            PathBuf::from(std::env::var_os("HOME").ok_or(Error::Association {
                reason: "HOME and XDG_STATE_HOME are unavailable",
            })?)
            .join(".local/state")
        };
        Self::establish_with_state_home(operator, &state_home)
    }

    /// Establish in an explicit host state directory, without rebinding existing checkouts.
    ///
    /// # Errors
    /// State inside the operator checkout and partial previous initialization are rejected.
    pub fn establish_with_state_home(operator: &Path, state_home: &Path) -> Result<Self, Error> {
        if operator.join(".git").join(BINDING).try_exists()? {
            return Self::resolve(operator);
        }
        let operator = Checkout::read(operator)?;
        let integration = Checkout::read(&operator.path.join(".loom/integration"))?;
        let state_home = std::path::absolute(state_home)?;
        if state_home.starts_with(&operator.path) || operator.path.starts_with(&state_home) {
            return Err(Error::Association {
                reason: "state directory overlaps the operator checkout",
            });
        }
        let state = Directory::ensure(&state_home)?.ensure_child(Path::new("loom-safety"))?;
        state.require_private()?;
        let key = format!("{:x}", Sha256::digest(serde_json::to_vec(&operator.path)?));
        let registry = state.create_child(Path::new(&key))?;
        let history = registry.create_child(Path::new("history"))?;
        let binding = Binding {
            registry: state_home.join("loom-safety").join(key),
            identity: Identity::from_stat(&registry.stat()?),
        };
        let record = Record {
            version: 1,
            operator,
            integration,
            history: Identity::from_stat(&history.stat()?),
        };
        history.sync()?;
        write_record(&registry, RECORD, &record)?;
        registry.sync()?;
        state.sync()?;
        for checkout in [&record.integration, &record.operator] {
            let git = Directory::open(&checkout.path.join(".git"))?;
            write_record(&git, BINDING, &binding)?;
            git.sync()?;
        }
        Self::resolve(&record.operator.path)
    }

    /// Resolve only reciprocal, owner-private bindings for the exact host checkout and Git directory.
    ///
    /// # Errors
    /// Unassociated checkouts, copied bindings, missing history, and replaced surfaces fail closed.
    pub fn resolve(checkout: &Path) -> Result<Self, Error> {
        let checkout = Checkout::read(checkout)?;
        let git = Directory::open(&checkout.path.join(".git"))?;
        let binding: Binding = read_record(&git, BINDING)?;
        let registry = Directory::open(&binding.registry)?;
        registry.require_private()?;
        if Identity::from_stat(&registry.stat()?) != binding.identity {
            return Err(Error::Association {
                reason: "registry directory was replaced",
            });
        }
        let record: Record = read_record(&registry, RECORD)?;
        if record.version != 1
            || (checkout != record.operator && checkout != record.integration)
            || record.integration.path != record.operator.path.join(".loom/integration")
            || binding.registry.starts_with(&record.operator.path)
        {
            return Err(Error::Association {
                reason: "checkout is not a registered host participant",
            });
        }
        for expected in [&record.operator, &record.integration] {
            if Checkout::read(&expected.path)? != *expected {
                return Err(Error::Association {
                    reason: "registered checkout or Git directory was replaced",
                });
            }
            let git = Directory::open(&expected.path.join(".git"))?;
            if read_record::<Binding>(&git, BINDING)? != binding {
                return Err(Error::Association {
                    reason: "checkout bindings disagree",
                });
            }
        }
        let history = registry.child(Path::new("history"))?;
        history.require_private()?;
        if Identity::from_stat(&history.stat()?) != record.history {
            return Err(Error::Association {
                reason: "canonical history directory was replaced",
            });
        }
        Ok(Self {
            history: binding.registry.join("history"),
            binding,
            record,
        })
    }

    /// Require the same current association before Evidence reads or admits facts.
    ///
    /// # Errors
    /// Replacement or loss of any required authority surface invalidates this capability.
    pub fn recheck(&self) -> Result<(), Error> {
        let current = Self::resolve(&self.record.operator.path)?;
        if current.record != self.record || current.binding != self.binding {
            return Err(Error::Association {
                reason: "workspace association changed",
            });
        }
        Ok(())
    }

    /// Evidence owns journal, immutable witness, and consolidation/checkpoint state beneath this root.
    pub fn history_dir(&self) -> &Path {
        &self.history
    }

    pub fn operator(&self) -> &Path {
        &self.record.operator.path
    }

    pub fn integration(&self) -> &Path {
        &self.record.integration.path
    }
}

fn disposable(path: &Path) -> bool {
    path.ancestors().any(|ancestor| {
        matches!(
            ancestor.file_name().and_then(|name| name.to_str()),
            Some("beads" | "tune" | "scratch")
        ) && ancestor
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == ".loom")
    })
}

fn read_record<T: serde::de::DeserializeOwned>(
    directory: &Directory,
    name: &str,
) -> Result<T, Error> {
    let mut bytes = Vec::new();
    directory
        .read_private_file(Path::new(name))?
        .take(65537)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err(Error::Association {
            reason: "authority record exceeds its size bound",
        });
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn write_record(directory: &Directory, name: &str, value: &impl Serialize) -> Result<(), Error> {
    let mut file = directory.create_file(Path::new(name), Mode::RUSR | Mode::WUSR)?;
    file.write_all(&serde_json::to_vec(value)?)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;
    use crate::agent::{MountSpec, SpawnConfig};
    use crate::scratch::{ScratchSession, runtime};

    struct Fixture {
        root: tempfile::TempDir,
        association: Association,
        worker: PathBuf,
        profile: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let operator = root.path().join("operator");
            fs::create_dir_all(operator.join(".git")).unwrap();
            fs::create_dir_all(operator.join(".loom/integration/.git")).unwrap();
            let association =
                Association::establish_with_state_home(&operator, &root.path().join("state"))
                    .unwrap();
            let worker = operator.join(".loom/beads/lm-test");
            fs::create_dir_all(worker.join(".git")).unwrap();
            let profile = root.path().join("profile.json");
            fs::write(&profile, r#"{"profile":{"mounts":[]}}"#).unwrap();
            Self {
                root,
                association,
                worker,
                profile,
            }
        }

        fn spawn(&self, scratch: &ScratchSession) -> SpawnConfig {
            let mut config: SpawnConfig = serde_json::from_value(serde_json::json!({
                "image_ref": "fixture-image", "workspace": self.worker, "env": [["LOOM_INSIDE", "1"]],
                "initial_prompt": "feedback", "agent_args": [],
                "repin": {"orientation":"", "pinned_context":"", "partial_bodies":[]}
            }))
            .unwrap();
            config.profile_config = Some(self.profile.clone());
            config.scratch_dir = scratch.path().to_path_buf();
            config
        }
    }

    #[test]
    fn operator_and_integration_resolve_the_same_external_history() {
        let fixture = Fixture::new();
        let operator = Association::resolve(fixture.association.operator()).unwrap();
        let integration = Association::resolve(fixture.association.integration()).unwrap();
        assert_eq!(operator.history_dir(), integration.history_dir());
        assert!(!operator.history_dir().starts_with(operator.operator()));
        fs::write(
            operator.history_dir().join("host-fixture"),
            b"host-owned bytes, not admitted evidence",
        )
        .unwrap();
        assert_eq!(
            fs::read(integration.history_dir().join("host-fixture")).unwrap(),
            b"host-owned bytes, not admitted evidence"
        );
    }

    #[test]
    fn copied_binding_and_worker_identity_cannot_resolve_host_authority() {
        let fixture = Fixture::new();
        fs::copy(
            fixture.association.operator().join(".git").join(BINDING),
            fixture.worker.join(".git").join(BINDING),
        )
        .unwrap();
        assert!(Association::resolve(&fixture.worker).is_err());
        assert!(
            Association::establish_with_state_home(
                &fixture.worker,
                &fixture.root.path().join("forged-state")
            )
            .is_err()
        );
        let unrelated = fixture.root.path().join("other/operator");
        fs::create_dir_all(unrelated.join(".git")).unwrap();
        fs::copy(
            fixture.association.operator().join(".git").join(BINDING),
            unrelated.join(".git").join(BINDING),
        )
        .unwrap();
        assert!(Association::resolve(&unrelated).is_err());
    }

    #[test]
    fn existing_binding_cannot_be_rebound_by_another_state_home() {
        let fixture = Fixture::new();
        let other_state = fixture.root.path().join("new-state");
        let resolved =
            Association::establish_with_state_home(fixture.association.operator(), &other_state)
                .unwrap();
        assert_eq!(resolved.history_dir(), fixture.association.history_dir());
        assert!(!other_state.exists());
    }

    #[test]
    fn missing_history_is_not_reinitialized_as_empty() {
        let fixture = Fixture::new();
        fs::remove_dir(fixture.association.history_dir()).unwrap();
        assert!(fixture.association.recheck().is_err());
        assert!(
            Association::establish_with_state_home(
                fixture.association.operator(),
                &fixture.root.path().join("new-state")
            )
            .is_err()
        );
        assert!(!fixture.association.history_dir().exists());
    }

    #[test]
    fn replaced_history_invalidates_an_existing_capability() {
        let fixture = Fixture::new();
        let history = fixture.association.history_dir();
        fs::rename(history, history.with_extension("preserved")).unwrap();
        fs::create_dir(history).unwrap();
        fs::set_permissions(history, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(fixture.association.recheck().is_err());
    }

    #[test]
    fn replaced_git_directory_cannot_reset_the_association() {
        let fixture = Fixture::new();
        let git = fixture.association.operator().join(".git");
        fs::rename(&git, git.with_extension("preserved")).unwrap();
        fs::create_dir(&git).unwrap();
        assert!(fixture.association.recheck().is_err());
        assert!(
            Association::establish_with_state_home(
                fixture.association.operator(),
                &fixture.root.path().join("state")
            )
            .is_err()
        );
    }

    #[test]
    fn private_record_symlink_is_not_authority() {
        let fixture = Fixture::new();
        let binding = fixture.association.operator().join(".git").join(BINDING);
        let moved = binding.with_extension("preserved");
        fs::rename(&binding, &moved).unwrap();
        symlink(moved, binding).unwrap();
        assert!(fixture.association.recheck().is_err());
    }

    #[test]
    fn unsafe_mounts_are_denied_at_the_common_config_writer() {
        let fixture = Fixture::new();
        let scratch = ScratchSession::open(&fixture.worker, "test", "feedback", "test").unwrap();
        let sources = [
            fixture.association.history_dir().to_path_buf(),
            fixture.association.history_dir().join("witnesses/object"),
            fixture.association.history_dir().join("checkpoint.json"),
            fixture.association.operator().join(".git").join(BINDING),
            fixture.association.integration().join(".loom/marker.json"),
            fixture.association.operator().to_path_buf(),
            fixture.root.path().to_path_buf(),
        ];
        for source in sources {
            for read_only in [false, true] {
                let mut config = fixture.spawn(&scratch);
                config.mounts.push(MountSpec {
                    host_path: source.clone(),
                    container_path: PathBuf::from("/extra"),
                    read_only,
                });
                assert!(
                    runtime::Config::write(scratch.path(), &config).is_err(),
                    "{source:?}"
                );
                assert!(!scratch.path().join("spawn-config.json").exists());
            }
        }
    }

    #[test]
    fn profile_mount_alias_cannot_bypass_the_launch_boundary() {
        let fixture = Fixture::new();
        let alias = fixture.root.path().join("cache-alias");
        symlink(fixture.association.history_dir(), &alias).unwrap();
        fs::write(
            &fixture.profile,
            serde_json::json!({"profile":{"mounts":[{"source":alias,"dest":"/cache","mode":"rw"}]}})
                .to_string(),
        )
        .unwrap();
        let scratch = ScratchSession::open(&fixture.worker, "test", "feedback", "test").unwrap();
        assert!(runtime::Config::write(scratch.path(), &fixture.spawn(&scratch)).is_err());
    }

    #[test]
    fn worker_local_reports_do_not_change_host_owned_surfaces() {
        let fixture = Fixture::new();
        let paths = ["journal.jsonl", "witness/object", "checkpoint.json"];
        for path in paths {
            let host = fixture.association.history_dir().join(path);
            fs::create_dir_all(host.parent().unwrap()).unwrap();
            fs::write(host, b"host-fixture").unwrap();
            let report = fixture.worker.join(".loom/safety").join(path);
            fs::create_dir_all(report.parent().unwrap()).unwrap();
            fs::write(report, b"forged-identity-and-outcome").unwrap();
        }
        let scratch = ScratchSession::open(&fixture.worker, "test", "feedback", "test").unwrap();
        runtime::Config::write(scratch.path(), &fixture.spawn(&scratch)).unwrap();
        for path in paths {
            assert_eq!(
                fs::read(fixture.association.history_dir().join(path)).unwrap(),
                b"host-fixture"
            );
        }
    }
}

//! Admission is bounded; uncertain execution lifetime never grants disposal authority.

mod collection;
mod inventory;
mod policy;
#[cfg(test)]
mod process;
#[cfg(test)]
mod tests;

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use policy::{Bytes, Policy, Request};

use super::record::{Lifetime, Placement, Record};
use super::{Association, Error, Identity};
use crate::clock::Clock;
use crate::scratch::directory::Directory;
use crate::state::{CacheDb, RecordKind};

/// Native protection purpose; distinct owners cannot release each other's holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Operator,
    Recovery,
    Evidence,
    Runtime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct Owner([u8; 32]);

/// A native protection with an immutable owner reference, not an agent status flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hold {
    kind: Kind,
    owner: Owner,
}

impl Hold {
    /// Bind a hold to one canonical immutable evidence, runtime or operator reference.
    ///
    /// # Errors
    /// Empty references cannot stand in for an identified protection owner.
    pub fn new(kind: Kind, reference: &[u8]) -> std::io::Result<Self> {
        use sha2::Digest as _;
        if reference.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Cargo hold requires an owner reference",
            ));
        }
        Ok(Self {
            kind,
            owner: Owner(sha2::Sha256::digest(reference).into()),
        })
    }
}

/// Conservative allocated storage plus the unmaterialized part of reservations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub allocated: Bytes,
    pub outstanding: Bytes,
    pub charged: Bytes,
    pub free: Bytes,
}

enum Scope<'a> {
    Worker(&'a Association),
    Trusted { path: &'a Path, identity: Identity },
}

impl Scope<'_> {
    fn base(&self) -> &Path {
        match self {
            Self::Worker(association) => association.operator(),
            Self::Trusted { path, .. } => path,
        }
    }

    fn recheck(&self) -> Result<(), Error> {
        match self {
            Self::Worker(association) => association.recheck(),
            Self::Trusted { path, identity } => {
                let directory = Directory::open(path)?;
                directory.require_private()?;
                require_identity(&directory, identity)
            }
        }
    }
}

pub(super) struct Fence<'a> {
    scope: Scope<'a>,
    pub(super) root: Directory,
    pub(super) database: CacheDb,
    _lock: File,
}

impl<'a> Fence<'a> {
    pub(super) fn open(association: &'a Association) -> Result<Self, Error> {
        association.recheck()?;
        let root = Directory::open(association.operator())?
            .ensure_child(Path::new(".loom"))?
            .ensure_child(Path::new("cargo-artifacts"))?;
        let state = Directory::open(association.operator())?.child(Path::new(".loom"))?;
        Self::locked(
            Scope::Worker(association),
            root,
            &state,
            &crate::state::path(association.operator()),
        )
    }

    pub(super) fn trusted(path: &'a Path) -> Result<Self, Error> {
        let root = Directory::ensure(path)?;
        let state = Directory::open(path)?;
        let identity = Identity::from_stat(&root.stat()?);
        Self::locked(
            Scope::Trusted { path, identity },
            root,
            &state,
            &path.join("state.db"),
        )
    }

    fn locked(
        scope: Scope<'a>,
        root: Directory,
        state: &Directory,
        database_path: &Path,
    ) -> Result<Self, Error> {
        root.require_private()?;
        let lock = root.append_private_file(Path::new("retention.lock"))?;
        match &scope {
            Scope::Trusted { .. } => lock.lock()?,
            Scope::Worker(_) => match lock.try_lock() {
                Ok(()) => {}
                Err(TryLockError::WouldBlock) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        "generated Cargo allocation or collection is busy; retry independently",
                    )
                    .into());
                }
                Err(TryLockError::Error(error)) => return Err(error.into()),
            },
        }
        inventory::require_state(state)?;
        let database = CacheDb::open(database_path)?;
        inventory::require_state(state)?;
        scope.recheck()?;
        Ok(Self {
            scope,
            root,
            _lock: lock,
            database,
        })
    }

    pub(super) fn admit(
        &self,
        policy: &Policy,
        reservation: Bytes,
        clock: &dyn Clock,
    ) -> Result<Report, Error> {
        let report = if reservation <= policy.max_bytes {
            self.collect(policy, reservation, clock)?
        } else {
            self.report(policy)?
        };
        if !fits(&report, policy, reservation)? {
            return Err(Error::CargoAdmission {
                charged: report.charged,
                reservation,
                limit: policy.max_bytes,
                free: report.free,
                headroom: policy.min_free_bytes.add(reservation)?,
            });
        }
        Ok(report)
    }

    pub(super) fn report(&self, policy: &Policy) -> Result<Report, Error> {
        for _ in 0..16 {
            match self.report_once(policy) {
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    tracing::debug!(
                        ?error,
                        "Cargo output changed during accounting; retrying snapshot"
                    );
                    std::thread::yield_now();
                }
                result => return result,
            }
        }
        self.report_once(policy)
    }

    fn report_once(&self, policy: &Policy) -> Result<Report, Error> {
        self.scope.recheck()?;
        let mut allocated = Bytes::default();
        let mut outstanding = Bytes::default();
        let mut excluded = std::collections::BTreeSet::new();
        let device = self.root.stat()?.st_dev;
        for (key, record) in self.database.records::<Record>(RecordKind::CargoTarget)? {
            let (_, directory) =
                self.stored_directory(&key, &record)?
                    .ok_or(Error::Association {
                        reason: "generated Cargo accounting has an unfinished removal; retained",
                    })?;
            let identity = directory.stat()?;
            if identity.st_dev != device {
                return Err(Error::Association {
                    reason: "generated Cargo namespace crosses the owned storage filesystem",
                });
            }
            excluded.insert((identity.st_dev, identity.st_ino));
            let used = directory.allocated_bytes()?;
            allocated = allocated.add(Bytes::new(used))?;
            let reservation = if record.version != 2 || record.lifetime == Lifetime::Unreconciled {
                record.reservation.max(policy.unknown_reservation())
            } else {
                record.reservation
            };
            outstanding = outstanding.add(Bytes::new(reservation.get().saturating_sub(used)))?;
        }
        for directory in self.unrecorded(&excluded)? {
            let identity = directory.stat()?;
            if identity.st_dev != device {
                return Err(Error::Association {
                    reason: "unrecorded Cargo namespace crosses the storage filesystem; retained",
                });
            }
            excluded.insert((identity.st_dev, identity.st_ino));
            let used = directory.allocated_bytes()?;
            allocated = allocated.add(Bytes::new(used))?;
            outstanding = outstanding.add(Bytes::new(
                policy.unknown_reservation().get().saturating_sub(used),
            ))?;
        }
        allocated = allocated.add(Bytes::new(self.root.allocated_bytes_excluding(&excluded)?))?;
        let charged = allocated.add(outstanding)?;
        let available = rustix::fs::fstatvfs(&self.root).map_err(std::io::Error::from)?;
        if available.f_frsize == 0 {
            return Err(std::io::Error::other(
                "generated Cargo free-space block size is unavailable",
            )
            .into());
        }
        let free = available
            .f_bavail
            .checked_mul(available.f_frsize)
            .map(Bytes::new)
            .ok_or_else(|| {
                std::io::Error::other("generated Cargo free-space accounting overflowed")
            })?;
        Ok(Report {
            allocated,
            outstanding,
            charged,
            free,
        })
    }

    pub(super) fn paths(&self, key: &str, record: &Record) -> Result<(PathBuf, PathBuf), Error> {
        let context = match (&self.scope, &record.bead) {
            (Scope::Worker(association), Some(bead))
                if record.profile.is_some()
                    && record.source
                        == association
                            .operator()
                            .join(".loom/beads")
                            .join(bead.as_str()) =>
            {
                association
                    .operator()
                    .join(".loom/cargo-artifacts/worker")
                    .join(bead.as_str())
                    .join(record.context.name())
            }
            (Scope::Trusted { path, .. }, None) if record.profile.is_none() => {
                path.join("native").join(record.context.name())
            }
            _ => {
                return Err(Error::Association {
                    reason: "Cargo record belongs to a different storage trust domain; retained",
                });
            }
        };
        let original = record.target.parent().ok_or(Error::Association {
            reason: "generated Cargo record has no namespace",
        })?;
        let name =
            original
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or(Error::Association {
                    reason: "generated Cargo record has an invalid namespace",
                })?;
        let expected_key = original
            .strip_prefix(self.scope.base())
            .map_err(std::io::Error::other)?;
        if record
            .target
            .file_name()
            .is_none_or(|name| name != "target")
            || original.parent() != Some(context.as_path())
            || !name.starts_with("generation-")
            || name.len() <= "generation-".len()
            || expected_key.to_str() != Some(key)
        {
            return Err(Error::Association {
                reason: "conflicting generated Cargo ownership or layout; retained",
            });
        }
        Ok((original.into(), context.join(format!(".quarantine-{name}"))))
    }
}

pub(super) const PROJECTION: &str = "projection";

pub(super) fn projected(directory: &Directory) -> Result<bool, Error> {
    match rustix::fs::statat(directory, PROJECTION, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(true),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(error) => Err(std::io::Error::from(error).into()),
    }
}

fn fits(report: &Report, policy: &Policy, reservation: Bytes) -> Result<bool, Error> {
    Ok(report.charged.add(reservation)? <= policy.max_bytes
        && report.free >= policy.min_free_bytes.add(reservation)?)
}

pub(super) fn require_identity(directory: &Directory, identity: &Identity) -> Result<(), Error> {
    if Identity::from_stat(&directory.stat()?) != *identity {
        return Err(Error::Association {
            reason: "generated Cargo filesystem identity changed; retained",
        });
    }
    Ok(())
}

/// Collect only provably never-dispatched abandoned generations, or retain them.
///
/// # Errors
/// Ownership, accounting and unavailable storage errors stop collection without widening its scope.
pub fn sweep(
    association: &Association,
    policy: &Policy,
    clock: &dyn Clock,
) -> Result<Report, Error> {
    let fence = Fence::open(association)?;
    fence.collect(policy, Bytes::default(), clock)
}

/// Change one native hold under the same fence used by allocation and collection.
///
/// # Errors
/// Unrecorded or substituted namespaces are not silently rebound.
pub fn set_hold(
    association: &Association,
    target: &Path,
    hold: Hold,
    action: HoldAction,
) -> Result<(), Error> {
    let fence = Fence::open(association)?;
    fence.change_hold(target, hold, action)
}

/// Inspect or collect only never-dispatched output in the shared per-user native pool.
///
/// # Errors
/// Untrusted policy, controller state or ambiguous ownership refuses collection.
pub fn sweep_trusted(clock: &dyn Clock) -> Result<Report, Error> {
    let output = super::trusted_root()?;
    let fence = Fence::trusted(&output)?;
    fence.collect(
        &super::trusted::policy(&fence.root)?,
        Bytes::default(),
        clock,
    )
}

/// Change one native-pool hold without altering any other owner's protection.
///
/// # Errors
/// Unrecorded, foreign or substituted targets cannot receive holds.
pub fn set_trusted_hold(target: &Path, hold: Hold, action: HoldAction) -> Result<(), Error> {
    let output = super::trusted_root()?;
    let fence = Fence::trusted(&output)?;
    fence.change_hold(target, hold, action)
}

impl Fence<'_> {
    pub(super) fn change_hold(
        &self,
        target: &Path,
        hold: Hold,
        action: HoldAction,
    ) -> Result<(), Error> {
        let namespace = target.parent().ok_or(Error::Association {
            reason: "Cargo hold has no namespace",
        })?;
        let key = namespace
            .strip_prefix(self.scope.base())
            .map_err(std::io::Error::other)?
            .to_string_lossy()
            .into_owned();
        let mut record: Record =
            self.database
                .record(RecordKind::CargoTarget, &key)?
                .ok_or(Error::Association {
                    reason: "unrecorded Cargo namespace cannot receive a hold",
                })?;
        if record.target != target {
            return Err(Error::Association {
                reason: "Cargo hold target does not match its ownership record",
            });
        }
        self.stored_directory(&key, &record)?
            .ok_or(Error::Association {
                reason: "removed Cargo namespace cannot receive a hold",
            })?;
        match action {
            HoldAction::Acquire => {
                record.holds.insert(hold);
            }
            HoldAction::Release => {
                record.holds.remove(&hold);
            }
        }
        self.database
            .put_record(RecordKind::CargoTarget, &key, &record)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum HoldAction {
    Acquire,
    Release,
}

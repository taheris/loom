use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};

use super::{
    Bytes, Clock, Directory, Error, Fence, Identity, Lifetime, Placement, Policy, Record,
    RecordKind, fits, require_identity,
};

impl Fence<'_> {
    pub(in crate::workspace::cargo) fn collect(
        &self,
        policy: &Policy,
        reservation: Bytes,
        clock: &dyn Clock,
    ) -> Result<super::Report, Error> {
        let mut records = self.database.records::<Record>(RecordKind::CargoTarget)?;
        records.sort_by_key(|(_, record)| record.created);
        let mut identities = std::collections::BTreeSet::new();
        for (key, record) in &records {
            self.stored_directory(key, record)?;
            if !identities.insert((
                record.namespace_identity.device,
                record.namespace_identity.inode,
            )) {
                return Err(Error::Association {
                    reason: "conflicting Cargo generation ownership records; retained",
                });
            }
        }
        let mut pressure = Vec::new();
        for (key, mut record) in records {
            let expired = match record.created {
                Some(created) => match clock.wall_now().duration_since(created) {
                    Ok(age) => age >= policy.max_age(),
                    Err(error) => {
                        tracing::warn!(?error, namespace = %key, "Cargo generation age is unavailable; retaining on age policy");
                        false
                    }
                },
                None => false,
            };
            if record.placement != Placement::Live || expired {
                self.remove(&key, &mut record)?;
            } else {
                pressure.push((key, record));
            }
        }
        let mut report = self.report(policy)?;
        for (key, mut record) in pressure {
            if fits(&report, policy, reservation)? {
                break;
            }
            if self.remove(&key, &mut record)? == Outcome::Collected {
                report = self.report(policy)?;
            }
        }
        Ok(report)
    }

    pub(in crate::workspace::cargo) fn stored_directory(
        &self,
        key: &str,
        record: &Record,
    ) -> Result<Option<(PathBuf, Directory)>, Error> {
        let (original, quarantine) = self.paths(key, record)?;
        let original_exists = exists(&original)?;
        let quarantine_exists = exists(&quarantine)?;
        let path = match (
            record.placement == Placement::Live,
            original_exists,
            quarantine_exists,
        ) {
            (true, true, false) => original,
            (false, false, true) => quarantine,
            (false, true, false) if record.placement == Placement::Quarantining => original,
            (false, false, false) if record.placement == Placement::Removing => return Ok(None),
            _ => {
                return Err(Error::Association {
                    reason: "ambiguous generated Cargo quarantine placement; retained",
                });
            }
        };
        let directory = Directory::open(&path)?;
        directory.require_private()?;
        require_identity(&directory, &record.namespace_identity)?;
        Ok(Some((path, directory)))
    }

    fn remove(&self, key: &str, record: &mut Record) -> Result<Outcome, Error> {
        if !matches!(record.version, 2 | 3)
            || (record.version == 2 && record.lifetime != Lifetime::Prepared)
            || record.lifetime == Lifetime::Unreconciled
            || record.created.is_none()
            || !record.holds.is_empty()
        {
            tracing::warn!(namespace = %key, "Cargo generation has a hold or unreconciled execution lifetime; retained");
            return Ok(Outcome::Retained);
        }
        let stored = self.stored_directory(key, record)?;
        match Directory::open(&record.source) {
            Ok(source) if Identity::from_stat(&source.stat()?) == record.source_identity => {}
            Ok(_) => {
                tracing::warn!(namespace = %key, "Cargo source ownership changed; retaining generation");
                return Ok(Outcome::Retained);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && record.version == 3 => {
                let safe = if let Some((_, directory)) = &stored {
                    match super::super::lifetime::source_absent_safe(directory, record) {
                        Ok(safe) => safe,
                        Err(error) => {
                            tracing::warn!(?error, namespace = %key, "missing-source protection is unavailable");
                            false
                        }
                    }
                } else {
                    record.placement == Placement::Removing
                };
                if !safe {
                    return Ok(Outcome::Retained);
                }
            }
            Err(error) => {
                tracing::warn!(?error, namespace = %key, "Cargo source ownership is unavailable; retaining generation");
                return Ok(Outcome::Retained);
            }
        }
        let Some((mut path, directory)) = stored else {
            self.recheck(key, record)?;
            if record.version == 3 {
                super::super::lifetime::removed(record, false)?;
            }
            self.database.remove_record(RecordKind::CargoTarget, key)?;
            if record.version == 3 {
                super::super::lifetime::removed(record, true)?;
            }
            return Ok(Outcome::Collected);
        };
        if record.version == 2
            && (super::projected(&directory)? || super::super::lifetime::journal_exists(record)?)
        {
            tracing::warn!(namespace = %key, "Cargo projection tombstone exists; retaining generation");
            return Ok(Outcome::Retained);
        }
        let (original, quarantine) = self.paths(key, record)?;
        if std::env::current_exe()?.starts_with(&path) {
            tracing::warn!(namespace = %key, "Cargo generation contains the executing runtime; retained");
            return Ok(Outcome::Retained);
        }
        let Some(_lease) = lease(&directory, record)? else {
            return Ok(Outcome::Retained);
        };
        require_target(&directory, record)?;
        self.recheck(key, record)?;
        if record.version == 3 {
            match super::super::lifetime::retire(&directory, record) {
                Ok(true) => super::super::lifetime::preserve(&directory, record)?,
                Ok(false) => return Ok(Outcome::Retained),
                Err(error) => {
                    tracing::warn!(?error, namespace = %key, "Cargo lifetime observation unavailable or conflicting; retained");
                    return Ok(Outcome::Retained);
                }
            }
        }
        let parent = Directory::open(path.parent().ok_or(Error::Association {
            reason: "generated Cargo quarantine has no parent",
        })?)?;
        if record.placement == Placement::Live {
            record.placement = Placement::Quarantining;
            self.database
                .put_record(RecordKind::CargoTarget, key, record)?;
            #[cfg(test)]
            super::tests::cases::checkpoint(super::tests::cases::Checkpoint::Intent)?;
        }
        if path == original {
            rustix::fs::renameat_with(
                &parent,
                file_name(&original)?,
                &parent,
                file_name(&quarantine)?,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(std::io::Error::from)?;
            parent.sync()?;
            path = quarantine;
            #[cfg(test)]
            super::tests::cases::checkpoint(super::tests::cases::Checkpoint::Renamed)?;
        }
        require_identity(&Directory::open(&path)?, &record.namespace_identity)?;
        self.recheck(key, record)?;
        record.placement = Placement::Removing;
        self.database
            .put_record(RecordKind::CargoTarget, key, record)?;
        #[cfg(test)]
        super::tests::cases::checkpoint(super::tests::cases::Checkpoint::Removing)?;
        self.recheck(key, record)?;
        require_target(&directory, record)?;
        parent.remove_child(file_name(&path)?, &directory)?;
        parent.sync()?;
        #[cfg(test)]
        super::tests::cases::checkpoint(super::tests::cases::Checkpoint::Unlinked)?;
        self.database.remove_record(RecordKind::CargoTarget, key)?;
        if record.version == 3 {
            super::super::lifetime::removed(record, true)?;
        }
        tracing::info!(namespace = %key, "collected owned generated Cargo output after fenced lifetime reconciliation");
        Ok(Outcome::Collected)
    }

    fn recheck(&self, key: &str, expected: &Record) -> Result<(), Error> {
        self.scope.recheck()?;
        let current = self
            .database
            .record::<Record>(RecordKind::CargoTarget, key)?;
        if current.as_ref() != Some(expected) {
            return Err(Error::Association {
                reason: "Cargo generation ownership or protection changed; retained",
            });
        }
        Ok(())
    }
}

fn file_name(path: &Path) -> Result<&Path, Error> {
    path.file_name().map(Path::new).ok_or(Error::Association {
        reason: "generated Cargo namespace has no name",
    })
}

fn exists(path: &Path) -> Result<bool, Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn require_target(directory: &Directory, record: &Record) -> Result<(), Error> {
    match directory.child(Path::new("target")) {
        Ok(target) => {
            if let Some(identity) = &record.target_identity {
                require_identity(&target, identity)?;
            }
        }
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && (record.placement == Placement::Removing
                    || record.target_identity.is_none()) => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[derive(PartialEq, Eq)]
enum Outcome {
    Retained,
    Collected,
}

enum Lease {
    Held { _file: File },
    Removed,
}

fn lease(directory: &Directory, record: &Record) -> Result<Option<Lease>, Error> {
    let file = match directory.read_private_file(Path::new("lease")) {
        Ok(file) => file,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && record.placement == Placement::Removing =>
        {
            return Ok(Some(Lease::Removed));
        }
        Err(error) => return Err(error.into()),
    };
    let current = Identity::from_stat(&rustix::fs::fstat(&file).map_err(std::io::Error::from)?);
    if record.lease_identity.as_ref() != Some(&current) {
        return Err(Error::Association {
            reason: "generated Cargo lifetime lease identity changed; retained",
        });
    }
    match file.try_lock() {
        Ok(()) => Ok(Some(Lease::Held { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

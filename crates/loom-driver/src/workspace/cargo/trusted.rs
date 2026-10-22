use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::record::{Lifetime, Placement, Record};
use super::retention::{self, Fence, Policy};
use super::{Directory, Error, Identity, RecordKind, context};
use crate::clock::Clock;

const POLICY: &str = "policy.json";

pub(super) enum Prepared {
    Unreconciled(PathBuf),
    Contained {
        target: PathBuf,
        lease: File,
        entry: PathBuf,
    },
}

/// Raw target exposure has no execution lifetime authority.
pub(super) fn prepare(output: &Path, source: &Path, clock: &dyn Clock) -> Result<PathBuf, Error> {
    prepare_inner(output, source, clock, None).map(|prepared| match prepared {
        Prepared::Unreconciled(target) | Prepared::Contained { target, .. } => target,
    })
}

pub(super) fn prepare_command(
    output: &Path,
    source: &Path,
    clock: &dyn Clock,
) -> Result<Prepared, Error> {
    match super::lifetime::native_root() {
        Ok(Some(root)) => prepare_inner(output, source, clock, Some(&root)),
        Ok(None) => prepare(output, source, clock).map(Prepared::Unreconciled),
        Err(error) => {
            tracing::warn!(
                ?error,
                "native containment unavailable; retaining unreconciled output"
            );
            prepare(output, source, clock).map(Prepared::Unreconciled)
        }
    }
}

fn prepare_inner(
    output: &Path,
    source: &Path,
    clock: &dyn Clock,
    native_root: Option<&Path>,
) -> Result<Prepared, Error> {
    use sha2::Digest as _;
    let fence = Fence::trusted(output)?;
    let policy = policy(&fence.root)?;
    let mut context = context(source, None)?;
    if let Some(root) = native_root {
        let parent = root.parent().ok_or(Error::Association {
            reason: "native cgroup root has no parent",
        })?;
        let scope = Directory::open(parent)?.stat()?;
        let mut hash = sha2::Sha256::new();
        hash.update(b"loom-native-cgroup-v1");
        hash.update(context.0);
        hash.update(parent.as_os_str().as_encoded_bytes());
        hash.update(scope.st_dev.to_le_bytes());
        hash.update(scope.st_ino.to_le_bytes());
        context = super::Context(hash.finalize().into());
        fence.admit(&policy, retention::Bytes::default(), clock)?;
    }
    let source_identity = source_identity(source)?;
    let mut candidates = Vec::new();
    for (key, record) in fence.database.records::<Record>(RecordKind::CargoTarget)? {
        if record.context != context
            || record.lifetime != Lifetime::Dispatched
            || !record.holds.is_empty()
        {
            continue;
        }
        if native_root.is_none() && super::lifetime::journal_exists(&record)? {
            continue;
        }
        if native_root.is_some() {
            let path = record.target.parent().ok_or(Error::Association {
                reason: "native target has no namespace",
            })?;
            if std::env::current_exe()?.starts_with(path) {
                continue;
            }
            let directory = Directory::open(path)?;
            match super::lifetime::reusable(&directory, &record) {
                Ok(true) => (),
                Ok(false) => continue,
                Err(error) => {
                    tracing::warn!(?error, target = ?record.target, "native runtime unavailable; retaining old output");
                    continue;
                }
            }
        }
        candidates.push((key, record));
    }
    if native_root.is_some() {
        candidates.sort_by_key(|(_, record)| std::cmp::Reverse(record.created));
    }
    let mut matching = candidates.into_iter();
    if let Some((key, mut record)) = matching.next() {
        if (native_root.is_none() && matching.next().is_some())
            || record.version != if native_root.is_some() { 3 } else { 2 }
            || record.bead.is_some()
            || record.source != source
            || record.source_identity != source_identity
            || record.placement != Placement::Live
        {
            return Err(Error::Association {
                reason: "ambiguous trusted Cargo context ownership; retained",
            });
        }
        fence.paths(&key, &record)?;
        let path = record.target.parent().ok_or(Error::Association {
            reason: "native target has no namespace",
        })?;
        let namespace = Directory::open(path)?;
        namespace.require_private()?;
        retention::require_identity(&namespace, &record.namespace_identity)?;
        let target = namespace.child(Path::new("target"))?;
        target.require_private()?;
        if record.target_identity.as_ref() != Some(&Identity::from_stat(&target.stat()?))
            || !retention::projected(&namespace)?
        {
            return Err(Error::Association {
                reason: "trusted Cargo target or dispatch protection changed; retained",
            });
        }
        let lease = if native_root.is_some() {
            let lease = namespace.read_private_file(Path::new("lease"))?;
            if record.lease_identity.as_ref()
                != Some(&Identity::from_stat(
                    &rustix::fs::fstat(&lease).map_err(std::io::Error::from)?,
                ))
            {
                return Err(Error::Association {
                    reason: "native execution lease identity changed",
                });
            }
            lease.lock_shared()?;
            Some(lease)
        } else {
            None
        };
        record.reservation = record.reservation.max(policy.reservation_bytes);
        fence
            .database
            .put_record(RecordKind::CargoTarget, &key, &record)?;
        if let (Some(root), Some(lease)) = (native_root, lease) {
            super::lifetime::rearm(&namespace, &record, root)?;
            let entry = super::lifetime::entry(&namespace, &record, path)?;
            fence.admit(&policy, retention::Bytes::default(), clock)?;
            return Ok(Prepared::Contained {
                target: record.target,
                lease,
                entry,
            });
        }
        fence.admit(&policy, retention::Bytes::default(), clock)?;
        return Ok(Prepared::Unreconciled(record.target));
    }
    fence.admit(&policy, policy.reservation_bytes, clock)?;
    let native = fence.root.ensure_child(Path::new("native"))?;
    let parent = native.ensure_child(Path::new(&context.name()))?;
    parent.require_private()?;
    let path = tempfile::Builder::new()
        .prefix("generation-")
        .tempdir_in(output.join("native").join(context.name()))?
        .keep();
    let namespace = Directory::open(&path)?;
    namespace.require_private()?;
    let lease = namespace.append_private_file(Path::new("lease"))?;
    lease.lock()?;
    let target = namespace.create_child(Path::new("target"))?;
    let mut record = Record {
        version: if native_root.is_some() { 3 } else { 2 },
        bead: None,
        source: source.into(),
        source_identity,
        target: path.join("target"),
        namespace_identity: Identity::from_stat(&namespace.stat()?),
        profile: None,
        context,
        lease_identity: Some(Identity::from_stat(
            &rustix::fs::fstat(&lease).map_err(std::io::Error::from)?,
        )),
        target_identity: Some(Identity::from_stat(&target.stat()?)),
        reservation: policy.reservation_bytes,
        created: Some(clock.wall_now()),
        lifetime: Lifetime::Prepared,
        placement: Placement::Live,
        holds: BTreeSet::new(),
    };
    let key = path
        .strip_prefix(output)
        .map_err(std::io::Error::other)?
        .to_str()
        .ok_or(Error::Association {
            reason: "invalid trusted Cargo namespace name",
        })?;
    if native_root.is_some() {
        super::lifetime::initialize(&namespace, &record)?;
    }
    fence
        .database
        .put_record(RecordKind::CargoTarget, key, &record)?;
    fence.admit(&policy, retention::Bytes::default(), clock)?;
    if let Some(root) = native_root {
        namespace
            .append_private_file(Path::new("execution"))?
            .sync_all()?;
        super::lifetime::dispatch_native(&namespace, &record, root)?;
        lease.lock_shared()?;
    } else {
        namespace
            .create_file(
                Path::new(retention::PROJECTION),
                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            )?
            .sync_all()?;
        namespace.sync()?;
    }
    record.lifetime = Lifetime::Dispatched;
    fence
        .database
        .put_record(RecordKind::CargoTarget, key, &record)?;
    if native_root.is_some() {
        let entry = super::lifetime::entry(&namespace, &record, &path)?;
        fence.admit(&policy, retention::Bytes::default(), clock)?;
        Ok(Prepared::Contained {
            target: record.target,
            lease,
            entry,
        })
    } else {
        Ok(Prepared::Unreconciled(record.target))
    }
}

/// Read-only source identity is context, never output ownership or disposal authority.
pub(super) fn source_identity(source: &Path) -> std::io::Result<Identity> {
    let directory = rustix::fs::open(
        source,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    Ok(Identity::from_stat(&rustix::fs::fstat(directory)?))
}

pub(super) fn policy(root: &Directory) -> Result<Policy, Error> {
    let file = match root.read_private_file(Path::new(POLICY)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut file = root.create_file(
                Path::new(POLICY),
                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            )?;
            serde_json::to_writer_pretty(&mut file, &Policy::default())
                .map_err(std::io::Error::other)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            root.sync()?;
            root.read_private_file(Path::new(POLICY))?
        }
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.take(65_537).read_to_end(&mut bytes)?;
    if bytes.len() > 65_536 {
        return Err(std::io::Error::other("trusted Cargo policy exceeds 64 KiB").into());
    }
    serde_json::from_slice(&bytes).map_err(|error| std::io::Error::other(error).into())
}

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::record::{Lifetime, Placement, Record};
use super::retention::{self, Fence, Policy};
use super::{Directory, Error, Identity, RecordKind, context};
use crate::clock::Clock;

const POLICY: &str = "policy.json";

/// Reserve the shared native pool before returning any target to a trusted command.
pub(super) fn prepare(output: &Path, source: &Path, clock: &dyn Clock) -> Result<PathBuf, Error> {
    let fence = Fence::trusted(output)?;
    let policy = policy(&fence.root)?;
    let context = context(source, None)?;
    let source_identity = source_identity(source)?;
    let records = fence.database.records::<Record>(RecordKind::CargoTarget)?;
    let mut matching = records
        .into_iter()
        .filter(|(_, record)| record.context == context && record.lifetime == Lifetime::Dispatched);
    if let Some((key, mut record)) = matching.next() {
        if matching.next().is_some()
            || record.version != 2
            || record.bead.is_some()
            || record.source != source
            || record.source_identity != source_identity
            || record.lifetime != Lifetime::Dispatched
            || record.placement != Placement::Live
        {
            return Err(Error::Association {
                reason: "ambiguous trusted Cargo context ownership; retained",
            });
        }
        fence.paths(&key, &record)?;
        let namespace = Directory::open(record.target.parent().ok_or(Error::Association {
            reason: "trusted Cargo target has no namespace",
        })?)?;
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
        record.reservation = record.reservation.max(policy.reservation_bytes);
        fence
            .database
            .put_record(RecordKind::CargoTarget, &key, &record)?;
        fence.admit(&policy, retention::Bytes::default(), clock)?;
        return Ok(record.target);
    }
    fence.admit(&policy, policy.reservation_bytes, clock)?;
    let native = fence.root.ensure_child(Path::new("native"))?;
    let parent = native.ensure_child(Path::new(&context.name()))?;
    parent.require_private()?;
    let parent_path = output.join("native").join(context.name());
    let path = tempfile::Builder::new()
        .prefix("generation-")
        .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .tempdir_in(&parent_path)?
        .keep();
    let namespace = Directory::open(&path)?;
    namespace.require_private()?;
    let lease = namespace.append_private_file(Path::new("lease"))?;
    lease.lock()?;
    let target = namespace.create_child(Path::new("target"))?;
    let mut record = Record {
        version: 2,
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
    fence
        .database
        .put_record(RecordKind::CargoTarget, key, &record)?;
    fence.admit(&policy, retention::Bytes::default(), clock)?;
    namespace
        .create_file(
            Path::new(retention::PROJECTION),
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )?
        .sync_all()?;
    namespace.sync()?;
    record.lifetime = Lifetime::Dispatched;
    fence
        .database
        .put_record(RecordKind::CargoTarget, key, &record)?;
    Ok(record.target)
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

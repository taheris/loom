use std::collections::BTreeSet;
use std::path::Path;

use rustix::fs::{AtFlags, Dev, FileType, statat};

use super::{Directory, Error, Fence};

impl Fence<'_> {
    pub(super) fn unrecorded(&self, known: &BTreeSet<(Dev, u64)>) -> Result<Vec<Directory>, Error> {
        let worker = match self.root.child(Path::new("worker")) {
            Ok(worker) => worker,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut contexts = Vec::new();
        for owner in children(&worker)? {
            contexts.extend(children(&owner)?);
        }
        let mut generations = Vec::new();
        let mut seen = known.clone();
        for context in contexts {
            for (name, stat) in context.entries()? {
                let bytes = name.as_os_str().as_encoded_bytes();
                if FileType::from_raw_mode(stat.st_mode) != FileType::Directory
                    || !(bytes.starts_with(b"generation-")
                        || bytes.starts_with(b".quarantine-generation-"))
                {
                    continue;
                }
                let generation = context.child(&name)?;
                let current = generation.stat()?;
                if current.st_dev != stat.st_dev || current.st_ino != stat.st_ino {
                    return Err(Error::Association {
                        reason: "unrecorded Cargo namespace changed during accounting; retained",
                    });
                }
                if seen.insert((current.st_dev, current.st_ino)) {
                    generations.push(generation);
                }
            }
        }
        Ok(generations)
    }
}

pub(super) fn require_state(directory: &Directory) -> Result<(), Error> {
    for name in [
        "state.db",
        "state.db-wal",
        "state.db-shm",
        "state.db.lock",
        "state.db.init.lock",
    ] {
        let stat = match statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(rustix::io::Errno::NOENT) => continue,
            Err(error) => return Err(std::io::Error::from(error).into()),
        };
        if stat.st_uid != rustix::process::geteuid().as_raw()
            || stat.st_mode & 0o022 != 0
            || stat.st_nlink != 1
            || FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        {
            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied,
                format!("Cargo controller state file {name} is indirect, shared or writable by another user; retained")
            ).into());
        }
    }
    Ok(())
}

fn children(directory: &Directory) -> Result<Vec<Directory>, Error> {
    directory
        .entries()?
        .into_iter()
        .filter_map(|(name, stat)| {
            (FileType::from_raw_mode(stat.st_mode) == FileType::Directory)
                .then(|| directory.child(&name).map_err(Error::from))
        })
        .collect()
}

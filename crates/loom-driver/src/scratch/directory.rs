use std::fs::File;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Component, Path};

use rustix::fs::{
    AtFlags, Dir, Mode, OFlags, Stat, fchmod, fstat, mkdirat, open, openat, statat, unlinkat,
};
use rustix::io::Errno;
use rustix::process::geteuid;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// A no-follow directory handle whose ancestors cannot be replaced by other users.
#[derive(Debug)]
pub struct Directory(OwnedFd, std::path::PathBuf);

impl AsFd for Directory {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl Directory {
    pub(crate) fn sync(&self) -> io::Result<()> {
        rustix::fs::fsync(&self.0)?;
        Ok(())
    }

    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        if path.as_os_str().is_empty() || path.components().any(|c| c == Component::ParentDir) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid runtime directory path",
            ));
        }
        let path = std::path::absolute(path)?;
        let mut dir = Self(open("/", DIRECTORY_FLAGS, Mode::empty())?, "/".into());
        let mut ancestor = std::path::PathBuf::from("/");
        for component in path.components() {
            if let Component::Normal(name) = component {
                ancestor.push(name);
                dir = dir.child(Path::new(name)).map_err(|error| {
                    if error.raw_os_error().is_some() {
                        error
                    } else {
                        io::Error::new(error.kind(), format!("{}: {error}", ancestor.display()))
                    }
                })?;
            }
        }
        Ok(dir)
    }

    pub(crate) fn ensure(path: &Path) -> io::Result<Self> {
        let path = std::path::absolute(path)?;
        if path.components().any(|c| c == Component::ParentDir) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid directory path",
            ));
        }
        let mut dir = Self(open("/", DIRECTORY_FLAGS, Mode::empty())?, "/".into());
        let mut ancestor = std::path::PathBuf::from("/");
        for component in path.components() {
            if let Component::Normal(name) = component {
                ancestor.push(name);
                dir = dir.ensure_child(Path::new(name)).map_err(|error| {
                    if error.raw_os_error().is_some() {
                        error
                    } else {
                        io::Error::new(error.kind(), format!("{}: {error}", ancestor.display()))
                    }
                })?;
            }
        }
        Ok(dir)
    }

    pub(crate) fn stat(&self) -> io::Result<Stat> {
        Ok(fstat(&self.0)?)
    }

    pub(crate) fn child(&self, name: &Path) -> io::Result<Self> {
        let child = Self(
            openat(&self.0, name, DIRECTORY_FLAGS, Mode::empty())?,
            self.1.join(name),
        );
        let stat = fstat(&child.0)?;
        let trusted_owner = stat.st_uid == geteuid().as_raw() || stat.st_uid == 0;
        let sticky = stat.st_mode & 0o1000 != 0;
        if !trusted_owner || (stat.st_mode & 0o022 != 0 && !sticky) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "runtime ancestor is writable by another user (owner={}, effective_uid={}, mode={:04o})",
                    stat.st_uid,
                    geteuid().as_raw(),
                    stat.st_mode & 0o7777,
                ),
            ));
        }
        Ok(child)
    }

    pub(crate) fn create_child(&self, name: &Path) -> io::Result<Self> {
        mkdirat(&self.0, name, Mode::RWXU)?;
        self.child(name)
    }

    pub(crate) fn ensure_child(&self, name: &Path) -> io::Result<Self> {
        match mkdirat(&self.0, name, Mode::RWXU) {
            Ok(()) | Err(Errno::EXIST) => self.child(name),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn make_private(&self) -> io::Result<()> {
        self.require_owner()?;
        fchmod(&self.0, Mode::RWXU)?;
        Ok(())
    }

    pub(crate) fn require_private(&self) -> io::Result<()> {
        let stat = self.require_owner()?;
        if stat.st_mode & 0o7777 != 0o700 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "runtime directory {} has mode {:04o}; required mode 0700",
                    self.1.display(),
                    stat.st_mode & 0o7777,
                ),
            ));
        }
        Ok(())
    }

    fn require_owner(&self) -> io::Result<Stat> {
        let stat = fstat(&self.0)?;
        if stat.st_uid != geteuid().as_raw() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "runtime directory belongs to another user",
            ));
        }
        Ok(stat)
    }

    pub(crate) fn create_file(&self, name: &Path, mode: Mode) -> io::Result<File> {
        let fd = openat(
            &self.0,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            mode,
        )?;
        Ok(File::from(fd))
    }

    pub(crate) fn append_private_file(&self, name: &Path) -> io::Result<File> {
        self.private_file(name, OFlags::RDWR | OFlags::APPEND | OFlags::CREATE)
    }

    pub(crate) fn read_private_file(&self, name: &Path) -> io::Result<File> {
        self.private_file(name, OFlags::RDWR)
    }

    fn private_file(&self, name: &Path, flags: OFlags) -> io::Result<File> {
        let fd = openat(
            &self.0,
            name,
            flags | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?;
        let stat = fstat(&fd)?;
        if stat.st_uid != geteuid().as_raw()
            || stat.st_mode & 0o7777 != 0o600
            || stat.st_nlink != 1
            || rustix::fs::FileType::from_raw_mode(stat.st_mode)
                != rustix::fs::FileType::RegularFile
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "driver recovery file must be private and singly linked",
            ));
        }
        Ok(File::from(fd))
    }

    pub(crate) fn entries(&self) -> io::Result<Vec<(std::path::PathBuf, Stat)>> {
        use std::os::unix::ffi::OsStrExt as _;
        let mut entries = Vec::new();
        for entry in Dir::read_from(&self.0)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == c"." || name == c".." {
                continue;
            }
            let stat = statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW)?;
            entries.push((
                std::path::PathBuf::from(std::ffi::OsStr::from_bytes(name.to_bytes())),
                stat,
            ));
        }
        Ok(entries)
    }

    pub(crate) fn allocated_bytes(&self) -> io::Result<u64> {
        self.allocated_bytes_excluding(&std::collections::BTreeSet::new())
    }

    pub(crate) fn allocated_bytes_excluding(
        &self,
        excluded: &std::collections::BTreeSet<(rustix::fs::Dev, u64)>,
    ) -> io::Result<u64> {
        use std::os::unix::ffi::OsStrExt as _;

        let device = self.stat()?.st_dev;
        let mut pending = vec![self.child(Path::new("."))?];
        let mut seen = std::collections::BTreeSet::new();
        let mut bytes = 0_u64;
        while let Some(directory) = pending.pop() {
            let stat = directory.stat()?;
            if excluded.contains(&(stat.st_dev, stat.st_ino))
                || seen.contains(&(stat.st_dev, stat.st_ino))
            {
                continue;
            }
            if stat.st_dev != device {
                return Err(io::Error::other(
                    "generated directory crosses a filesystem boundary",
                ));
            }
            count_blocks(&stat, &mut seen, &mut bytes)?;
            for entry in Dir::read_from(&directory.0)? {
                let entry = entry?;
                let name = entry.file_name();
                if name == c"." || name == c".." {
                    continue;
                }
                let stat = statat(&directory.0, name, AtFlags::SYMLINK_NOFOLLOW)?;
                if stat.st_dev != device {
                    return Err(io::Error::other(
                        "generated entry crosses a filesystem boundary",
                    ));
                }
                if rustix::fs::FileType::from_raw_mode(stat.st_mode)
                    == rustix::fs::FileType::Directory
                {
                    let child = Self(
                        openat(&directory.0, name, DIRECTORY_FLAGS, Mode::empty())?,
                        directory
                            .1
                            .join(std::ffi::OsStr::from_bytes(name.to_bytes())),
                    );
                    let current = child.stat()?;
                    if current.st_dev != stat.st_dev || current.st_ino != stat.st_ino {
                        return Err(io::Error::other(
                            "generated directory changed during accounting",
                        ));
                    }
                    pending.push(child);
                } else {
                    count_blocks(&stat, &mut seen, &mut bytes)?;
                }
            }
        }
        Ok(bytes)
    }

    pub(crate) fn remove_file(&self, name: &Path) -> io::Result<()> {
        match unlinkat(&self.0, name, AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn remove_child(&self, name: &Path, child: &Self) -> io::Result<()> {
        let current = match statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(Errno::NOENT) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let owned = fstat(&child.0)?;
        if current.st_dev != owned.st_dev || current.st_ino != owned.st_ino {
            return Err(io::Error::other(
                "scratch directory was replaced before cleanup",
            ));
        }
        clear(&child.0)?;
        let current = statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW)?;
        if current.st_dev != owned.st_dev || current.st_ino != owned.st_ino {
            return Err(io::Error::other(
                "scratch directory was replaced during cleanup",
            ));
        }
        unlinkat(&self.0, name, AtFlags::REMOVEDIR)?;
        Ok(())
    }
}

fn count_blocks(
    stat: &Stat,
    seen: &mut std::collections::BTreeSet<(rustix::fs::Dev, u64)>,
    bytes: &mut u64,
) -> io::Result<()> {
    if seen.insert((stat.st_dev, stat.st_ino)) {
        let allocated = u64::try_from(stat.st_blocks)
            .map_err(io::Error::other)?
            .checked_mul(512)
            .ok_or_else(|| io::Error::other("generated block accounting overflowed"))?;
        *bytes = bytes
            .checked_add(allocated)
            .ok_or_else(|| io::Error::other("generated byte accounting overflowed"))?;
    }
    Ok(())
}

fn clear(dir: &OwnedFd) -> io::Result<()> {
    for entry in Dir::read_from(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == c"." || name == c".." {
            continue;
        }
        match unlinkat(dir, name, AtFlags::empty()) {
            Ok(()) => {}
            Err(Errno::ISDIR | Errno::PERM) => {
                let child = openat(dir, name, DIRECTORY_FLAGS, Mode::empty())?;
                if fstat(&child)?.st_dev != fstat(dir)?.st_dev {
                    return Err(io::Error::other(
                        "scratch cleanup crosses a filesystem boundary",
                    ));
                }
                clear(&child)?;
                unlinkat(dir, name, AtFlags::REMOVEDIR)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use anyhow::Context as _;

    use super::*;

    #[test]
    fn unsafe_ancestor_feedback_identifies_path_owner_and_mode_without_weakening_guard()
    -> anyhow::Result<()> {
        let fixture = tempfile::tempdir()?;
        let unsafe_dir = fixture.path().join("unsafe-ancestor");
        std::fs::create_dir(&unsafe_dir)?;
        std::fs::set_permissions(&unsafe_dir, std::fs::Permissions::from_mode(0o777))?;
        for open in [Directory::open, Directory::ensure] {
            let error = open(&unsafe_dir)
                .err()
                .context("unsafe ancestor was accepted")?;
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            let message = error.to_string();
            assert!(
                message.contains(&unsafe_dir.display().to_string()),
                "{message}"
            );
            assert!(message.contains("owner="), "{message}");
            assert!(message.contains("effective_uid="), "{message}");
            assert!(message.contains("mode=0777"), "{message}");
        }
        let missing = Directory::open(&fixture.path().join("missing"))
            .err()
            .context("missing directory was accepted")?;
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
        assert_eq!(missing.raw_os_error(), Some(Errno::NOENT.raw_os_error()));
        Ok(())
    }
}

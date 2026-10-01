use std::fs::File;
use std::io;
use std::os::fd::OwnedFd;
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
pub(super) struct Directory(OwnedFd);

impl Directory {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        if path.as_os_str().is_empty() || path.components().any(|c| c == Component::ParentDir) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid runtime directory path",
            ));
        }
        let path = std::path::absolute(path)?;
        let mut dir = Self(open("/", DIRECTORY_FLAGS, Mode::empty())?);
        for component in path.components() {
            if let Component::Normal(name) = component {
                dir = dir.child(Path::new(name))?;
            }
        }
        Ok(dir)
    }

    pub(super) fn child(&self, name: &Path) -> io::Result<Self> {
        let child = Self(openat(&self.0, name, DIRECTORY_FLAGS, Mode::empty())?);
        let stat = fstat(&child.0)?;
        let trusted_owner = stat.st_uid == geteuid().as_raw() || stat.st_uid == 0;
        let sticky = stat.st_mode & 0o1000 != 0;
        if !trusted_owner || (stat.st_mode & 0o022 != 0 && !sticky) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "runtime ancestor is writable by another user",
            ));
        }
        Ok(child)
    }

    pub(super) fn create_child(&self, name: &Path) -> io::Result<Self> {
        mkdirat(&self.0, name, Mode::RWXU)?;
        self.child(name)
    }

    pub(super) fn ensure_child(&self, name: &Path) -> io::Result<Self> {
        match mkdirat(&self.0, name, Mode::RWXU) {
            Ok(()) | Err(Errno::EXIST) => self.child(name),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn make_private(&self) -> io::Result<()> {
        self.require_owner()?;
        fchmod(&self.0, Mode::RWXU)?;
        Ok(())
    }

    pub(super) fn require_private(&self) -> io::Result<()> {
        let stat = self.require_owner()?;
        if stat.st_mode & 0o7777 != 0o700 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "runtime directory must have mode 0700",
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

    pub(super) fn create_file(&self, name: &Path, mode: Mode) -> io::Result<File> {
        let fd = openat(
            &self.0,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            mode,
        )?;
        Ok(File::from(fd))
    }

    pub(super) fn remove_file(&self, name: &Path) -> io::Result<()> {
        match unlinkat(&self.0, name, AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn remove_child(&self, name: &Path, child: &Self) -> io::Result<()> {
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
        unlinkat(&self.0, name, AtFlags::REMOVEDIR)?;
        Ok(())
    }
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
                clear(&child)?;
                unlinkat(dir, name, AtFlags::REMOVEDIR)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

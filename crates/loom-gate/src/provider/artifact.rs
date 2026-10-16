//! Content identity shared only for a locally kernel-protected running Linux executable.

use std::fs::File;
use std::path::Path;

use super::{Digest, Error};

fn digest(file: &mut File, path: &Path) -> Result<Digest, Error> {
    #[cfg(test)]
    HASHES.with(|count| count.set(count.get() + 1));
    let mut hash = blake3::Hasher::new();
    hash.update_reader(file).map_err(|source| Error::Artifact {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Digest::from_hash(hash.finalize()))
}

fn open(path: &Path) -> Result<File, Error> {
    File::open(path).map_err(|source| Error::Artifact {
        path: path.to_path_buf(),
        source,
    })
}

pub(super) fn read(path: &Path) -> Result<Digest, Error> {
    let mut file = open(path)?;
    #[cfg(target_os = "linux")]
    if let Some(identity) = linux::running(&file)? {
        return Ok(identity);
    }
    digest(&mut file, path)
}

#[cfg(target_os = "linux")]
mod linux {
    use std::os::unix::fs::MetadataExt as _;
    use std::sync::Mutex;

    use super::{Digest, Error, File, Path, digest, open};

    struct Image {
        file: File,
        identity: Digest,
    }

    static IMAGE: Mutex<Option<Image>> = Mutex::new(None);

    fn same(left: &File, right: &File) -> std::io::Result<bool> {
        let left = left.metadata()?;
        let right = right.metadata()?;
        Ok(left.dev() == right.dev() && left.ino() == right.ino())
    }

    fn local(kind: rustix::fs::FsWord) -> bool {
        const EXT: u64 = 0xef53;
        const XFS: u64 = 0x5846_5342;
        const BTRFS: u64 = 0x9123_683e;
        const TMPFS: u64 = 0x0102_1994;
        const OVERLAY: u64 = 0x794c_7630;
        // Remote/FUSE servers can change bytes despite local executable write denial.
        // Overlay lower layers must obey the filesystem's immutable-layer contract.
        matches!(u64::try_from(kind), Ok(EXT | XFS | BTRFS | TMPFS | OVERLAY))
    }

    pub(super) fn cacheable(file: &File) -> Result<bool, Error> {
        let filesystem =
            rustix::fs::fstatfs(file).map_err(|source| Error::Executable(source.into()))?;
        Ok(local(filesystem.f_type))
    }

    pub(super) fn running(file: &File) -> Result<Option<Digest>, Error> {
        let path = Path::new("/proc/self/exe");
        let current = open(path)?;
        if !same(file, &current).map_err(Error::Executable)? || !cacheable(&current)? {
            return Ok(None);
        }
        let mut image = IMAGE.lock().map_err(|_| {
            Error::Executable(std::io::Error::other(
                "running executable identity lock poisoned",
            ))
        })?;
        if let Some(image) = image.as_ref() {
            return same(&image.file, &current)
                .map(|matches| matches.then(|| image.identity.clone()))
                .map_err(Error::Executable);
        }
        // Linux denies writes to this inode for the lifetime of the executing process.
        let mut current = current;
        let identity = digest(&mut current, path)?;
        *image = Some(Image {
            file: current,
            identity: identity.clone(),
        });
        drop(image);
        Ok(Some(identity))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn remote_userspace_and_unknown_filesystems_do_not_share_identity() {
            for kind in [0x6969, 0x6573_5546, 0xff53_4d42, 0xfe53_4d42, 0] {
                assert!(!local(kind));
            }
            for kind in [0xef53, 0x5846_5342, 0x9123_683e, 0x0102_1994, 0x794c_7630] {
                assert!(local(kind));
            }
        }
    }
}

#[cfg(test)]
thread_local! {
    static HASHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutable_artifacts_are_rehashed_even_with_identical_size_and_modification_time() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"before").unwrap();
        let time = file.as_file().metadata().unwrap().modified().unwrap();
        let before = read(file.path()).unwrap();
        std::fs::write(file.path(), b"after!").unwrap();
        file.as_file()
            .set_times(std::fs::FileTimes::new().set_modified(time))
            .unwrap();
        assert_ne!(before, read(file.path()).unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn running_image_shares_identity_only_when_locally_kernel_protected() {
        let path = std::env::current_exe().unwrap();
        let shared = linux::cacheable(&File::open(&path).unwrap()).unwrap();
        let boundaries = if shared { 65 * 5 } else { 2 };
        let expected = Digest::of(&std::fs::read(&path).unwrap());
        HASHES.with(|count| count.set(0));
        for _ in 0..boundaries {
            assert_eq!(read(&path).unwrap(), expected);
        }
        HASHES.with(|count| {
            if shared {
                assert!(count.get() <= 1, "{} full artifact reads", count.get());
            } else {
                assert_eq!(count.get(), boundaries);
            }
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn writable_executable_fixture_retains_kernel_and_path_replacement_guards() {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().unwrap();
        let copy = directory.path().join("checker");
        std::fs::copy(std::env::current_exe().unwrap(), &copy).unwrap();
        std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o700)).unwrap();
        let output = std::process::Command::new(&copy)
            .args([
                "--exact",
                "provider::artifact::tests::running_image_child",
                "--nocapture",
            ])
            .env("LOOM_ARTIFACT_TEST_CHILD", directory.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn running_image_child() {
        let Some(directory) = std::env::var_os("LOOM_ARTIFACT_TEST_CHILD") else {
            return;
        };
        let path = std::env::current_exe().unwrap();
        assert_eq!(path.parent(), Some(Path::new(&directory)));
        assert_eq!(path.file_name(), Some(std::ffi::OsStr::new("checker")));
        let expected = read(&path).unwrap();
        let error = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap_err();
        assert_eq!(
            error.raw_os_error(),
            Some(26),
            "writable fixture must be kernel write-denied"
        );
        let moved = path.with_extension("running");
        std::fs::rename(&path, &moved).unwrap();
        std::fs::write(&path, b"replacement is not the running checker").unwrap();
        assert_ne!(read(&path).unwrap(), expected);
        assert_eq!(read(&moved).unwrap(), expected);
    }
}

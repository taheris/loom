use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use rustix::fs::{FlockOperation, Mode, OFlags, flock, open};

use super::{Error, Report, catalog::Packages, transfer};

pub(super) struct Profile {
    pub(super) path: PathBuf,
    _lock: File,
}

pub(super) fn locked_profiles(source: &Path) -> Result<Vec<Profile>, Error> {
    let mut profiles = Vec::new();
    for directory in directories(source)? {
        if directory.join(".cargo-lock").try_exists()? {
            profiles.push(lock(&directory)?);
        } else {
            if directory.join("deps").try_exists()?
                || directory.join(".fingerprint").try_exists()?
            {
                return Err(Error::Storage(directory));
            }
            for nested in directories(&directory)? {
                if nested.join(".cargo-lock").try_exists()? {
                    profiles.push(lock(&nested)?);
                }
            }
        }
    }
    if profiles.is_empty() {
        return Err(Error::Storage(source.to_path_buf()));
    }
    Ok(profiles)
}

fn directories(parent: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut result = Vec::new();
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(Error::Storage(entry.path()));
        }
        if kind.is_dir() {
            result.push(entry.path());
        }
    }
    Ok(result)
}

fn lock(profile: &Path) -> Result<Profile, Error> {
    let path = profile.join(".cargo-lock");
    let file = File::from(
        open(
            &path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    if !file.metadata()?.is_file() {
        return Err(Error::Storage(path));
    }
    flock(&file, FlockOperation::NonBlockingLockShared).map_err(std::io::Error::from)?;
    Ok(Profile {
        path: profile.to_path_buf(),
        _lock: file,
    })
}

pub(super) fn copy_profile(
    source: &Path,
    destination: &Path,
    packages: &Packages,
    report: &mut Report,
) -> Result<(), Error> {
    let fingerprints = source.join(".fingerprint");
    let deps = source.join("deps");
    if !fs::symlink_metadata(&fingerprints)?.is_dir() || !fs::symlink_metadata(&deps)?.is_dir() {
        return Err(Error::Storage(source.to_path_buf()));
    }
    fs::create_dir_all(destination.join("deps"))?;
    let mut artifacts = BTreeSet::new();
    for unit in fs::read_dir(&fingerprints)? {
        let unit = unit?;
        let name = unit.file_name();
        let Some(hash) = name.to_str().and_then(|name| packages.unit(name)) else {
            continue;
        };
        if !unit.file_type()?.is_dir() {
            return Err(Error::Storage(unit.path()));
        }
        for entry in fs::read_dir(unit.path())? {
            let entry = entry?;
            let name = entry.file_name();
            if let Some(library) = name
                .to_str()
                .and_then(|name| name.strip_prefix("lib-"))
                .and_then(|name| name.strip_suffix(".json"))
            {
                artifacts.insert(format!("{library}-{hash}"));
            }
        }
        transfer::tree(
            &unit.path(),
            &destination.join(".fingerprint").join(unit.file_name()),
            report,
        )?;
    }
    for entry in fs::read_dir(&deps)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some((stem, extension)) = name.rsplit_once('.') else {
            continue;
        };
        if !artifacts.contains(stem)
            && !stem
                .strip_prefix("lib")
                .is_some_and(|name| artifacts.contains(name))
        {
            continue;
        }
        transfer::file(
            &entry.path(),
            &destination.join("deps").join(name),
            name.starts_with("lib") && matches!(extension, "rlib" | "rmeta"),
            report,
        )?;
    }
    let build = source.join("build");
    if build.try_exists()? {
        if !fs::symlink_metadata(&build)?.is_dir() {
            return Err(Error::Storage(build));
        }
        for unit in fs::read_dir(&build)? {
            let unit = unit?;
            if unit
                .file_name()
                .to_str()
                .is_some_and(|name| packages.unit(name).is_some())
            {
                transfer::tree(
                    &unit.path(),
                    &destination.join("build").join(unit.file_name()),
                    report,
                )?;
            }
        }
    }
    File::create_new(destination.join(".cargo-lock"))?;
    Ok(())
}

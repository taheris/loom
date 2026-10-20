//! Optional worker-only dependency seeding; cached bytes never establish verification authority.

mod catalog;
mod context;
mod layout;
mod transfer;

use std::fs;
use std::path::{Path, PathBuf};

use displaydoc::Display;
use rustix::fs::{CWD, RenameFlags, renameat_with};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Permission to reuse dependency artifacts between worker checkouts.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reuse {
    Off,
    #[default]
    CopyOnWrite,
    /// Prefer independent copy-on-write clones; permit writable library hard links as fallback.
    AllowHardlinks,
}

/// Failure to safely initialize a target; callers may continue with ordinary Cargo.
#[derive(Debug, Display, Error)]
pub enum Error {
    /// target seeding filesystem operation failed
    Io(#[from] std::io::Error),
    /// target seeding input is invalid TOML
    Toml(#[from] toml::de::Error),
    /// target seeding context could not be encoded
    Encode(#[from] toml::ser::Error),
    /// copy-on-write is unavailable and hard links are not permitted: {0}
    CloneUnavailable(PathBuf),
    /// target seeding input is not UTF-8
    Utf8(#[from] std::str::Utf8Error),
    /// target seeding refused unsafe or unsupported storage: {0}
    Storage(PathBuf),
}

/// A target is either preserved, initialized cold, or seeded from a worker sibling.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Disabled,
    Preserved,
    Private,
    Cold,
    Seeded(Report),
}

/// File-level accounting, not a claim of reclaimed physical blocks.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub cloned_bytes: u64,
    pub linked_bytes: u64,
    pub copied_bytes: u64,
}

/// Initialize an absent target using compatible direct worker siblings only.
///
/// # Errors
/// Returns unsupported storage, malformed input or filesystem failures without replacing a target.
pub fn prepare(
    beads: &Path,
    workspace: &Path,
    image: &Path,
    reuse: Reuse,
) -> Result<Outcome, Error> {
    prepare_with(beads, workspace, image, reuse, &transfer::Native)
}

fn prepare_with(
    beads: &Path,
    workspace: &Path,
    image: &Path,
    reuse: Reuse,
    cloner: &impl transfer::Cloner,
) -> Result<Outcome, Error> {
    if reuse == Reuse::Off {
        return Ok(Outcome::Disabled);
    }
    let beads = context::directory(beads)?;
    let workspace = context::directory(workspace)?;
    if workspace.parent() != Some(beads.as_path()) {
        return Err(Error::Storage(workspace));
    }
    let target = workspace.join("target");
    if fs::symlink_metadata(&target).is_ok() {
        return Ok(Outcome::Preserved);
    }
    if !context::sharing_allowed(&workspace)? {
        return Ok(Outcome::Private);
    }
    let context = context::Stamp {
        fingerprint: context::fingerprint(&workspace, image)?,
        reuse,
    };
    let packages = catalog::Packages::read(&workspace.join("Cargo.lock"))?;
    let mut siblings = fs::read_dir(&beads)?.collect::<Result<Vec<_>, _>>()?;
    siblings.sort_by_key(std::fs::DirEntry::file_name);
    for sibling in siblings {
        if sibling.path() == workspace || !sibling.file_type()?.is_dir() {
            continue;
        }
        match seed(
            &sibling.path(),
            &workspace,
            image,
            &context,
            &packages,
            cloner,
        ) {
            Ok(Some(report)) => return Ok(Outcome::Seeded(report)),
            Ok(None) => {}
            Err(error) => tracing::warn!(donor = ?sibling.path(), ?error, "target donor skipped"),
        }
        if fs::symlink_metadata(&target).is_ok() {
            return Ok(Outcome::Preserved);
        }
    }
    let stage = staging(&workspace)?;
    context.write(stage.path())?;
    if publish(stage.path(), &target)? {
        Ok(Outcome::Cold)
    } else {
        Ok(Outcome::Preserved)
    }
}

fn seed(
    donor: &Path,
    workspace: &Path,
    image: &Path,
    expected: &context::Stamp,
    packages: &catalog::Packages,
    cloner: &impl transfer::Cloner,
) -> Result<Option<Report>, Error> {
    let donor = context::directory(donor)?;
    if !context::sharing_allowed(&donor)?
        || context::fingerprint(&donor, image)? != expected.fingerprint
    {
        return Ok(None);
    }
    let source = context::directory(&donor.join("target"))?;
    let recorded = context::Stamp::read(&source)?;
    if recorded.reuse == Reuse::Off || recorded.fingerprint != expected.fingerprint {
        return Ok(None);
    }
    let reuse = if recorded.reuse == Reuse::AllowHardlinks {
        expected.reuse
    } else {
        Reuse::CopyOnWrite
    };
    let profiles = layout::locked_profiles(&source)?;
    let stage = staging(workspace)?;
    let mut report = Report::default();
    for profile in &profiles {
        let relative = profile
            .path
            .strip_prefix(&source)
            .map_err(|_| Error::Storage(profile.path.clone()))?;
        layout::copy_profile(
            &profile.path,
            &stage.path().join(relative),
            packages,
            reuse,
            cloner,
            &mut report,
        )?;
    }
    if context::fingerprint(&donor, image)? != expected.fingerprint {
        return Err(Error::Storage(donor));
    }
    expected.write(stage.path())?;
    if publish(stage.path(), &workspace.join("target"))? {
        Ok(Some(report))
    } else {
        Ok(None)
    }
}

fn staging(workspace: &Path) -> Result<tempfile::TempDir, Error> {
    Ok(tempfile::Builder::new()
        .prefix(".loom-shared-")
        .tempdir_in(workspace)?)
}

fn publish(source: &Path, destination: &Path) -> Result<bool, Error> {
    match renameat_with(CWD, source, CWD, destination, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(true),
        Err(rustix::io::Errno::EXIST) => Ok(false),
        Err(error) => Err(Error::Io(error.into())),
    }
}

#[cfg(test)]
mod tests;

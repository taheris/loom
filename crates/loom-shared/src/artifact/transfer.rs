use std::fs;
use std::io;
use std::path::Path;

use filetime::FileTime;

use super::{Error, Report, Reuse};

pub(super) trait Cloner {
    fn clone_file(&self, source: &Path, destination: &Path) -> io::Result<()>;
}

pub(super) struct Native;

impl Cloner for Native {
    fn clone_file(&self, source: &Path, destination: &Path) -> io::Result<()> {
        reflink_copy::reflink(source, destination).map_err(|error| {
            if matches!(error.raw_os_error(), Some(18 | 22 | 25 | 38 | 45 | 95)) {
                io::Error::new(io::ErrorKind::Unsupported, error)
            } else {
                error
            }
        })
    }
}

pub(super) fn library(
    source: &Path,
    destination: &Path,
    reuse: Reuse,
    cloner: &impl Cloner,
    report: &mut Report,
) -> Result<(), Error> {
    let metadata = regular_file(source)?;
    match cloner.clone_file(source, destination) {
        Ok(()) => {
            report.cloned_bytes += metadata.len();
            filetime::set_file_mtime(
                destination,
                FileTime::from_last_modification_time(&metadata),
            )?;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::Unsupported => {
            if reuse != Reuse::AllowHardlinks {
                return Err(Error::CloneUnavailable(source.to_path_buf()));
            }
            fs::hard_link(source, destination)?;
            report.linked_bytes += metadata.len();
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn regular_file(source: &Path) -> Result<fs::Metadata, Error> {
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.is_file() {
        return Err(Error::Storage(source.to_path_buf()));
    }
    Ok(metadata)
}

pub(super) fn private(source: &Path, destination: &Path, report: &mut Report) -> Result<(), Error> {
    let metadata = regular_file(source)?;
    fs::copy(source, destination)?;
    filetime::set_file_mtime(
        destination,
        FileTime::from_last_modification_time(&metadata),
    )?;
    report.copied_bytes += metadata.len();
    Ok(())
}

pub(super) fn tree(source: &Path, destination: &Path, report: &mut Report) -> Result<(), Error> {
    if !fs::symlink_metadata(source)?.is_dir() {
        return Err(Error::Storage(source.to_path_buf()));
    }
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let to = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            tree(&entry.path(), &to, report)?;
        } else {
            private(&entry.path(), &to, report)?;
        }
    }
    filetime::set_file_mtime(
        destination,
        FileTime::from_last_modification_time(&fs::metadata(source)?),
    )?;
    Ok(())
}

use std::fs;
use std::path::Path;

use filetime::FileTime;

use super::{Error, Report};

pub(super) fn file(
    source: &Path,
    destination: &Path,
    library: bool,
    report: &mut Report,
) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.is_file() {
        return Err(Error::Storage(source.to_path_buf()));
    }
    if library {
        match reflink_copy::reflink(source, destination) {
            Ok(()) => {
                report.cloned_bytes += metadata.len();
                filetime::set_file_mtime(
                    destination,
                    FileTime::from_last_modification_time(&metadata),
                )?;
                return Ok(());
            }
            Err(error) if matches!(error.raw_os_error(), Some(18 | 22 | 25 | 38 | 45 | 95)) => {}
            Err(error) => return Err(error.into()),
        }
        fs::hard_link(source, destination)?;
        report.linked_bytes += metadata.len();
    } else {
        fs::copy(source, destination)?;
        filetime::set_file_mtime(
            destination,
            FileTime::from_last_modification_time(&metadata),
        )?;
        report.copied_bytes += metadata.len();
    }
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
            file(&entry.path(), &to, false, report)?;
        }
    }
    filetime::set_file_mtime(
        destination,
        FileTime::from_last_modification_time(&fs::metadata(source)?),
    )?;
    Ok(())
}

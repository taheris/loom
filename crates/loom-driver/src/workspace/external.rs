//! Optional host-side membership admission, independent of Loom execution authority.

use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rustix::fs::{AtFlags, FileType, Mode, OFlags, fstat, openat, renameat, statat};
use rustix::process::geteuid;
use serde::{Deserialize, Serialize};

use super::{Checkout, Error, Identity, disposable};
use crate::scratch::directory::Directory;

#[cfg(test)]
mod tests;

const SELECTION: &str = "loom-workspace-associator.json";
const STAGING: &str = "loom-workspace-associator.staging";
const LIMIT: u64 = 16 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    version: u32,
    #[serde(deserialize_with = "Option::<PathBuf>::deserialize")]
    program: Option<PathBuf>,
}

/// An explicitly selected host adapter. Membership never grants native authority.
#[derive(Debug)]
pub struct Membership {
    operator: Checkout,
    selection: Selection,
    file: Identity,
}

impl Membership {
    /// Load the controlling operator's selection, never a worker's selection.
    ///
    /// # Errors
    /// Unsafe, malformed, unreadable or interrupted selection fails closed.
    pub fn load(checkout: &Path) -> Result<Option<Self>, Error> {
        let state = crate::state::path(checkout);
        let operator = state
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| invalid("missing operator"))?;
        let git_path = operator.join(".git");
        match std::fs::symlink_metadata(&git_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
            // Do not follow a .git pointer or change standalone GitClient behavior for linked
            // worktrees. Explicit configuration still requires a direct native Git directory.
            Ok(metadata) if metadata.is_file() => return Ok(None),
            Ok(_) => {}
        }
        // Observe genuine absence before imposing adapter-specific storage checks. In particular,
        // standalone GitClient use must not acquire new directory-permission requirements.
        let mut selected = false;
        for name in [SELECTION, STAGING] {
            match std::fs::symlink_metadata(git_path.join(name)) {
                Ok(_) => selected = true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        if !selected {
            return Ok(None);
        }
        if disposable(operator) {
            return Err(invalid("execution checkouts cannot select a host adapter"));
        }
        let git = Directory::open(&git_path)?;
        let Some((selection, file)) = read(&git)? else {
            return Ok(None);
        };
        let operator = Checkout::read(operator)?;
        if selection.program.is_none() {
            return Ok(None);
        }
        if std::env::var_os(crate::agent::LOOM_INSIDE_ENV).is_some() {
            return Err(invalid(
                "host membership adapters cannot run inside workers",
            ));
        }
        Ok(Some(Self {
            operator,
            selection,
            file,
        }))
    }

    /// Explicitly select, replace or disable an adapter under the workspace initialization lock.
    ///
    /// A failed admission does not undo this durable selection. Interrupted staging is retained
    /// and must be inspected before another selection can be published.
    ///
    /// # Errors
    /// Rejects worker invocations, unsafe storage, relative programs, staging residue and I/O failures.
    pub fn configure(
        operator: &Path,
        program: Option<&Path>,
        guard: &crate::lock::LockGuard,
    ) -> Result<(), Error> {
        if std::env::var_os(crate::agent::LOOM_INSIDE_ENV).is_some() {
            return Err(invalid(
                "host membership selection cannot be changed inside workers",
            ));
        }
        let operator = Checkout::read(operator)?;
        if crate::state::path(&operator.path) != operator.path.join(".loom/state.db") {
            return Err(invalid("configure membership from the operator checkout"));
        }
        if guard.workspace() != Some(operator.path.as_path()) {
            return Err(invalid(
                "membership selection requires the controlling workspace initialization lock",
            ));
        }
        let git = Directory::open(&operator.path.join(".git"))?;
        reject_staging(&git)?;
        // Explicit replacement may repair malformed contents, never an unsafe filesystem object.
        let _previous = read_file(&git)?;
        let selection = Selection {
            version: 1,
            program: program.map(Path::to_path_buf),
        };
        selection.validate()?;
        let fd = openat(
            git.as_fd(),
            STAGING,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?;
        let mut staging = std::fs::File::from(fd);
        staging.write_all(&serde_json::to_vec(&selection).map_err(std::io::Error::other)?)?;
        staging.sync_all()?;
        git.sync()?;
        if Checkout::read(&operator.path)? != operator {
            return Err(invalid(
                "operator changed during selection; staging retained",
            ));
        }
        renameat(git.as_fd(), STAGING, git.as_fd(), SELECTION)?;
        git.sync()?;
        Ok(())
    }

    /// Confirm the operator, then associate-or-confirm the target before preparation.
    ///
    /// The adapter must validate its own participants, refuse conflicting membership and make
    /// retries idempotent. A timeout can follow a committed external association.
    ///
    /// # Errors
    /// Adapter rejection, uncertain cleanup, changed participants or changed selection stops use.
    pub async fn admit(&self, target: &Path) -> Result<(), Error> {
        self.admit_with_clock(target, &crate::clock::SystemClock::new())
            .await
    }

    async fn admit_with_clock(
        &self,
        target: &Path,
        clock: &dyn crate::clock::Clock,
    ) -> Result<(), Error> {
        self.recheck()?;
        let target = Checkout::read_participant(target)?;
        let program = self
            .selection
            .program
            .as_deref()
            .ok_or_else(|| invalid("adapter is disabled"))?;
        let executable = executable(program)?;
        let mut command = tokio::process::Command::new(program);
        command
            .current_dir(&self.operator.path)
            .arg("--operator")
            .arg(&self.operator.path)
            .arg("--checkout")
            .arg(&target.path);
        let output = crate::process::output_limited(&mut command, clock, TIMEOUT, LIMIT).await?;
        self.recheck()?;
        if Checkout::read_participant(&target.path)? != target
            || executable != self::executable(program)?
        {
            return Err(invalid(
                "participant or adapter changed during membership admission",
            ));
        }
        if !output.status.success() {
            return Err(Error::ExternalRejected {
                status: output.status,
                detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok(())
    }

    /// Admit from synchronous host initialization.
    ///
    /// # Errors
    /// Returns native, selection, process or runtime initialization failures.
    pub fn admit_blocking(&self, target: &Path) -> Result<(), Error> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(invalid(
                "use asynchronous membership admission inside a runtime",
            ));
        }
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(self.admit(target))
    }

    fn recheck(&self) -> Result<(), Error> {
        if Checkout::read(&self.operator.path)? != self.operator {
            return Err(invalid("operator changed during membership admission"));
        }
        let git = Directory::open(&self.operator.path.join(".git"))?;
        if read(&git)?
            != Some((
                Selection {
                    version: self.selection.version,
                    program: self.selection.program.clone(),
                },
                self.file.clone(),
            ))
        {
            return Err(invalid("membership selection changed during admission"));
        }
        Ok(())
    }
}

/// Admit from synchronous host preparation; no runtime is needed in standalone mode.
///
/// # Errors
/// Returns selection, adapter, native-recheck or runtime initialization failures.
pub fn admit_blocking(checkout: &Path, target: &Path) -> Result<(), Error> {
    if let Some(membership) = Membership::load(checkout)? {
        membership.admit_blocking(target)?;
    }
    Ok(())
}

impl Selection {
    fn validate(&self) -> Result<(), Error> {
        if self.version != 1
            || self
                .program
                .as_ref()
                .is_some_and(|path| !path.is_absolute())
        {
            return Err(invalid(
                "unsupported membership selection or non-absolute program",
            ));
        }
        Ok(())
    }
}

fn read(git: &Directory) -> Result<Option<(Selection, Identity)>, Error> {
    reject_staging(git)?;
    let Some((bytes, identity)) = read_file(git)? else {
        return Ok(None);
    };
    let selection: Selection =
        serde_json::from_slice(&bytes).map_err(|_| invalid("malformed membership selection"))?;
    selection.validate()?;
    Ok(Some((selection, identity)))
}

fn reject_staging(git: &Directory) -> Result<(), Error> {
    match statat(git.as_fd(), STAGING, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Err(invalid(
            "interrupted membership selection staging retained; inspect before repair",
        )),
        Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(error) => Err(std::io::Error::from(error).into()),
    }
}

fn read_file(git: &Directory) -> Result<Option<(Vec<u8>, Identity)>, Error> {
    let fd = match openat(
        git.as_fd(),
        SELECTION,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(std::io::Error::from(error).into()),
    };
    let stat = fstat(&fd)?;
    if stat.st_uid != geteuid().as_raw()
        || stat.st_mode & 0o7777 != 0o600
        || stat.st_nlink != 1
        || FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
    {
        return Err(invalid(
            "membership selection must be an owner-private, singly linked regular file",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::from(fd)
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(invalid("membership selection exceeds size limit"));
    }
    Ok(Some((bytes, Identity::from_stat(&stat))))
}

fn executable(path: &Path) -> Result<Identity, Error> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("adapter has no parent directory"))?;
    let directory = Directory::open(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid("adapter has no file name"))?;
    let stat = statat(directory.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || (stat.st_uid != geteuid().as_raw() && stat.st_uid != 0)
        || stat.st_mode & 0o022 != 0
        || stat.st_mode & 0o111 == 0
    {
        return Err(invalid(
            "adapter must be an executable regular file owned by the user or root, not writable by others",
        ));
    }
    Ok(Identity::from_stat(&stat))
}

impl From<rustix::io::Errno> for Error {
    fn from(error: rustix::io::Errno) -> Self {
        Self::Io(error.into())
    }
}

const fn invalid(reason: &'static str) -> Error {
    Error::Association { reason }
}

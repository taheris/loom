//! Durable host-only dispatch and protection, independent of transient cache replay.

#[cfg(test)]
mod tests;

use super::{
    Directory, Error, Identity, Record,
    retention::{self, Hold},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

fn entry_name(group: &Path) -> Result<String, Error> {
    let name = group
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| std::io::Error::other("native cgroup has no name"))?;
    Ok(format!("native-entry-{name}"))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Protection {
    version: u8,
    binding: [u8; 32],
    target: PathBuf,
    holds: BTreeSet<Hold>,
    authority: Authority,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
enum Authority {
    Prepared,
    Unknown,
    Runtime(Runtime),
    Retired(Runtime),
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
enum Runtime {
    Wrix {
        issuer: PathBuf,
        issuer_identity: Identity,
        workspace: PathBuf,
        state_home: PathBuf,
        ticket: String,
    },
    Native {
        group: PathBuf,
        identity: Identity,
        boot: String,
        entry_identity: Identity,
        execution_identity: Identity,
    },
}

fn binding(record: &Record) -> Result<[u8; 32], Error> {
    let bytes = serde_json::to_vec(&(
        record.version,
        &record.bead,
        &record.profile,
        &record.source,
        &record.source_identity,
        &record.target,
        &record.namespace_identity,
        &record.lease_identity,
        &record.target_identity,
        &record.context,
    ))
    .map_err(std::io::Error::other)?;
    Ok(Sha256::digest(bytes).into())
}

fn journal(target: &Path) -> Result<(Directory, String), Error> {
    let namespace = target.parent().ok_or(Error::Association {
        reason: "Cargo target has no namespace",
    })?;
    let parent = Directory::open(namespace.parent().ok_or(Error::Association {
        reason: "Cargo namespace has no parent",
    })?)?;
    parent.require_private()?;
    let name = namespace
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(Error::Association {
            reason: "Cargo namespace has no name",
        })?;
    Ok((parent, format!(".{name}.protection")))
}

pub(super) fn journal_exists(record: &Record) -> Result<bool, Error> {
    let (parent, name) = journal(&record.target)?;
    match parent.read_private_file(Path::new(&name)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn decode(file: std::fs::File, record: &Record) -> Result<Protection, Error> {
    let mut bytes = Vec::new();
    file.take(65_537).read_to_end(&mut bytes)?;
    if bytes.len() > 65_536 {
        return Err(std::io::Error::other("Cargo protection exceeds 64 KiB").into());
    }
    let protection: Protection = serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
    if protection.version != 1
        || protection.target != record.target
        || protection.binding != binding(record)?
    {
        return Err(Error::Association {
            reason: "Cargo durable protection identity conflicts with ownership; retained",
        });
    }
    Ok(protection)
}

fn read(directory: &Directory, record: &Record) -> Result<Protection, Error> {
    let (parent, name) = journal(&record.target)?;
    let file = match parent.read_private_file(Path::new(&name)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            directory.read_private_file(Path::new(retention::PROJECTION))?
        }
        Err(error) => return Err(error.into()),
    };
    let protection = decode(file, record)?;
    if let Authority::Runtime(Runtime::Native {
        execution_identity, ..
    }) = &protection.authority
    {
        let file = directory.read_private_file(Path::new("execution"))?;
        if Identity::from_stat(&rustix::fs::fstat(&file).map_err(std::io::Error::from)?)
            != *execution_identity
        {
            return Err(Error::Association {
                reason: "native execution lock identity changed; retained",
            });
        }
    }
    Ok(protection)
}

fn write(directory: &Directory, protection: &Protection, initial: bool) -> Result<(), Error> {
    let (parent, journal_name) = journal(&protection.target)?;
    let journaled = match parent.read_private_file(Path::new(&journal_name)) {
        Ok(_) => !initial,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    if journaled {
        return publish(&parent, &journal_name, protection, false);
    }
    let name = if initial {
        retention::PROJECTION.to_owned()
    } else {
        format!("protection-{}.tmp", nonce()?)
    };
    let mut file = directory.create_file(
        Path::new(&name),
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )?;
    serde_json::to_writer(&mut file, protection).map_err(std::io::Error::other)?;
    file.sync_all()?;
    if !initial {
        rustix::fs::renameat(directory, &name, directory, retention::PROJECTION)
            .map_err(std::io::Error::from)?;
    }
    directory.sync()?;
    Ok(())
}

fn publish(
    parent: &Directory,
    name: &str,
    protection: &Protection,
    initial: bool,
) -> Result<(), Error> {
    let temporary = format!("protection-{}.tmp", nonce()?);
    let mut file = parent.create_file(
        Path::new(&temporary),
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )?;
    serde_json::to_writer(&mut file, protection).map_err(std::io::Error::other)?;
    file.sync_all()?;
    let flags = if initial {
        rustix::fs::RenameFlags::NOREPLACE
    } else {
        rustix::fs::RenameFlags::empty()
    };
    rustix::fs::renameat_with(parent, &temporary, parent, name, flags)
        .map_err(std::io::Error::from)?;
    parent.sync()?;
    Ok(())
}

/// Keep the terminal proof outside the directory that recursive unlink can partially remove.
pub(super) fn preserve(directory: &Directory, record: &Record) -> Result<(), Error> {
    let protection = read(directory, record)?;
    if !protection.holds.is_empty()
        || protection.holds != record.holds
        || !matches!(
            protection.authority,
            Authority::Prepared | Authority::Retired(_)
        )
    {
        return Err(Error::Association {
            reason: "Cargo quarantine lacks terminal durable protection",
        });
    }
    let (parent, name) = journal(&record.target)?;
    match parent.read_private_file(Path::new(&name)) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            publish(&parent, &name, &protection, true)
        }
        Err(error) => Err(error.into()),
    }
}

pub(super) fn removed(record: &Record, forget: bool) -> Result<(), Error> {
    let (parent, name) = journal(&record.target)?;
    let protection = decode(parent.read_private_file(Path::new(&name))?, record)?;
    if !protection.holds.is_empty()
        || protection.holds != record.holds
        || !matches!(
            protection.authority,
            Authority::Prepared | Authority::Retired(_)
        )
    {
        return Err(Error::Association {
            reason: "Cargo removal lacks terminal durable protection",
        });
    }
    if forget {
        rustix::fs::unlinkat(&parent, name.as_str(), rustix::fs::AtFlags::empty())
            .map_err(std::io::Error::from)?;
        parent.sync()?;
    }
    Ok(())
}

pub(super) fn initialize(directory: &Directory, record: &Record) -> Result<(), Error> {
    write(
        directory,
        &Protection {
            version: 1,
            binding: binding(record)?,
            target: record.target.clone(),
            holds: record.holds.clone(),
            authority: Authority::Prepared,
        },
        true,
    )
}

pub(super) fn legacy_holds(
    directory: &Directory,
    record: &mut Record,
    hold: Hold,
    acquire: bool,
) -> Result<(), Error> {
    if !journal_exists(record)? {
        let structured = match directory.read_private_file(Path::new(retention::PROJECTION)) {
            Ok(file) => file.metadata()?.len() > 0,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        if !structured {
            let (parent, name) = journal(&record.target)?;
            publish(
                &parent,
                &name,
                &Protection {
                    version: 1,
                    binding: binding(record)?,
                    target: record.target.clone(),
                    holds: record.holds.clone(),
                    authority: Authority::Unknown,
                },
                true,
            )?;
        }
    }
    holds(directory, record, hold, acquire)
}

pub(super) fn holds(
    directory: &Directory,
    record: &mut Record,
    hold: Hold,
    acquire: bool,
) -> Result<(), Error> {
    let mut protection = read(directory, record)?;
    protection.holds.extend(record.holds.iter().copied());
    if acquire {
        protection.holds.insert(hold);
    } else {
        protection.holds.remove(&hold);
    }
    record.holds.clone_from(&protection.holds);
    write(directory, &protection, false)
}

pub(super) fn dispatch_unknown(directory: &Directory, record: &Record) -> Result<(), Error> {
    let mut protection = read(directory, record)?;
    if !matches!(protection.authority, Authority::Prepared) || protection.holds != record.holds {
        return Err(Error::Association {
            reason: "Cargo durable dispatch protection changed; retained",
        });
    }
    protection.authority = Authority::Unknown;
    write(directory, &protection, false)
}

pub(super) fn dispatch_worker(
    directory: &Directory,
    record: &Record,
    spawn: &mut crate::agent::SpawnConfig,
) -> Result<(), Error> {
    dispatch_unknown(directory, record)?;
    let Some(manifest_launcher) = &spawn.wrix_launcher else {
        return Ok(());
    };
    let Some(profile) = &spawn.profile_config else {
        return Ok(());
    };
    let profile: serde_json::Value =
        serde_json::from_slice(&std::fs::read(profile)?).map_err(std::io::Error::other)?;
    if profile["features"]["execution_tickets"] != true {
        return Ok(());
    }
    let issuer = crate::wrix::resolve_launcher(manifest_launcher, None);
    let result = (|| {
        let issuer = std::fs::canonicalize(&issuer)?;
        let issuer_identity = issuer_identity(&issuer, &record.source)?;
        let output = bounded(
            &issuer,
            &[
                "execution",
                "reserve",
                record
                    .source
                    .to_str()
                    .ok_or_else(|| std::io::Error::other("non-Unicode Wrix workspace"))?,
            ],
            None,
        )?;
        let ticket = std::str::from_utf8(&output)
            .map_err(std::io::Error::other)?
            .trim();
        if ticket.len() != 64
            || !ticket
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(std::io::Error::other("unavailable Wrix generation ticket"));
        }
        let state_home = crate::lock::resolve_xdg_state_home().map_err(std::io::Error::other)?;
        let mut protection = read(directory, record).map_err(std::io::Error::other)?;
        protection.authority = Authority::Runtime(Runtime::Wrix {
            issuer,
            issuer_identity,
            workspace: record.source.clone(),
            state_home,
            ticket: ticket.into(),
        });
        write(directory, &protection, false).map_err(std::io::Error::other)?;
        spawn
            .launcher_env
            .retain(|(name, _)| name != "WRIX_EXECUTION_TICKET");
        spawn
            .launcher_env
            .push(("WRIX_EXECUTION_TICKET".into(), ticket.into()));
        Ok::<_, std::io::Error>(())
    })();
    if let Err(error) = result {
        tracing::warn!(?error, target = ?record.target, "Wrix lifetime unavailable; dispatched Cargo output remains protected");
    }
    Ok(())
}

fn issuer_identity(path: &Path, source: &Path) -> std::io::Result<Identity> {
    use std::os::unix::fs::MetadataExt;
    Directory::open(
        path.parent()
            .ok_or_else(|| std::io::Error::other("Wrix issuer has no parent"))?,
    )?;
    let metadata = std::fs::symlink_metadata(path)?;
    let operator = source
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "beads"))
        .and_then(Path::parent)
        .filter(|path| path.file_name().is_some_and(|name| name == ".loom"))
        .and_then(Path::parent)
        .ok_or_else(|| std::io::Error::other("Wrix lifetime source is not a direct bead clone"))?;
    if !path.is_absolute()
        || path.starts_with(operator)
        || !metadata.is_file()
        || (metadata.uid() != 0 && metadata.nlink() != 1)
        || metadata.mode() & 0o6000 != 0
        || metadata.mode() & 0o022 != 0
        || metadata.mode() & 0o111 == 0
        || ![0, rustix::process::geteuid().as_raw()].contains(&metadata.uid())
    {
        return Err(std::io::Error::other("untrusted Wrix lifetime issuer"));
    }
    Directory::open(
        path.parent()
            .ok_or_else(|| std::io::Error::other("issuer has no parent"))?,
    )?;
    let file = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    Ok(Identity::from_stat(&rustix::fs::fstat(file)?))
}

pub(super) fn bounded(
    program: &Path,
    args: &[&str],
    state_home: Option<&Path>,
) -> std::io::Result<Vec<u8>> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .env_remove("WRIX_EXECUTION_TICKET");
    if let Some(root) = state_home {
        command.env("XDG_STATE_HOME", root);
    }
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                let clock = crate::clock::SystemClock::new();
                runtime.block_on(async {
                    let output = crate::process::output_limited(
                        &mut command,
                        &clock,
                        Duration::from_secs(15),
                        16 * 1024,
                    )
                    .await
                    .map_err(std::io::Error::other)?;
                    if !output.status.success() {
                        return Err(std::io::Error::other(
                            "Wrix lifecycle observation unavailable",
                        ));
                    }
                    Ok(output.stdout)
                })
            })
            .join()
            .map_err(|_| std::io::Error::other("Wrix observation thread failed"))?
    })
}

pub(super) fn native_root() -> std::io::Result<Option<PathBuf>> {
    if !cfg!(target_os = "linux")
        || std::env::var_os("NIX_BUILD_TOP").is_some()
        || std::env::var_os(crate::agent::LOOM_INSIDE_ENV).is_some()
        || option_env!("LOOM_NATIVE_FLOCK_BIN").is_none()
        || option_env!("LOOM_NATIVE_SHELL_BIN").is_none()
        || option_env!("LOOM_NATIVE_STAT_BIN").is_none()
        || std::env::vars_os()
            .any(|(name, value)| name.as_encoded_bytes().starts_with(b"LD_") && !value.is_empty())
    {
        return Ok(None);
    }
    let uid = rustix::process::geteuid().as_raw();
    let delegated = format!("/user.slice/user-{uid}.slice/user@{uid}.service");
    let membership = std::fs::read_to_string("/proc/self/cgroup")?;
    let Some(current) = membership.lines().find_map(|line| line.strip_prefix("0::")) else {
        return Ok(None);
    };
    if !current.starts_with(&format!("{delegated}/")) {
        return Ok(None);
    }
    let root = Path::new("/sys/fs/cgroup").join(current.trim_start_matches('/'));
    if std::fs::read_to_string(root.join("cgroup.type"))?.trim() != "domain" {
        return Ok(None);
    }
    let directory = Directory::open(&root)?;
    if directory.stat()?.st_uid != uid {
        return Ok(None);
    }
    let root = root.join("loom-cargo");
    let directory = Directory::ensure(&root)?;
    directory.require_private()?;
    if rustix::fs::fstatfs(&directory)?.f_type != 0x6367_7270 {
        return Ok(None);
    }
    Ok(Some(root))
}

pub(super) fn dispatch_native(
    directory: &Directory,
    record: &Record,
    root: &Path,
) -> Result<(), Error> {
    let group = root.join(format!("generation-{}", nonce()?));
    let parent = Directory::ensure(root)?;
    parent.require_private()?;
    let handle = parent.create_child(
        group
            .file_name()
            .map(Path::new)
            .ok_or_else(|| std::io::Error::other("cgroup has no name"))?,
    )?;
    let identity = Identity::from_stat(&handle.stat()?);
    let procs = rustix::fs::statat(
        &handle,
        "cgroup.procs",
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(std::io::Error::from)?;
    let events = rustix::fs::statat(
        &handle,
        "cgroup.events",
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(std::io::Error::from)?;
    let boot = boot()?;
    let stat = option_env!("LOOM_NATIVE_STAT_BIN")
        .ok_or_else(|| std::io::Error::other("native cgroup identity helper unavailable"))?;
    let entry = format!(
        "set -euo pipefail\nIFS= read -r boot < /proc/sys/kernel/random/boot_id\n[[ \"$boot\" == '{boot}' ]]\nexec {{group}}>'{0}/cgroup.procs'\n[[ \"$('{stat}' -L -c '%d:%i' \"/proc/self/fd/$group\")\" == '{1}:{2}' ]]\nexec {{events}}<'{0}/cgroup.events'\n[[ \"$('{stat}' -L -c '%d:%i' \"/proc/self/fd/$events\")\" == '{3}:{4}' ]]\npopulated=unknown\nwhile read -r key value <&\"$events\"; do if [[ \"$key\" == populated ]]; then populated=\"$value\"; fi; done\n[[ \"$populated\" == 0 ]]\nexec {{events}}>&-\nprintf '%s\\n' \"$$\" >&\"$group\"\nexec {{group}}>&-\nexec \"$@\"\n",
        group.display().to_string().replace('\'', "'\\''"),
        procs.st_dev,
        procs.st_ino,
        events.st_dev,
        events.st_ino
    );
    let mut file = directory.create_file(
        Path::new(&entry_name(&group)?),
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )?;
    file.write_all(entry.as_bytes())?;
    file.sync_all()?;
    let entry_identity =
        Identity::from_stat(&rustix::fs::fstat(&file).map_err(std::io::Error::from)?);
    let execution = directory.read_private_file(Path::new("execution"))?;
    let execution_identity =
        Identity::from_stat(&rustix::fs::fstat(&execution).map_err(std::io::Error::from)?);
    let mut protection = read(directory, record)?;
    protection.authority = Authority::Runtime(Runtime::Native {
        group,
        identity,
        boot,
        entry_identity,
        execution_identity,
    });
    write(directory, &protection, false)
}

pub(super) fn reusable(directory: &Directory, record: &Record) -> Result<bool, Error> {
    let protection = read(directory, record)?;
    if protection.holds != record.holds {
        return Ok(false);
    }
    if let Authority::Retired(Runtime::Native {
        execution_identity, ..
    }) = &protection.authority
    {
        let file = directory.read_private_file(Path::new("execution"))?;
        return Ok(
            Identity::from_stat(&rustix::fs::fstat(&file).map_err(std::io::Error::from)?)
                == *execution_identity,
        );
    }
    let Authority::Runtime(Runtime::Native {
        group,
        identity,
        boot: expected,
        entry_identity,
        ..
    }) = protection.authority
    else {
        return Ok(false);
    };
    if !native_identity(&group, &identity, &expected)? {
        return Ok(false);
    }
    let file = directory.read_private_file(Path::new(&entry_name(&group)?))?;
    if Identity::from_stat(&rustix::fs::fstat(&file).map_err(std::io::Error::from)?)
        != entry_identity
    {
        return Ok(false);
    }
    Ok(true)
}

pub(super) fn rearm(directory: &Directory, record: &Record, root: &Path) -> Result<(), Error> {
    if matches!(
        read(directory, record)?.authority,
        Authority::Retired(Runtime::Native { .. })
    ) {
        dispatch_native(directory, record, root)?;
    }
    Ok(())
}

pub(super) fn entry(
    directory: &Directory,
    record: &Record,
    namespace: &Path,
) -> Result<PathBuf, Error> {
    let protection = read(directory, record)?;
    let Authority::Runtime(Runtime::Native {
        group,
        entry_identity,
        ..
    }) = protection.authority
    else {
        return Err(Error::Association {
            reason: "native entry has no live runtime binding",
        });
    };
    let name = entry_name(&group)?;
    let file = directory.read_private_file(Path::new(&name))?;
    if Identity::from_stat(&rustix::fs::fstat(&file).map_err(std::io::Error::from)?)
        != entry_identity
    {
        return Err(Error::Association {
            reason: "native runtime entry identity changed",
        });
    }
    Ok(namespace.join(name))
}

pub(super) fn wrap(command: &mut Command, target: &Path, entry: &Path) -> std::io::Result<()> {
    let Some(flock) = option_env!("LOOM_NATIVE_FLOCK_BIN") else {
        return Err(std::io::Error::other(
            "native containment lock helper unavailable",
        ));
    };
    let Some(shell) = option_env!("LOOM_NATIVE_SHELL_BIN") else {
        return Err(std::io::Error::other(
            "native containment shell unavailable",
        ));
    };
    let namespace = target
        .parent()
        .ok_or_else(|| std::io::Error::other("native target has no namespace"))?;
    let mut wrapper = Command::new(flock);
    wrapper
        .args(["--shared", "--no-fork"])
        .arg(namespace.join("lease"))
        .arg(flock)
        .args(["--exclusive", "--no-fork"])
        .arg(namespace.join("execution"))
        .arg(shell)
        .arg("-p")
        .arg(entry);
    wrapper.arg(command.get_program()).args(command.get_args());
    if let Some(directory) = command.get_current_dir() {
        wrapper.current_dir(directory);
    }
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            wrapper.env(key, value);
        } else {
            wrapper.env_remove(key);
        }
    }
    *command = wrapper;
    Ok(())
}

pub(super) fn retire(directory: &Directory, record: &Record) -> Result<bool, Error> {
    let mut protection = read(directory, record)?;
    if protection.holds != record.holds {
        return Ok(false);
    }
    match &protection.authority {
        Authority::Prepared => return Ok(record.lifetime == super::Lifetime::Prepared),
        Authority::Unknown => return Ok(false),
        Authority::Retired(_) => return Ok(record.lifetime == super::Lifetime::Dispatched),
        Authority::Runtime(_) if record.lifetime != super::Lifetime::Dispatched => {
            return Ok(false);
        }
        Authority::Runtime(Runtime::Wrix {
            issuer,
            issuer_identity: expected,
            workspace,
            state_home,
            ticket,
        }) => {
            if issuer_identity(issuer, workspace)? != *expected {
                return Ok(false);
            }
            let output = bounded(
                issuer,
                &[
                    "execution",
                    "reconcile",
                    workspace
                        .to_str()
                        .ok_or_else(|| std::io::Error::other("non-Unicode workspace"))?,
                    ticket,
                ],
                Some(state_home),
            )?;
            let observation: String =
                serde_json::from_slice(&output).map_err(std::io::Error::other)?;
            if observation != "retired" {
                return Ok(false);
            }
            if issuer_identity(issuer, workspace)? != *expected {
                return Ok(false);
            }
        }
        Authority::Runtime(Runtime::Native {
            group,
            identity,
            boot: expected,
            ..
        }) => {
            if !native_identity(group, identity, expected)? {
                return Ok(false);
            }
            let handle = Directory::open(group)?;
            let file = rustix::fs::openat(
                &handle,
                "cgroup.events",
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(std::io::Error::from)?;
            let mut events = String::new();
            File::from(file).take(4097).read_to_string(&mut events)?;
            if events.len() > 4096
                || events
                    .lines()
                    .filter(|line| line.starts_with("populated "))
                    .collect::<Vec<_>>()
                    != ["populated 0"]
            {
                return Ok(false);
            }
            retention::require_identity(&Directory::open(group)?, identity)?;
            std::fs::remove_dir(group)?;
        }
    }
    let Authority::Runtime(runtime) = protection.authority else {
        return Ok(false);
    };
    protection.authority = Authority::Retired(runtime);
    write(directory, &protection, false)?;
    if let Authority::Retired(Runtime::Native { group, .. }) = &protection.authority
        && let Some(root) = group.parent()
    {
        match std::fs::remove_dir(root) {
            Ok(()) => (),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(error) => tracing::debug!(?error, "native cgroup parent remains allocated"),
        }
    }
    Ok(true)
}

pub(super) fn source_absent_safe(directory: &Directory, record: &Record) -> Result<bool, Error> {
    let protection = read(directory, record)?;
    Ok(protection.holds == record.holds
        && match protection.authority {
            Authority::Prepared => record.lifetime == super::Lifetime::Prepared,
            Authority::Retired(_) | Authority::Runtime(Runtime::Native { .. }) => {
                record.lifetime == super::Lifetime::Dispatched
            }
            Authority::Unknown | Authority::Runtime(Runtime::Wrix { .. }) => false,
        })
}

pub(super) fn finish_native(root: &Path, target: &Path) -> Result<(), Error> {
    let fence = retention::Fence::trusted_try(root)?;
    let namespace = target
        .parent()
        .ok_or_else(|| std::io::Error::other("native target has no namespace"))?;
    let key = namespace
        .strip_prefix(root)
        .map_err(std::io::Error::other)?
        .to_str()
        .ok_or_else(|| std::io::Error::other("non-Unicode native namespace"))?;
    let record: Record = fence
        .database
        .record(super::RecordKind::CargoTarget, key)?
        .ok_or_else(|| std::io::Error::other("native lifetime has no ownership"))?;
    let Some((_, directory)) = fence.stored_directory(key, &record)? else {
        return Ok(());
    };
    let lease = directory.read_private_file(Path::new("lease"))?;
    if record.lease_identity.as_ref()
        != Some(&Identity::from_stat(
            &rustix::fs::fstat(&lease).map_err(std::io::Error::from)?,
        ))
    {
        return Err(Error::Association {
            reason: "native lifetime lease identity changed",
        });
    }
    match lease.try_lock() {
        Ok(()) => {
            retire(&directory, &record)?;
        }
        Err(std::fs::TryLockError::WouldBlock) => (),
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    }
    Ok(())
}

pub(super) fn finish_worker(namespace: &super::Namespace) -> Result<(), Error> {
    let association = super::Association::resolve(&namespace.operator)?;
    let fence = retention::Fence::open(&association)?;
    let key = namespace
        .path
        .strip_prefix(&namespace.operator)
        .map_err(std::io::Error::other)?
        .to_str()
        .ok_or_else(|| std::io::Error::other("non-Unicode worker namespace"))?;
    let record: Record = fence
        .database
        .record(super::RecordKind::CargoTarget, key)?
        .ok_or_else(|| std::io::Error::other("worker lifetime has no ownership"))?;
    if record.version != 3 {
        return Ok(());
    }
    let Some((_, directory)) = fence.stored_directory(key, &record)? else {
        return Ok(());
    };
    retention::require_identity(&directory, &namespace.identity)?;
    if record.lease_identity.as_ref()
        != Some(&Identity::from_stat(
            &rustix::fs::fstat(&namespace.lease).map_err(std::io::Error::from)?,
        ))
    {
        return Err(Error::Association {
            reason: "worker lifetime lease identity changed",
        });
    }
    retire(&directory, &record)?;
    Ok(())
}

fn native_identity(group: &Path, identity: &Identity, expected_boot: &str) -> Result<bool, Error> {
    if !cfg!(target_os = "linux")
        || !group.starts_with("/sys/fs/cgroup")
        || group
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        || group
            .parent()
            .and_then(Path::file_name)
            .is_none_or(|name| name != "loom-cargo")
        || expected_boot != boot()?
    {
        return Ok(false);
    }
    let name = group
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| std::io::Error::other("invalid native cgroup name"))?;
    let Some(suffix) = name.strip_prefix("generation-") else {
        return Ok(false);
    };
    if suffix.len() != 32
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Ok(false);
    }
    let directory = Directory::open(group)?;
    directory.require_private()?;
    Ok(Identity::from_stat(&directory.stat()?) == *identity
        && rustix::fs::fstatfs(&directory)
            .map_err(std::io::Error::from)?
            .f_type
            == 0x6367_7270)
}

fn boot() -> std::io::Result<String> {
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    if boot.trim().len() != 36
        || !boot
            .trim()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        return Err(std::io::Error::other("native boot generation unavailable"));
    }
    Ok(boot.trim().into())
}

fn nonce() -> std::io::Result<String> {
    let mut bytes = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let digits = b"0123456789abcdef";
    Ok(bytes
        .iter()
        .flat_map(|byte| {
            [
                char::from(digits[usize::from(byte >> 4)]),
                char::from(digits[usize::from(byte & 15)]),
            ]
        })
        .collect())
}

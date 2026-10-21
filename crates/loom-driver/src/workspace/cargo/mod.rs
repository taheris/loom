//! Private Cargo output selection. A namespace record is never evidence or disposal authority.

mod record;
pub mod retention;

use std::fs::{self, File};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Association, Error, Identity};
use crate::agent::{MountSpec, SpawnConfig};
use crate::clock::Clock;
use crate::identifier::BeadId;
use crate::scratch::directory::Directory;
use crate::state::RecordKind;
use record::{Lifetime, Placement, Record};
use retention::Fence;

const WORKER_TARGET: &str = "/loom-cargo-target";

/// Apply Wrix's cache-sharing policy consistently before any additional cache projection.
///
/// # Errors
/// Invalid or indirect policy is not permission to share.
pub fn shared_cache_config(
    config: &crate::config::LoomTopConfig,
    workspace: &Path,
) -> std::io::Result<crate::config::LoomTopConfig> {
    let mut admitted = config.clone();
    if !loom_shared::artifact::sharing_allowed(workspace).map_err(std::io::Error::other)? {
        admitted.sccache_dir = None;
    }
    Ok(admitted)
}

/// A private generated namespace retained on every exit, including process death.
pub struct Namespace {
    path: PathBuf,
    operator: PathBuf,
    identity: Identity,
    lease: File,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct Context([u8; 32]);

impl Context {
    fn name(&self) -> String {
        format!("{:x}", sha2::digest::Output::<Sha256>::from(self.0))
    }
}

impl Namespace {
    pub fn target(&self) -> PathBuf {
        self.path.join("target")
    }

    /// Durably mark dispatch before projecting this generation to a worker.
    ///
    /// # Errors
    /// Changed ownership or quarantine stops projection and retains the output.
    pub fn configure(&self, spawn: &mut SpawnConfig) -> Result<(), Error> {
        let association = Association::resolve(&self.operator)?;
        let fence = Fence::open(&association)?;
        let key = self
            .path
            .strip_prefix(&self.operator)
            .map_err(std::io::Error::other)?
            .to_string_lossy()
            .into_owned();
        let mut record: Record = fence
            .database
            .record(RecordKind::CargoTarget, &key)?
            .ok_or(Error::Association {
                reason: "Cargo dispatch has no ownership record",
            })?;
        if record.version != 2
            || record.lifetime != Lifetime::Prepared
            || record.placement != Placement::Live
            || record.namespace_identity != self.identity
            || record.target != self.target()
            || spawn.workspace != record.source
            || spawn
                .profile_config
                .as_ref()
                .is_some_and(|profile| profile != &record.profile)
            || record.lease_identity.as_ref()
                != Some(&Identity::from_stat(
                    &rustix::fs::fstat(&self.lease).map_err(std::io::Error::from)?,
                ))
        {
            return Err(Error::Association {
                reason: "Cargo dispatch ownership or lifecycle changed",
            });
        }
        fence.paths(&key, &record)?;
        let directory = Directory::open(&self.path)?;
        directory.require_private()?;
        retention::require_identity(&directory, &self.identity)?;
        retention::require_identity(&Directory::open(&record.source)?, &record.source_identity)?;
        let target = Directory::open(&self.target())?;
        if record.target_identity.as_ref() != Some(&Identity::from_stat(&target.stat()?)) {
            return Err(Error::Association {
                reason: "Cargo dispatch target identity changed",
            });
        }
        if retention::projected(&directory)? {
            return Err(Error::Association {
                reason: "generated Cargo namespace already has a projection tombstone; retained",
            });
        }
        directory
            .create_file(
                Path::new(retention::PROJECTION),
                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            )?
            .sync_all()?;
        directory.sync()?;
        record.lifetime = Lifetime::Dispatched;
        fence
            .database
            .put_record(RecordKind::CargoTarget, &key, &record)?;
        for key in [
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET_DIR",
            "CARGO_BUILD_BUILD_DIR",
        ] {
            spawn.env.retain(|(name, _)| name != key);
            spawn.env.push((key.into(), WORKER_TARGET.into()));
        }
        spawn.mounts.push(MountSpec {
            host_path: self.target(),
            container_path: WORKER_TARGET.into(),
            read_only: false,
        });
        Ok(())
    }
}

/// Allocate a unique owned generation and optionally seed compatible worker dependencies.
///
/// # Errors
/// Unsafe storage and state failures stop preparation without touching legacy targets.
pub fn prepare(
    association: &Association,
    workspace: &Path,
    profile: &Path,
    reuse: crate::config::CargoTargetReuse,
    request: retention::Request<'_>,
    clock: &dyn Clock,
) -> Result<Namespace, Error> {
    association.recheck()?;
    let beads = association.operator().join(".loom/beads");
    if workspace.parent() != Some(beads.as_path()) {
        return Err(Error::Association {
            reason: "Cargo namespace requires a direct worker clone",
        });
    }
    let bead: BeadId = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(Error::Association {
            reason: "worker has no Cargo owner",
        })?
        .parse()
        .map_err(|_| Error::Association {
            reason: "invalid Cargo worker owner",
        })?;
    let source_identity = Identity::from_stat(&Directory::open(workspace)?.stat()?);
    let context = context(workspace, Some(profile))?;
    let fence = Fence::open(association)?;
    fence.admit(request.policy, request.reservation, clock)?;
    let root = fence.root.ensure_child(Path::new("worker"))?;
    let owner = root.ensure_child(Path::new(bead.as_str()))?;
    owner.ensure_child(Path::new(&context.name()))?;
    let parent = association
        .operator()
        .join(".loom/cargo-artifacts/worker")
        .join(bead.as_str())
        .join(context.name());
    let generation = tempfile::Builder::new()
        .prefix("generation-")
        .tempdir_in(&parent)?
        .keep();
    let generation_handle = Directory::open(&generation)?;
    generation_handle.require_private()?;
    let lease = generation_handle.append_private_file(Path::new("lease"))?;
    lease.lock()?;
    let namespace = Namespace {
        path: generation,
        operator: association.operator().into(),
        identity: Identity::from_stat(&generation_handle.stat()?),
        lease,
    };
    let mut record = Record {
        version: 2,
        bead,
        source: workspace.into(),
        source_identity,
        target: namespace.target(),
        namespace_identity: Identity::from_stat(&generation_handle.stat()?),
        profile: profile.into(),
        context,
        lease_identity: Some(Identity::from_stat(
            &rustix::fs::fstat(&namespace.lease).map_err(std::io::Error::from)?,
        )),
        target_identity: None,
        reservation: request.reservation,
        created: Some(clock.wall_now()),
        lifetime: Lifetime::Prepared,
        placement: Placement::Live,
        holds: std::collections::BTreeSet::new(),
    };
    let key = namespace
        .path
        .strip_prefix(association.operator())
        .map_err(std::io::Error::other)?
        .to_string_lossy()
        .into_owned();
    let database = &fence.database;
    database.put_record(RecordKind::CargoTarget, &key, &record)?;
    let donors = database
        .records::<Record>(RecordKind::CargoTarget)?
        .into_iter()
        .filter_map(|(_, donor)| eligible_donor(association, &record, donor).transpose())
        .collect::<Result<Vec<_>, _>>()?;
    let recipient = loom_shared::artifact::Location {
        workspace: workspace.into(),
        target: namespace.target(),
    };
    match loom_shared::artifact::prepare_managed(&beads, &recipient, profile, reuse, &donors) {
        Ok(outcome) => {
            tracing::info!(?outcome, target = ?recipient.target, "managed worker target prepared");
        }
        Err(error) => tracing::warn!(
            ?error,
            "managed dependency seeding unavailable; using a cold private target"
        ),
    }
    if !namespace.target().try_exists()? {
        generation_handle.create_child(Path::new("target"))?;
    }
    record.target_identity = Some(Identity::from_stat(
        &Directory::open(&namespace.target())?.stat()?,
    ));
    database.put_record(RecordKind::CargoTarget, &key, &record)?;
    fence.admit(request.policy, retention::Bytes::default(), clock)?;
    Ok(namespace)
}

fn eligible_donor(
    association: &Association,
    recipient: &Record,
    donor: Record,
) -> Result<Option<loom_shared::artifact::Location>, Error> {
    if donor.target == recipient.target
        || !matches!(donor.version, 1 | 2)
        || donor.placement != Placement::Live
        || (donor.version == 2 && donor.target_identity.is_none())
        || donor.profile != recipient.profile
        || donor.context != recipient.context
    {
        return Ok(None);
    }
    let root = association
        .operator()
        .join(".loom/cargo-artifacts/worker")
        .join(donor.bead.as_str())
        .join(donor.context.name());
    let Some(parent) = donor.target.parent() else {
        return Ok(None);
    };
    if donor.target.file_name().is_none_or(|name| name != "target")
        || parent.parent() != Some(root.as_path())
        || donor.source
            != association
                .operator()
                .join(".loom/beads")
                .join(donor.bead.as_str())
    {
        return Ok(None);
    }
    let source = match Directory::open(&donor.source) {
        Ok(directory) => directory,
        Err(error) => {
            tracing::warn!(?error, source = ?donor.source, "Cargo donor source unavailable");
            return Ok(None);
        }
    };
    let directory = match Directory::open(parent) {
        Ok(directory) => directory,
        Err(error) => {
            tracing::warn!(?error, path = ?parent, "Cargo donor generation unavailable");
            return Ok(None);
        }
    };
    if Identity::from_stat(&source.stat()?) != donor.source_identity
        || Identity::from_stat(&directory.stat()?) != donor.namespace_identity
    {
        return Ok(None);
    }
    if let Some(identity) = &donor.target_identity {
        match Directory::open(&donor.target) {
            Ok(target) if Identity::from_stat(&target.stat()?) == *identity => {}
            Ok(_) => return Ok(None),
            Err(error) => {
                tracing::warn!(?error, target = ?donor.target, "Cargo donor target unavailable");
                return Ok(None);
            }
        }
    }
    Ok(Some(loom_shared::artifact::Location {
        workspace: donor.source,
        target: donor.target,
    }))
}

fn context(workspace: &Path, profile: Option<&Path>) -> std::io::Result<Context> {
    let mut hash = Sha256::new();
    hash.update(b"loom-cargo-context-v1");
    if let Some(profile) = profile {
        hash.update(profile.as_os_str().as_encoded_bytes());
        hash.update(fs::read(profile)?);
    } else {
        hash.update(workspace.as_os_str().as_encoded_bytes());
        let executable = std::env::current_exe()?;
        let runtime = fs::metadata(&executable)?;
        hash.update(executable.as_os_str().as_encoded_bytes());
        hash.update(runtime.dev().to_le_bytes());
        hash.update(runtime.ino().to_le_bytes());
        for key in [
            "PATH",
            "RUSTC",
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "RUSTUP_TOOLCHAIN",
        ] {
            hash.update(key.as_bytes());
            if let Some(value) = std::env::var_os(key) {
                hash.update(value.as_encoded_bytes());
            }
        }
        hash.update(std::env::consts::OS.as_bytes());
        hash.update(std::env::consts::ARCH.as_bytes());
    }
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        ".cargo/config",
        ".cargo/config.toml",
        "rust-toolchain",
        "rust-toolchain.toml",
    ] {
        hash.update(name.as_bytes());
        let path = workspace.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => {
                if name.starts_with(".cargo/")
                    && !fs::symlink_metadata(workspace.join(".cargo"))?.is_dir()
                {
                    return Err(std::io::Error::other(
                        "Cargo configuration directory is indirect",
                    ));
                }
                let bytes = fs::read(path)?;
                hash.update(bytes.len().to_le_bytes());
                hash.update(bytes);
            }
            Ok(_) => {
                return Err(std::io::Error::other(
                    "Cargo context contains an indirect or special file",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => hash.update(b"absent"),
            Err(error) => return Err(error),
        }
    }
    Ok(Context(hash.finalize().into()))
}

/// Select isolation for native verifiers; worker feedback retains its admitted worker projection.
///
/// # Errors
/// Reports unavailable trusted output storage before any verifier is executed.
pub fn verifier_command(command: &mut Command, workspace: &Path) -> std::io::Result<()> {
    if std::env::var_os(crate::agent::LOOM_INSIDE_ENV).is_none() {
        trusted_command(command, workspace)?;
    }
    Ok(())
}

/// Isolate trusted subprocesses from inherited worker targets, wrappers and compiler-cache servers.
///
/// # Errors
/// An unavailable or indirect trusted output root refuses execution, rather than using ambient output.
pub fn trusted_command(command: &mut Command, workspace: &Path) -> std::io::Result<()> {
    trusted_command_at(command, workspace, &trusted_root()?)
}

pub(super) fn trusted_root() -> std::io::Result<PathBuf> {
    std::path::absolute(
        crate::lock::resolve_xdg_state_home()
            .map_err(std::io::Error::other)?
            .join("loom/trusted-cargo"),
    )
}

fn trusted_command_at(
    command: &mut Command,
    workspace: &Path,
    output: &Path,
) -> std::io::Result<()> {
    let workspace = std::path::absolute(workspace)?;
    let workspace = workspace
        .ancestors()
        .find(|parent| parent.join(".git").exists())
        .unwrap_or(&workspace)
        .to_path_buf();
    if workspace
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(std::io::Error::other("invalid trusted Cargo workspace"));
    }
    let controlling = workspace
        .ancestors()
        .find_map(|ancestor| {
            let group = ancestor.parent()?;
            let loom = group.parent()?;
            (matches!(
                group.file_name()?.to_str(),
                Some("beads" | "tune" | "scratch")
            ) && loom.file_name()? == ".loom")
                .then(|| loom.parent())
                .flatten()
        })
        .unwrap_or(&workspace);
    let state = crate::state::path(controlling);
    let controlling = state
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| std::io::Error::other("trusted workspace has no controlling root"))?;
    if output.starts_with(controlling) || controlling.starts_with(output) {
        return Err(std::io::Error::other(
            "trusted Cargo output overlaps a source checkout",
        ));
    }
    let root = Directory::ensure(output)?;
    root.require_private()?;
    let context = context(&workspace, None)?.name();
    root.ensure_child(Path::new(&context))?;
    let target = output.join(context);
    let keys: Vec<_> = std::env::vars_os()
        .map(|(key, _)| key)
        .chain(command.get_envs().map(|(key, _)| key.to_os_string()))
        .filter(|key| key.as_encoded_bytes().starts_with(b"SCCACHE_"))
        .collect();
    for key in keys {
        command.env_remove(key);
    }
    for key in [
        "RUSTC_WRAPPER",
        "CARGO_BUILD_RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    ] {
        command.env(key, "");
    }
    for key in [
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET_DIR",
        "CARGO_BUILD_BUILD_DIR",
    ] {
        command.env(key, &target);
    }
    Ok(())
}

#[cfg(test)]
mod tests;

//! Host composition, separate from the extractable Issues storage contract.

use loom_driver::{
    agent::{ProtocolError, SpawnConfig, WorkerTrackingAccess},
    bd::{BdClient, CommandRunner},
};
use loom_issues::{memory::Backend, tracking::Store};

/// A configured tracker and the worker boundary its actual implementation supports.
/// This is not a transport probe or a worker-supplied authority claim.
pub trait Adapter: Store {
    fn worker_access(&self) -> WorkerTrackingAccess;
}

impl<R: CommandRunner> Adapter for BdClient<R> {
    fn worker_access(&self) -> WorkerTrackingAccess {
        // Wrix owns authenticated shared Beads access. Loom supplies no socket,
        // auto-start setting, credential, host RPC, or extra mount here.
        WorkerTrackingAccess::LauncherManaged
    }
}

impl Adapter for Backend {
    fn worker_access(&self) -> WorkerTrackingAccess {
        // Controlled in-process agents share handles, not a native database.
        // Serializing this selection for a separate worker must fail closed.
        WorkerTrackingAccess::InProcess
    }
}

/// Bind actual adapter access without supplying any launcher-owned credentials or sockets.
///
/// # Errors
/// Rejects Beads-only environment or mounts in an in-process configuration.
pub fn configure_worker<T: Adapter>(
    adapter: &T,
    config: &mut SpawnConfig,
) -> Result<(), ProtocolError> {
    let access = adapter.worker_access();
    if access == WorkerTrackingAccess::InProcess
        && (config.env.iter().any(|(key, _)| {
            ["BD_", "BEADS_", "DOLT_"]
                .iter()
                .any(|prefix| key.starts_with(prefix))
        }) || config.mounts.iter().any(|mount| {
            let path = &mount.container_path;
            path.starts_with("/run/wrix/beads")
                || path.starts_with("/run/wrix/dolt.sock")
                || path.starts_with("/run/wrix/beads-sync")
                || path.starts_with("/run/wrix/beads-git")
                || path.starts_with("/run/wrix/beads-lock")
                || path.starts_with("/run/wrix-beads")
        }))
    {
        return Err(ProtocolError::Unsupported);
    }
    config.tracking_access = access;
    Ok(())
}

impl<T: Adapter + ?Sized> Adapter for &T {
    fn worker_access(&self) -> WorkerTrackingAccess {
        (**self).worker_access()
    }
}

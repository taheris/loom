use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use super::{Association, Error};
use crate::agent::{MountSpec, SpawnConfig};
use crate::scratch::directory::Directory;

/// Recheck host authority and all additive mounts immediately before publishing launch configuration.
///
/// # Errors
/// A bead launch cannot expose canonical history, either host Git authority, or integration markers.
pub fn validate_spawn(config: &SpawnConfig) -> Result<(), Error> {
    if let Some(operator) = worker_operator(&config.workspace) {
        let ambient = std::env::var_os("WRIX_UNSAFE_PODMAN_SOCKET");
        validate_worker_environment(&config.launcher_env, ambient.as_deref())?;
        let association = Association::resolve(operator)?;
        for (key, value) in &config.launcher_env {
            if matches!(key.as_str(), "HOME" | "USER") && std::env::var(key).as_ref() != Ok(value) {
                return Err(Error::Association {
                    reason: "launcher environment changes profile mount expansion",
                });
            }
        }
        association.validate_worker(
            &config.workspace,
            &config.mounts,
            config.profile_config.as_deref(),
        )?;
    }
    Ok(())
}

fn validate_worker_environment(
    launcher_env: &[(String, String)],
    ambient_unsafe_socket: Option<&std::ffi::OsStr>,
) -> Result<(), Error> {
    let requested = ambient_unsafe_socket.is_some_and(|value| value != "0")
        || launcher_env
            .iter()
            .any(|(name, value)| name == "WRIX_UNSAFE_PODMAN_SOCKET" && value != "0");
    if requested {
        return Err(Error::Association {
            reason: "worker launch cannot project the host engine socket through an unsafe launcher override",
        });
    }
    Ok(())
}

#[cfg(test)]
mod environment_tests {
    use super::validate_worker_environment;
    use std::ffi::OsStr;

    #[test]
    fn worker_environment_rejects_ambient_engine_authority_before_launch() {
        assert!(validate_worker_environment(&[], Some(OsStr::new("1"))).is_err());
        assert!(
            validate_worker_environment(
                &[("WRIX_UNSAFE_PODMAN_SOCKET".into(), "0".into())],
                Some(OsStr::new("1")),
            )
            .is_err()
        );
    }

    #[test]
    fn worker_environment_rejects_explicit_engine_authority_before_launch() {
        assert!(
            validate_worker_environment(&[("WRIX_UNSAFE_PODMAN_SOCKET".into(), "1".into())], None,)
                .is_err()
        );
        assert!(validate_worker_environment(&[], None).is_ok());
        assert!(validate_worker_environment(&[], Some(OsStr::new("0"))).is_ok());
    }
}

impl Association {
    /// Admit only a self-contained bead checkout with no projection of host authority.
    ///
    /// # Errors
    /// Unsafe workspace, profile, or per-launch mount sources and destinations are rejected.
    pub fn validate_worker(
        &self,
        workspace: &Path,
        mounts: &[MountSpec],
        profile: Option<&Path>,
    ) -> Result<(), Error> {
        self.recheck()?;
        let workspace = std::path::absolute(workspace)?;
        if worker_operator(&workspace) != Some(self.operator()) {
            return Err(Error::Association {
                reason: "worker checkout is not isolated beneath the registered operator",
            });
        }
        let directory = Directory::open(&workspace)?;
        directory.child(Path::new(".git"))?;
        self.deny_source(&workspace)?;
        for mount in mounts {
            self.validate_mount(&mount.host_path, &mount.container_path)?;
        }
        let profile = profile.ok_or(Error::Association {
            reason: "worker launch has no selected profile configuration",
        })?;
        let profile = resolved_path(profile)?;
        if overlaps(&profile, &workspace) {
            return Err(Error::Mount { path: profile });
        }
        let profile: Profile =
            serde_json::from_slice(&std::fs::read(&profile)?).map_err(std::io::Error::other)?;
        for mount in profile.profile.mounts {
            self.validate_mount(&expand_source(&mount.source)?, &mount.dest)?;
        }
        Ok(())
    }

    /// Check a planned projection before creating its source or launching a worker.
    ///
    /// # Errors
    /// A mount exposing host authority or overriding the workspace is rejected.
    pub fn admit_mount(&self, mount: &MountSpec) -> Result<(), Error> {
        self.recheck()?;
        self.validate_mount(&mount.host_path, &mount.container_path)
    }

    fn validate_mount(&self, source: &Path, destination: &Path) -> Result<(), Error> {
        self.deny_source(&resolved_path(source)?)?;
        if !plain_absolute(destination) || Path::new("/workspace").starts_with(destination) {
            return Err(Error::Mount {
                path: destination.to_path_buf(),
            });
        }
        Ok(())
    }

    fn deny_source(&self, source: &Path) -> Result<(), Error> {
        let database = crate::state::path(self.operator());
        let sidecar = |suffix: &str| {
            let mut path = database.as_os_str().to_os_string();
            path.push(suffix);
            PathBuf::from(path)
        };
        for protected in [
            database.clone(),
            sidecar("-wal"),
            sidecar("-shm"),
            sidecar(".lock"),
            sidecar(".init.lock"),
            self.integration().to_path_buf(),
            self.operator().join(".git"),
        ] {
            if overlaps(source, &protected) {
                return Err(Error::Mount {
                    path: source.to_path_buf(),
                });
            }
        }
        Ok(())
    }
}

fn worker_operator(workspace: &Path) -> Option<&Path> {
    let beads = workspace.parent()?;
    let loom = beads.parent()?;
    (beads.file_name()? == "beads" && loom.file_name()? == ".loom")
        .then(|| loom.parent())
        .flatten()
}

fn overlaps(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn plain_absolute(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
        && !path.as_os_str().as_encoded_bytes().contains(&b':')
}

fn resolved_path(path: &Path) -> Result<PathBuf, Error> {
    if !plain_absolute(path) {
        return Err(Error::Mount {
            path: path.to_path_buf(),
        });
    }
    let mut existing = path;
    let mut suffix = Vec::new();
    while !existing.try_exists()? {
        suffix.push(existing.file_name().ok_or_else(|| Error::Mount {
            path: path.to_path_buf(),
        })?);
        existing = existing.parent().ok_or_else(|| Error::Mount {
            path: path.to_path_buf(),
        })?;
    }
    let mut resolved = existing.canonicalize()?;
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn expand_source(source: &str) -> Result<PathBuf, Error> {
    let mut expanded = source.to_owned();
    for variable in ["HOME", "USER"] {
        if expanded.contains(&format!("${variable}"))
            || (variable == "HOME" && expanded.starts_with('~'))
        {
            let value = std::env::var(variable).map_err(|_| Error::Association {
                reason: "profile mount expansion is unavailable",
            })?;
            if let Some(suffix) = expanded.strip_prefix('~') {
                expanded = format!("{value}{suffix}");
            }
            expanded = expanded.replace(&format!("${variable}"), &value);
        }
    }
    Ok(PathBuf::from(expanded))
}

#[derive(Deserialize)]
struct Profile {
    profile: Mounts,
}

#[derive(Deserialize)]
struct Mounts {
    #[serde(default)]
    mounts: Vec<ProfileMount>,
}

#[derive(Deserialize)]
struct ProfileMount {
    source: String,
    dest: PathBuf,
}

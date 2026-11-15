//! Ordinary native recipes use the existing agent boundary, never a remote unsandboxed builder.

use std::path::Path;

use super::{Digest, Error};

/// Captured execution context is part of identity, not a worker-supplied trust flag.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Context {
    LocalAgent { image: Digest },
    Sandboxed,
}

impl Context {
    pub(super) fn capture() -> Result<Self, Error> {
        Self::at(Path::new("/etc/wrix/image-agent"))
    }

    fn at(marker: &Path) -> Result<Self, Error> {
        use std::os::unix::fs::MetadataExt as _;

        if !cfg!(target_os = "linux") {
            return Ok(Self::Sandboxed);
        }
        let metadata = match marker.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::Sandboxed);
            }
            Err(error) => return Err(error.into()),
        };
        // Only root-owned regular markers may select local execution.
        if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Ok(Self::Sandboxed);
        }
        let roots = marker.with_file_name("store-roots");
        let metadata = match roots.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::Sandboxed);
            }
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_file()
            || metadata.uid() != 0
            || metadata.mode() & 0o022 != 0
            || metadata.len() > 1_048_576
        {
            return Ok(Self::Sandboxed);
        }
        // Bind the materialized public SDK/profile roots, never credentials.
        Ok(Self::LocalAgent {
            image: Digest::of(&serde_json::to_vec(&(
                std::fs::read(marker)?,
                std::fs::read(roots)?,
            ))?),
        })
    }

    pub(super) const fn local(&self) -> bool {
        matches!(self, Self::LocalAgent { .. })
    }

    pub(super) fn require_current(&self) -> Result<(), Error> {
        if *self != Self::capture()? {
            return Err(Error::GraphChanged);
        }
        Ok(())
    }
}

/// Apply one captured policy before execution; failure never changes this choice.
pub(super) fn policy(context: Option<&Context>, arguments: &mut Vec<&str>) {
    let local = context.is_some_and(Context::local);
    arguments.extend([
        "--max-jobs",
        "1",
        "--cores",
        "2",
        "--option",
        "sandbox",
        if local { "false" } else { "true" },
        "--option",
        "sandbox-fallback",
        "false",
        "--option",
        "sandbox-paths",
        "",
        "--option",
        "extra-sandbox-paths",
        "",
        "--option",
        "builders",
        "",
    ]);
    if local {
        // Never send a worker recipe to a host/remote daemon without confinement.
        arguments.extend(["--store", "local"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn build_parallelism_is_bounded_without_changing_confinement() {
        for context in [
            None,
            Some(Context::Sandboxed),
            Some(Context::LocalAgent {
                image: Digest::of(b"policy-unit-fixture"),
            }),
        ] {
            let mut arguments = vec!["build"];
            policy(context.as_ref(), &mut arguments);
            assert_eq!(
                &arguments[..5],
                ["build", "--max-jobs", "1", "--cores", "2"]
            );
            let local = context.as_ref().is_some_and(Context::local);
            let sandbox = if local { "false" } else { "true" };
            assert!(
                arguments
                    .windows(3)
                    .any(|args| args == ["--option", "sandbox", sandbox])
            );
            assert_eq!(
                arguments
                    .windows(2)
                    .any(|args| args == ["--store", "local"]),
                local
            );
            assert!(
                arguments
                    .windows(3)
                    .any(|args| args == ["--option", "sandbox-fallback", "false"])
            );
            assert!(
                arguments
                    .windows(3)
                    .any(|args| args == ["--option", "builders", ""])
            );
        }
    }

    #[test]
    fn absent_writable_or_indirect_markers_cannot_select_local_execution() {
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("marker");
        assert_eq!(Context::at(&marker).unwrap(), Context::Sandboxed);
        std::fs::write(&marker, "pi\n").unwrap();
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert_eq!(Context::at(&marker).unwrap(), Context::Sandboxed);
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(marker, &alias).unwrap();
        assert_eq!(Context::at(&alias).unwrap(), Context::Sandboxed);
    }
}

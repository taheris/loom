//! A fixed container-engine observation, never an arbitrary-command fallback.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, PathBuf};
use std::process::Command;
use std::sync::Arc;

use displaydoc::Display;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::tracked::{Observed, Outcome};
use super::{Candidate, Capability, Definition, Digest, Effect, Execution, Origin, wire};
use crate::inventory::snapshot::{ResourcePath, Snapshot};
use crate::inventory::{Duty, Inventory, Name};

/// An explicit local engine endpoint; no ambient connection or SSH authority is selected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PathBuf", into = "PathBuf")]
pub struct Endpoint(PathBuf);

impl TryFrom<PathBuf> for Endpoint {
    type Error = Error;

    fn try_from(path: PathBuf) -> Result<Self, Error> {
        let normalized: PathBuf = path.components().collect();
        if !path.is_absolute()
            || path.file_name().is_none()
            || path.to_str().is_none_or(|value| {
                value
                    .chars()
                    .any(|character| character.is_control() || matches!(character, '?' | '#' | '%'))
            })
            || normalized.as_os_str() != path.as_os_str()
            || path
                .components()
                .skip(1)
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(Error::Endpoint { path });
        }
        Ok(Self(path))
    }
}

impl From<Endpoint> for PathBuf {
    fn from(endpoint: Endpoint) -> Self {
        endpoint.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Description {
    effect: Effect,
    executable: PathBuf,
    artifact: Digest,
    adapter: PathBuf,
    adapter_artifact: Digest,
    endpoint: Endpoint,
    system: Name,
}

/// The fixed engine client and endpoint are captured before generating any candidate.
pub struct Client {
    description: Description,
}

impl Client {
    /// Capture the explicit podman client, including its current executable identity.
    ///
    /// # Errors
    /// Rejects non-absolute executables, non-podman clients, and unreadable artifacts.
    pub fn capture(executable: PathBuf, endpoint: Endpoint) -> Result<Self, super::Error> {
        if !executable.is_absolute()
            || executable
                .file_name()
                .is_none_or(|name| name != "podman" && name != "podman-remote")
        {
            return Err(Error::Executable.into());
        }
        let executable = std::fs::canonicalize(executable).map_err(Error::Io)?;
        let adapter = std::env::current_exe().map_err(Error::Io)?;
        let description = Description {
            effect: Effect::ContainerEngineInfo,
            adapter_artifact: super::read_artifact(&adapter)?,
            adapter,
            artifact: super::read_artifact(&executable)?,
            executable,
            endpoint,
            system: Name::try_from(format!(
                "{}-{}",
                std::env::consts::ARCH,
                std::env::consts::OS
            ))?,
        };
        Ok(Self { description })
    }
}

pub(super) struct Engine {
    description: Description,
}

impl Definition {
    /// Bind the supported engine-info effect to its actual executable and explicit Unix endpoint.
    ///
    /// # Errors
    /// Rejects stale inventory, unresolved responsibility, missing producer inputs or executable.
    pub fn container_engine_info(
        snapshot: &Snapshot,
        inventory: &Inventory,
        target: Name,
        producer: ResourcePath,
        subjects: &BTreeSet<Name>,
        duties: &BTreeSet<Duty>,
        client: Client,
    ) -> Result<Self, super::Error> {
        super::require_context(snapshot, inventory)?;
        let responsibilities = super::responsibility(inventory, &target, subjects, duties)?;
        let mut inputs = BTreeMap::<ResourcePath, BTreeSet<Origin>>::new();
        super::add_rust_inputs(&mut inputs, inventory, &target, &producer)?;
        for name in subjects {
            super::add_subject_inputs(&mut inputs, inventory.subject(name)?);
        }
        let inputs = super::capture_inputs(snapshot, inputs)?;
        let description = client.description;
        let definition = Digest::of(&serde_json::to_vec(&(
            snapshot.identity(),
            &target,
            &producer,
            &responsibilities,
            &inputs,
            &description,
        ))?);
        Ok(Self {
            candidate: Candidate::SupportedEffect {
                contract: wire::Contract {
                    target,
                    snapshot: snapshot.identity().clone(),
                    definition,
                    origin: producer,
                    responsibilities,
                    inputs,
                },
                execution: description.clone(),
            },
            execution: Execution::Effect(Arc::new(Engine { description })),
        })
    }
}

impl Engine {
    pub(super) fn require_current(&self) -> Result<(), super::Error> {
        if super::read_artifact(&self.description.executable)? != self.description.artifact
            || super::read_artifact(&self.description.adapter)? != self.description.adapter_artifact
        {
            return Err(Error::Changed.into());
        }
        Ok(())
    }

    pub(super) fn run(&self, target: Name) -> Result<Observed, super::Error> {
        let outcome = if !cfg!(unix) {
            Outcome::Inapplicable {
                system: self.description.system.clone(),
            }
        } else if !self
            .description
            .endpoint
            .0
            .try_exists()
            .map_err(Error::Io)?
        {
            Outcome::Unavailable {
                capability: Capability::ContainerEngine,
                detail: "the explicit Unix engine endpoint is absent; restore the engine service and retry".into(),
            }
        } else {
            let output = Command::new(&self.description.executable)
                .env_clear()
                .args(["--remote", "--url"])
                .arg(format!("unix://{}", self.description.endpoint.0.display()))
                .args(["info", "--format", "json"])
                .output()
                .map_err(Error::Io)?;
            if output.status.success() {
                let info: serde_json::Value =
                    serde_json::from_slice(&output.stdout).map_err(Error::Response)?;
                let nonempty = |pointer| {
                    info.pointer(pointer)
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|value| !value.trim().is_empty())
                };
                if ![
                    "/host/os",
                    "/host/arch",
                    "/host/kernel",
                    "/store/graphRoot",
                    "/store/graphDriverName",
                ]
                .into_iter()
                .all(nonempty)
                    || !info
                        .pointer("/host/security/rootless")
                        .is_some_and(serde_json::Value::is_boolean)
                {
                    return Err(Error::Shape.into());
                }
                Outcome::Pass
            } else {
                Outcome::Fail {
                    detail: crate::inputs::bounded_query_diagnostic(&String::from_utf8_lossy(
                        &output.stderr,
                    )),
                }
            }
        };
        Ok(Observed::execution(target, outcome, Vec::new()))
    }
}

#[derive(Debug, Display, Error)]
pub enum Error {
    /// effect endpoint `{path}` must be an absolute normalized UTF-8 Unix socket path without URL delimiters or control characters
    Endpoint { path: PathBuf },
    /// supported engine-info provider requires an absolute podman client executable; provision it and retry `loom gate verify`
    Executable,
    /// supported engine-info executable changed; reconstruct its checked definition and retry `loom gate verify`
    Changed,
    /// supported engine-info effect I/O failed
    Io(#[source] std::io::Error),
    /// supported engine-info returned malformed JSON, not a successful capability observation
    Response(#[source] serde_json::Error),
    /// supported engine-info response omitted host OS/architecture/kernel, rootless status, or storage identity; repair the engine response and retry `loom gate verify`
    Shape,
}

//! Strict candidate wire data carries no execution or admission authority.

use std::collections::BTreeSet;

use displaydoc::Display;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{Input, Responsibility};
use crate::inventory::Name;
use crate::inventory::snapshot::{Identity, ResourcePath};

/// A content identity is parsed, never an arbitrary producer-supplied token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest(blake3::Hash);

impl Digest {
    pub(super) fn of(bytes: &[u8]) -> Self {
        Self::from_hash(blake3::hash(bytes))
    }

    pub(super) const fn from_hash(hash: blake3::Hash) -> Self {
        Self(hash)
    }
}

impl TryFrom<String> for Digest {
    type Error = Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let hash = blake3::Hash::from_hex(&value).map_err(|_| Error::Digest)?;
        if hash.to_hex().as_str() != value {
            return Err(Error::Digest);
        }
        Ok(Self(hash))
    }
}

impl From<Digest> for String {
    fn from(value: Digest) -> Self {
        value.0.to_hex().to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    NixSandbox,
    ContainerEngine,
    HostKernel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Discovery {
    CollectTracked,
    ConstructedGraph,
    SupportedEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mapping {
    PerTarget,
    DerivationOutput,
    ExitStatus,
}

/// Only specifically supported effects have a candidate shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Effect {
    ContainerEngineInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub(super) target: Name,
    pub(super) snapshot: Identity,
    pub(super) definition: Digest,
    pub(super) origin: ResourcePath,
    #[serde(deserialize_with = "super::parse_set")]
    pub(super) responsibilities: BTreeSet<Responsibility>,
    pub(super) inputs: Vec<Input>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rust {
    pub(super) checker: ResourcePath,
    pub(super) artifact: Digest,
    pub(super) entry: Name,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Nix {
    pub(super) flake: ResourcePath,
    pub(super) attribute: Name,
    pub(super) system: Name,
    pub(super) graph: Digest,
    pub(super) derivation: super::nix::StorePath,
    pub(super) source: super::nix::StorePath,
    pub(super) executor: super::nix::StorePath,
    pub(super) binary: ResourcePath,
    pub(super) artifact: Digest,
}

/// Deserializing any closed variant still leaves a candidate that requires resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Candidate {
    TrackedRust {
        contract: Contract,
        execution: Rust,
    },
    HermeticNix {
        contract: Contract,
        execution: Nix,
    },
    SupportedEffect {
        contract: Contract,
        execution: super::effect::Description,
    },
}

impl Candidate {
    /// Parse descriptions only; no raw input-list or trust-flag compatibility path exists.
    ///
    /// # Errors
    /// Unknown kinds, fields, malformed identities and incompatible execution shapes fail.
    pub fn parse(json: &str) -> Result<Self, super::Error> {
        Ok(serde_json::from_str(json)?)
    }

    const fn contract(&self) -> &Contract {
        match self {
            Self::TrackedRust { contract, .. }
            | Self::HermeticNix { contract, .. }
            | Self::SupportedEffect { contract, .. } => contract,
        }
    }

    pub const fn target(&self) -> &Name {
        &self.contract().target
    }

    pub const fn snapshot(&self) -> &Identity {
        &self.contract().snapshot
    }

    pub const fn definition(&self) -> &Digest {
        &self.contract().definition
    }

    pub const fn origin(&self) -> &ResourcePath {
        &self.contract().origin
    }

    pub const fn responsibilities(&self) -> &BTreeSet<Responsibility> {
        &self.contract().responsibilities
    }

    pub const fn inputs(&self) -> &[Input] {
        self.contract().inputs.as_slice()
    }

    pub const fn discovery(&self) -> Discovery {
        match self {
            Self::TrackedRust { .. } => Discovery::CollectTracked,
            Self::HermeticNix { .. } => Discovery::ConstructedGraph,
            Self::SupportedEffect { .. } => Discovery::SupportedEffect,
        }
    }

    pub const fn mapping(&self) -> Mapping {
        match self {
            Self::TrackedRust { .. } => Mapping::PerTarget,
            Self::HermeticNix { .. } => Mapping::DerivationOutput,
            Self::SupportedEffect { .. } => Mapping::ExitStatus,
        }
    }

    pub fn capabilities(&self) -> BTreeSet<Capability> {
        match self {
            Self::TrackedRust { .. } => BTreeSet::new(),
            Self::HermeticNix { .. } => BTreeSet::from([Capability::NixSandbox]),
            Self::SupportedEffect { .. } => {
                BTreeSet::from([Capability::ContainerEngine, Capability::HostKernel])
            }
        }
    }
}

#[derive(Serialize)]
pub(super) struct Query<'a> {
    pub units: Vec<&'a Candidate>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Response {
    pub units: Vec<Candidate>,
}

#[derive(Debug, Display, Error)]
pub enum Error {
    /// provider identities must be canonical 64-character lowercase BLAKE3 hashes
    Digest,
}

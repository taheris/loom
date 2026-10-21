use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use super::retention::{Bytes, Hold};
use super::{Context, Identity};
use crate::identifier::BeadId;

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub(super) version: u8,
    pub(super) bead: BeadId,
    pub(super) source: PathBuf,
    pub(super) source_identity: Identity,
    pub(super) target: PathBuf,
    pub(super) namespace_identity: Identity,
    pub(super) profile: PathBuf,
    pub(super) context: Context,
    #[serde(default)]
    pub(super) lease_identity: Option<Identity>,
    #[serde(default)]
    pub(super) target_identity: Option<Identity>,
    #[serde(default)]
    pub(super) reservation: Bytes,
    #[serde(default)]
    pub(super) created: Option<SystemTime>,
    #[serde(default)]
    pub(super) lifetime: Lifetime,
    #[serde(default)]
    pub(super) placement: Placement,
    #[serde(default)]
    pub(super) holds: BTreeSet<Hold>,
}

/// A dispatched generation remains unreconciled until its runtime proves teardown.
#[derive(Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Lifetime {
    #[default]
    Unreconciled,
    Prepared,
    Dispatched,
}

#[derive(Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Placement {
    #[default]
    Live,
    Quarantining,
    Removing,
}

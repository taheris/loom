use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{Priority, label::Label};
use crate::identifier::{BeadId, MoleculeId};

/// Dependency-relevant projection of `bd show --json`.
///
/// The workflow uses this separate shape only when validating a dependency
/// wait. Keeping it separate from [`Bead`] avoids burdening ordinary list
/// responses and fixtures with nested dependency records.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DependencySnapshot {
    status: Status,
    #[serde(default)]
    dependencies: Vec<Dependency>,
}

impl DependencySnapshot {
    /// True only while the waiting bead remains open.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.status == Status::Open
    }

    /// Stable status label for invalid-wait diagnostics.
    #[must_use]
    pub const fn status_label(&self) -> &'static str {
        self.status.as_str()
    }

    /// Active direct blockers declared through `bd dep add`.
    #[must_use]
    pub fn active_blockers(&self) -> Vec<BeadId> {
        self.dependencies
            .iter()
            .filter(|dependency| {
                dependency.kind == DependencyKind::Blocks && dependency.status != Status::Closed
            })
            .map(|dependency| dependency.id.clone())
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct Dependency {
    id: BeadId,
    status: Status,
    #[serde(rename = "dependency_type")]
    kind: DependencyKind,
}

/// Recognized external statuses; unknown values are rejected with their wire name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Open,
    InProgress,
    Blocked,
    Deferred,
    Closed,
    Pinned,
    Tombstone,
}

impl Status {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::InProgress => "in_progress",
            Self::Blocked => "blocked",
            Self::Deferred => "deferred",
            Self::Closed => "closed",
            Self::Pinned => "pinned",
            Self::Tombstone => "tombstone",
        }
    }
}

impl std::str::FromStr for Status {
    type Err = serde::de::value::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::deserialize(serde::de::value::StrDeserializer::new(value))
    }
}
impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Supported issue classes; unrecognized external types are rejected.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueType {
    #[default]
    Task,
    Bug,
    Feature,
    Epic,
    Chore,
}
impl IssueType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Bug => "bug",
            Self::Feature => "feature",
            Self::Epic => "epic",
            Self::Chore => "chore",
        }
    }
}
impl std::str::FromStr for IssueType {
    type Err = serde::de::value::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::deserialize(serde::de::value::StrDeserializer::new(value))
    }
}
impl std::fmt::Display for IssueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
enum DependencyKind {
    #[serde(rename = "blocks")]
    Blocks,
    #[serde(rename = "parent-child")]
    ParentChild,
    #[serde(other)]
    Other,
}

/// One bead as produced by `bd show --json` and `bd list --json`.
///
/// `bd` emits more fields than these (timestamps, owner, dependency lists);
/// they are intentionally not modelled here. `serde` ignores unknown fields
/// by default, so the wrapper does not break when `bd` adds new keys. Add
/// fields when a caller needs them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bead {
    pub id: BeadId,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub status: Status,
    #[serde(default)]
    pub priority: Priority,
    #[serde(default, rename = "issue_type")]
    pub issue_type: IssueType,
    #[serde(default)]
    pub labels: Vec<Label>,
    /// Parent bead id from `bd show --json`'s `parent` field. For a bead
    /// bonded to a molecule the parent is the molecule id; `None` means
    /// the bead is unbonded — fix-up spawning refuses unbonded origins
    /// per `specs/harness.md` §"Verdict gate · Fix-up beads bond to
    /// the originating molecule".
    #[serde(default)]
    pub parent: Option<BeadId>,
    /// Free-form metadata blob from `bd show --json`'s `metadata` object.
    /// `bd list --json` omits the field; absent ⇒ empty map. Keys used by
    /// loom include `loom.base_commit` (the molecule's diff anchor —
    /// `specs/harness.md` § *Plan creates the molecule*).
    #[serde(default)]
    pub metadata: BTreeMap<String, serde_json::Value>,
    /// `bd update --notes "..."` body. Used by the reviewer to carry the
    /// `## Options` block for promoted `loom:blocked` → `loom:clarify`
    /// beads (`specs/gate.md` § *Options Format Contract*) and by
    /// the verdict gate to record `infra-preflight` / `infra-repeated`
    /// causes. Absent ⇒ `None`.
    #[serde(default)]
    pub notes: Option<String>,
}

/// One molecule row. Beads exposes `bd mol show --json`; the shape is the
/// same epic-shaped record as a bead with extra molecule metadata, so the
/// wrapper currently surfaces only the always-present fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Molecule {
    pub id: MoleculeId,
    pub title: String,
    pub status: Status,
}

/// Output of `bd mol progress <id> --json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MolProgress {
    pub molecule_id: MoleculeId,
    #[serde(default)]
    pub molecule_title: String,
    pub completed: u32,
    pub in_progress: u32,
    pub total: u32,
    #[serde(default)]
    pub percent: f64,
    #[serde(default)]
    pub current_step_id: Option<String>,
}

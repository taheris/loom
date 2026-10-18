use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{Priority, label::Label};
use loom_events::identifier::{BeadId, MoleculeId};

/// Task status and explicit direct dependencies from one observation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DependencySnapshot {
    pub status: Status,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
}

impl DependencySnapshot {
    /// True only while the waiting bead remains open.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.status == Status::Open
    }

    /// Current native status, independent of the dependency collection.
    #[must_use]
    pub const fn status(&self) -> Status {
        self.status
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
pub struct Dependency {
    pub id: BeadId,
    pub status: Status,
    #[serde(rename = "dependency_type")]
    pub kind: DependencyKind,
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
    Decision,
    Spike,
    Story,
    Milestone,
    Message,
    Molecule,
    Gate,
    Event,
}
impl IssueType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Bug => "bug",
            Self::Feature => "feature",
            Self::Epic => "epic",
            Self::Chore => "chore",
            Self::Decision => "decision",
            Self::Spike => "spike",
            Self::Story => "story",
            Self::Milestone => "milestone",
            Self::Message => "message",
            Self::Molecule => "molecule",
            Self::Gate => "gate",
            Self::Event => "event",
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
pub enum DependencyKind {
    #[serde(rename = "blocks")]
    Blocks,
    #[serde(rename = "parent-child")]
    ParentChild,
    #[serde(rename = "discovered-from")]
    DiscoveredFrom,
}

/// Row from a native `dep list --direction up --type blocks` query. The
/// filtered edge kind is still checked rather than trusting CLI filtering.
#[derive(Debug, Deserialize)]
pub struct BlockingDependent {
    pub id: BeadId,
    #[serde(rename = "dependency_type")]
    _kind: BlockingKind,
}

#[derive(Debug, Deserialize)]
enum BlockingKind {
    #[serde(rename = "blocks")]
    Blocks,
}

/// Task content and tracking state; metadata does not convey workflow authority.
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
    /// Explicit parent identity, never inferred from identifier spelling.
    #[serde(default)]
    pub parent: Option<BeadId>,
    /// Backend-provided metadata; absent projections contain an empty map.
    #[serde(default)]
    pub metadata: BTreeMap<String, serde_json::Value>,
    /// Optional free-form notes.
    #[serde(default)]
    pub notes: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{IssueType, MolProgress};

    #[test]
    fn progress_parses_empty_current_step_as_absence_but_rejects_invalid_ids() {
        for step in [
            serde_json::Value::Null,
            serde_json::json!(""),
            serde_json::json!("lm-task.1"),
        ] {
            let progress: MolProgress = serde_json::from_value(serde_json::json!({
                "molecule_id": "lm-group", "completed": 0, "in_progress": 0, "total": 1,
                "current_step_id": step,
            }))
            .unwrap();
            assert_eq!(progress.current_step_id.is_some(), step == "lm-task.1");
        }
        assert!(
            serde_json::from_value::<MolProgress>(serde_json::json!({
                "molecule_id": "lm-group", "completed": 0, "in_progress": 0, "total": 1,
                "current_step_id": "invalid identity",
            }))
            .is_err()
        );
    }

    #[test]
    fn built_in_issue_types_round_trip() {
        for name in [
            "task",
            "bug",
            "feature",
            "epic",
            "chore",
            "decision",
            "spike",
            "story",
            "milestone",
            "message",
            "molecule",
            "gate",
            "event",
        ] {
            let kind: IssueType = name.parse().expect("built-in type");
            assert_eq!(kind.as_str(), name);
            let json = format!("\"{name}\"");
            assert_eq!(
                serde_json::from_str::<IssueType>(&json).expect("JSON type"),
                kind
            );
            assert_eq!(serde_json::to_string(&kind).expect("serialize type"), json);
        }
        assert!("invented".parse::<IssueType>().is_err());
    }
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
    #[serde(default, deserialize_with = "optional_step")]
    pub current_step_id: Option<BeadId>,
}

fn optional_step<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<BeadId>, D::Error> {
    match Option::<String>::deserialize(deserializer)? {
        None => Ok(None),
        Some(value) if value.is_empty() => Ok(None),
        Some(value) => BeadId::new(&value)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

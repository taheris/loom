use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use displaydoc::Display;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::checker::CheckerId;
use crate::config::{EvidenceConfig, SelectionFraction};
use crate::target::Target;

/// Evidence root kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    Workspace,
    External,
}

impl fmt::Display for RootKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Workspace => f.write_str("workspace"),
            Self::External => f.write_str("external"),
        }
    }
}

/// One evidence root printed before harvesting.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Root {
    pub kind: RootKind,
    pub path: PathBuf,
}

/// Deterministic evidence root report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootReport {
    roots: Vec<Root>,
}

impl RootReport {
    pub fn from_config(workspace: impl AsRef<Path>, config: &EvidenceConfig) -> Self {
        let workspace = workspace.as_ref().to_path_buf();
        let external_roots = config
            .external_roots
            .iter()
            .filter(|path| *path != &workspace)
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut roots = vec![Root {
            kind: RootKind::Workspace,
            path: workspace,
        }];
        roots.extend(external_roots.into_iter().map(|path| Root {
            kind: RootKind::External,
            path,
        }));
        Self { roots }
    }

    pub fn roots(&self) -> &[Root] {
        &self.roots
    }

    pub fn lines(&self) -> Vec<String> {
        self.roots
            .iter()
            .map(|root| format!("{}: {}", root.kind, root.path.display()))
            .collect()
    }
}

/// Stable evidence item id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ItemId(String);

impl ItemId {
    pub fn new(value: impl Into<String>) -> Result<Self, ParseItemIdError> {
        value.into().parse()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ItemId {
    type Err = ParseItemIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() {
            Err(ParseItemIdError::Empty)
        } else {
            Ok(Self(value.to_owned()))
        }
    }
}

impl<'de> Deserialize<'de> for ItemId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// Evidence item id parse failures.
#[derive(Debug, Clone, PartialEq, Eq, Display, Error)]
pub enum ParseItemIdError {
    /// evidence item id is empty
    Empty,
}

/// Mined evidence item available to checker planning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: ItemId,
    pub checker: CheckerId,
    pub targets: Vec<Target>,
}

impl Item {
    pub fn new(id: ItemId, checker: CheckerId, targets: Vec<Target>) -> Self {
        Self {
            id,
            checker,
            targets,
        }
    }
}

/// Mined evidence split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Train,
    Selection,
}

/// Stable split metadata recorded in reports and manifests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SplitMetadata {
    pub algorithm: String,
    pub salt_id: String,
    pub selection_fraction: SelectionFraction,
}

/// Stable train/selection splitter.
#[derive(Debug, Clone, PartialEq)]
pub struct Splitter {
    salt_id: String,
    salt_material: Vec<u8>,
    selection_fraction: SelectionFraction,
}

impl Splitter {
    pub fn new(
        salt_id: impl Into<String>,
        salt_material: impl AsRef<[u8]>,
        selection_fraction: SelectionFraction,
    ) -> Result<Self, SplitError> {
        let salt_id = salt_id.into();
        if salt_id.is_empty() {
            return Err(SplitError::EmptySaltId);
        }
        Ok(Self {
            salt_id,
            salt_material: salt_material.as_ref().to_vec(),
            selection_fraction,
        })
    }

    pub fn metadata(&self) -> SplitMetadata {
        SplitMetadata {
            algorithm: "sha256-salt-v1".to_owned(),
            salt_id: self.salt_id.clone(),
            selection_fraction: self.selection_fraction,
        }
    }

    pub fn assign(&self, item_id: &ItemId) -> Split {
        let mut hasher = Sha256::new();
        hasher.update(&self.salt_material);
        hasher.update(item_id.as_str().as_bytes());
        let digest = hasher.finalize();
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(&digest[..8]);
        let bucket = u64::from_be_bytes(bytes);
        let unit = (bucket as f64) / ((u64::MAX as f64) + 1.0);
        if unit < self.selection_fraction.get() {
            Split::Selection
        } else {
            Split::Train
        }
    }

    pub fn snapshot(&self, items: impl IntoIterator<Item = Item>) -> Snapshot {
        let mut train = Vec::new();
        let mut selection = Vec::new();
        for item in items {
            match self.assign(&item.id) {
                Split::Train => train.push(item),
                Split::Selection => selection.push(item),
            }
        }
        train.sort_by(|left, right| left.id.cmp(&right.id));
        selection.sort_by(|left, right| left.id.cmp(&right.id));
        Snapshot {
            train,
            selection,
            metadata: self.metadata(),
        }
    }
}

/// Evidence split construction failures.
#[derive(Debug, Clone, PartialEq, Eq, Display, Error)]
pub enum SplitError {
    /// evidence split salt id is empty
    EmptySaltId,
}

/// Mined evidence partition used by checker planning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub train: Vec<Item>,
    pub selection: Vec<Item>,
    pub metadata: SplitMetadata,
}

impl Snapshot {
    pub fn empty(metadata: SplitMetadata) -> Self {
        Self {
            train: Vec::new(),
            selection: Vec::new(),
            metadata,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_roots_report_workspace_first_and_only_explicit_external_roots() {
        let config = EvidenceConfig {
            external_roots: vec![PathBuf::from("/tmp/z"), PathBuf::from("/tmp/a")],
            ..EvidenceConfig::default()
        };
        let report = RootReport::from_config("/workspace", &config);
        assert_eq!(
            report.lines(),
            vec![
                "workspace: /workspace".to_owned(),
                "external: /tmp/a".to_owned(),
                "external: /tmp/z".to_owned(),
            ]
        );
        assert!(!report.lines().iter().any(|line| line.contains(".claude")));
        assert!(!report.lines().iter().any(|line| line.contains(".codex")));
    }

    #[test]
    fn evidence_split_is_stable_and_seed_independent() {
        let fraction = SelectionFraction::new(0.34).expect("fraction");
        let splitter = Splitter::new("repo", b"stable workspace", fraction).expect("splitter");
        let id = ItemId::new("logs/review.jsonl#7").expect("item id");
        let first = splitter.assign(&id);
        let second = splitter.assign(&id);
        assert_eq!(first, second);
        assert_eq!(splitter.metadata().algorithm, "sha256-salt-v1");
        assert_eq!(splitter.metadata().salt_id, "repo");
    }
}

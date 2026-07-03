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

/// Opaque stable split salt owned by evidence mining.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitSalt {
    id: String,
    material: [u8; 32],
}

impl SplitSalt {
    /// Derive an opaque salt from stable repository identity components.
    pub fn repository<I, S>(origin_url: Option<&str>, root_commits: I) -> Result<Self, SplitError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let roots = root_commits
            .into_iter()
            .map(|root| root.as_ref().to_owned())
            .collect::<BTreeSet<_>>();
        if roots.is_empty() {
            return Err(SplitError::EmptyRepositoryIdentity);
        }

        let mut identity = Vec::new();
        push_salt_field(
            &mut identity,
            "version",
            "loom-tune-repository-split-salt-v1",
        );
        match origin_component(origin_url) {
            OriginComponent::Remote(url) => {
                push_salt_field(&mut identity, "origin-kind", "remote");
                push_salt_field(&mut identity, "origin-url", url);
            }
            OriginComponent::Local => push_salt_field(&mut identity, "origin-kind", "local"),
            OriginComponent::Absent => push_salt_field(&mut identity, "origin-kind", "absent"),
        }
        for root in roots {
            push_salt_field(&mut identity, "root", &root);
        }

        let digest = Sha256::digest(identity);
        let mut material = [0_u8; 32];
        material.copy_from_slice(&digest);
        let id = format!("repo-sha256-v1:{}", hex_lower(&material));
        Ok(Self { id, material })
    }

    /// Opaque salt identifier safe for reports and manifests.
    pub fn id(&self) -> &str {
        &self.id
    }

    fn material(&self) -> &[u8] {
        &self.material
    }
}

/// Stable train/selection splitter.
#[derive(Debug, Clone, PartialEq)]
pub struct Splitter {
    salt: SplitSalt,
    selection_fraction: SelectionFraction,
}

impl Splitter {
    pub fn new(salt: SplitSalt, selection_fraction: SelectionFraction) -> Self {
        Self {
            salt,
            selection_fraction,
        }
    }

    pub fn metadata(&self) -> SplitMetadata {
        SplitMetadata {
            algorithm: "sha256-salt-v1".to_owned(),
            salt_id: self.salt.id().to_owned(),
            selection_fraction: self.selection_fraction,
        }
    }

    pub fn assign(&self, item_id: &ItemId) -> Split {
        let mut hasher = Sha256::new();
        hasher.update(self.salt.material());
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OriginComponent<'a> {
    Remote(&'a str),
    Local,
    Absent,
}

fn origin_component(origin_url: Option<&str>) -> OriginComponent<'_> {
    let Some(url) = origin_url.filter(|url| !url.is_empty()) else {
        return OriginComponent::Absent;
    };
    if is_remote_origin(url) {
        OriginComponent::Remote(url)
    } else {
        OriginComponent::Local
    }
}

fn is_remote_origin(url: &str) -> bool {
    has_remote_scheme(url) || looks_like_scp_origin(url)
}

fn has_remote_scheme(url: &str) -> bool {
    let Some((scheme, _rest)) = url.split_once("://") else {
        return false;
    };
    !scheme.eq_ignore_ascii_case("file")
}

fn looks_like_scp_origin(url: &str) -> bool {
    let Some((user_host, _repo_path)) = url.split_once(':') else {
        return false;
    };
    user_host.contains('@') && !user_host.contains('/')
}

fn push_salt_field(material: &mut Vec<u8>, name: &str, value: &str) {
    material.extend_from_slice(name.as_bytes());
    material.push(0);
    material.extend_from_slice(value.len().to_string().as_bytes());
    material.push(0);
    material.extend_from_slice(value.as_bytes());
    material.push(0);
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

/// Evidence split construction failures.
#[derive(Debug, Clone, PartialEq, Eq, Display, Error)]
pub enum SplitError {
    /// evidence split repository identity has no root commits
    EmptyRepositoryIdentity,
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
    fn repository_split_salt_is_stable_and_opaque() {
        let root = "0123456789abcdef0123456789abcdef01234567";
        let origin = "git@example.invalid:wrix/loom.git";
        let first = SplitSalt::repository(Some(origin), [root]).expect("salt");
        let second = SplitSalt::repository(Some(origin), [root]).expect("salt");
        let changed_root =
            SplitSalt::repository(Some(origin), ["89abcdef012345670123456789abcdef01234567"])
                .expect("salt");

        assert_eq!(first, second);
        assert_ne!(first, changed_root);
        assert!(first.id().starts_with("repo-sha256-v1:"));
        assert!(!first.id().contains(origin));
        assert!(!first.id().contains(root));
    }

    #[test]
    fn repository_split_salt_ignores_local_origin_paths() {
        let root = "0123456789abcdef0123456789abcdef01234567";
        let first = SplitSalt::repository(Some("checkout-a/origin.git"), [root]).expect("salt");
        let second = SplitSalt::repository(Some("checkout-b/origin.git"), [root]).expect("salt");
        let file_url =
            SplitSalt::repository(Some("file:///checkout-c/origin.git"), [root]).expect("salt");
        let remote =
            SplitSalt::repository(Some("git@example.invalid:wrix/loom.git"), [root]).expect("salt");

        assert_eq!(first, second);
        assert_eq!(first, file_url);
        assert_ne!(first, remote);
    }

    #[test]
    fn evidence_split_is_stable_and_seed_independent() {
        let fraction = SelectionFraction::new(0.34).expect("fraction");
        let salt = SplitSalt::repository(
            Some("git@example.invalid:wrix/loom.git"),
            ["0123456789abcdef0123456789abcdef01234567"],
        )
        .expect("salt");
        let splitter = Splitter::new(salt, fraction);
        let id = ItemId::new("logs/review.jsonl#7").expect("item id");
        let first = splitter.assign(&id);
        let second = splitter.assign(&id);
        assert_eq!(first, second);
        assert_eq!(splitter.metadata().algorithm, "sha256-salt-v1");
        assert!(splitter.metadata().salt_id.starts_with("repo-sha256-v1:"));
    }
}

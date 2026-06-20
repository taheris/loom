use std::collections::BTreeMap;
use std::str::FromStr;

use loom_driver::bd::{Bead, Label};
use loom_driver::identifier::SpecLabel;

use super::options::parse_options_in;

const TUNE_LABEL: &str = "loom:tune";
const TUNE_STATE_KEY: &str = "loom.tune.state";
const TUNE_ID_KEY: &str = "loom.tune.id";
const TUNE_BRANCH_KEY: &str = "loom.tune.proposal_branch";
const TUNE_HEAD_KEY: &str = "loom.tune.proposal_head";
const TUNE_BASE_KEY: &str = "loom.tune.base_commit";

/// Which human-decision flow an item belongs to in the inbox queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxKind {
    Clarify,
    Blocked,
    Tune,
}

impl InboxKind {
    /// Short tag printed alongside each row.
    pub fn tag(self) -> &'static str {
        match self {
            InboxKind::Clarify => "clarify",
            InboxKind::Blocked => "blocked",
            InboxKind::Tune => "tune",
        }
    }

    fn rank(self) -> u8 {
        match self {
            InboxKind::Clarify => 0,
            InboxKind::Blocked => 1,
            InboxKind::Tune => 2,
        }
    }
}

impl FromStr for InboxKind {
    type Err = InboxKindParseError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "clarify" => Ok(Self::Clarify),
            "blocked" => Ok(Self::Blocked),
            "tune" => Ok(Self::Tune),
            other => Err(InboxKindParseError {
                value: other.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxKindParseError {
    pub value: String,
}

impl std::fmt::Display for InboxKindParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown inbox kind `{}` (expected clarify, blocked, or tune)",
            self.value
        )
    }
}

impl std::error::Error for InboxKindParseError {}

/// A tune proposal's durable bead state plus local artifact paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuneInfo {
    pub proposal_id: String,
    pub state: String,
    pub proposal_branch: Option<String>,
    pub proposal_head: Option<String>,
    pub base_commit: Option<String>,
}

/// One filtered inbox item. The item owns the bead snapshot returned by bd so
/// host-side view and chat rendering consume one stable queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxItem {
    pub index: u32,
    pub bead: Bead,
    pub kind: InboxKind,
    pub spec: Option<SpecLabel>,
    pub summary: String,
    pub tune: Option<TuneInfo>,
}

impl InboxItem {
    pub fn durable_id(&self) -> &str {
        match &self.tune {
            Some(tune) => &tune.proposal_id,
            None => self.bead.id.as_str(),
        }
    }
}

/// One row of the rendered inbox table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxRow {
    pub index: u32,
    pub id: String,
    pub spec: Option<String>,
    pub summary: String,
    pub kind: InboxKind,
    pub status: String,
}

/// Classify a bead's inbox membership without applying status/filter rules.
/// Tune wins over generic blocked labels so corrupt tune proposals remain tune
/// items instead of falling into the blocked-bead queue.
pub fn kind_of(bead: &Bead) -> Option<InboxKind> {
    if is_tune_bead(bead) {
        Some(InboxKind::Tune)
    } else if bead.labels.iter().any(Label::is_blocked) {
        Some(InboxKind::Blocked)
    } else if bead.labels.iter().any(Label::is_clarify) {
        Some(InboxKind::Clarify)
    } else {
        None
    }
}

/// Build the visible inbox queue. Filters narrow before positional numbering;
/// ordering is group-first (`clarify`, `blocked`, `tune`) and FIFO within each
/// group according to the input bd order.
pub fn build_queue(
    beads: &[Bead],
    spec: Option<&SpecLabel>,
    kind: Option<InboxKind>,
    include_epics: bool,
) -> Vec<InboxItem> {
    let mut candidates: Vec<(usize, InboxItem)> = beads
        .iter()
        .enumerate()
        .filter_map(|(pos, bead)| build_candidate(pos, bead, spec, kind, include_epics))
        .collect();
    candidates.sort_by_key(|(pos, item)| (item.kind.rank(), *pos));
    candidates
        .into_iter()
        .enumerate()
        .map(|(idx, (_, mut item))| {
            item.index = u32::try_from(idx + 1).unwrap_or(u32::MAX);
            item
        })
        .collect()
}

pub fn build_rows(items: &[InboxItem], spec_filter: Option<&SpecLabel>) -> Vec<InboxRow> {
    items
        .iter()
        .map(|item| InboxRow {
            index: item.index,
            id: item.durable_id().to_owned(),
            spec: if spec_filter.is_some() {
                None
            } else {
                Some(
                    item.spec
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "—".to_string()),
                )
            },
            summary: item.summary.clone(),
            kind: item.kind,
            status: item.bead.status.clone(),
        })
        .collect()
}

/// Extract the `spec:<label>` value from a bead's labels.
pub fn spec_label_of(bead: &Bead) -> Option<SpecLabel> {
    bead.labels.iter().find_map(Label::spec_label)
}

pub fn find_by_bead_id<'a>(items: &'a [InboxItem], id: &str) -> Option<&'a InboxItem> {
    items.iter().find(|item| item.bead.id.as_str() == id)
}

pub fn find_by_proposal_id<'a>(items: &'a [InboxItem], id: &str) -> Option<&'a InboxItem> {
    items.iter().find(|item| {
        item.tune
            .as_ref()
            .is_some_and(|tune| tune.proposal_id == id)
    })
}

pub fn find_by_index(items: &[InboxItem], index: u32) -> Option<&InboxItem> {
    if index == 0 {
        return None;
    }
    let idx = usize::try_from(index - 1).ok()?;
    items.get(idx)
}

fn build_candidate(
    pos: usize,
    bead: &Bead,
    spec: Option<&SpecLabel>,
    kind: Option<InboxKind>,
    include_epics: bool,
) -> Option<(usize, InboxItem)> {
    if is_closed(bead) || (!include_epics && bead.issue_type == "epic") {
        return None;
    }
    let classified = classify_visible(bead)?;
    if kind.is_some_and(|want| want != classified.kind) || !matches_spec(bead, spec) {
        return None;
    }
    Some((
        pos,
        InboxItem {
            index: 0,
            bead: bead.clone(),
            kind: classified.kind,
            spec: spec_label_of(bead).or_else(|| metadata_spec_label(&bead.metadata)),
            summary: summary_for(bead),
            tune: classified.tune,
        },
    ))
}

struct Classified {
    kind: InboxKind,
    tune: Option<TuneInfo>,
}

fn classify_visible(bead: &Bead) -> Option<Classified> {
    if is_tune_bead(bead) {
        return tune_info(bead).map(|tune| Classified {
            kind: InboxKind::Tune,
            tune: Some(tune),
        });
    }
    if bead.labels.iter().any(Label::is_clarify) {
        return Some(Classified {
            kind: InboxKind::Clarify,
            tune: None,
        });
    }
    if bead.labels.iter().any(Label::is_blocked) {
        return Some(Classified {
            kind: InboxKind::Blocked,
            tune: None,
        });
    }
    None
}

fn is_closed(bead: &Bead) -> bool {
    bead.status == "closed"
}

fn is_tune_bead(bead: &Bead) -> bool {
    bead.labels.iter().any(|label| label.as_str() == TUNE_LABEL)
        || bead
            .metadata
            .keys()
            .any(|key| key.starts_with("loom.tune."))
}

fn tune_info(bead: &Bead) -> Option<TuneInfo> {
    let state = metadata_string(&bead.metadata, TUNE_STATE_KEY).unwrap_or_else(|| {
        if bead.status == "blocked" {
            "blocked".to_owned()
        } else {
            "pending".to_owned()
        }
    });
    if !matches!(state.as_str(), "pending" | "blocked" | "apply_failed") {
        return None;
    }
    Some(TuneInfo {
        proposal_id: metadata_string(&bead.metadata, TUNE_ID_KEY)
            .unwrap_or_else(|| bead.id.to_string()),
        state,
        proposal_branch: metadata_string(&bead.metadata, TUNE_BRANCH_KEY),
        proposal_head: metadata_string(&bead.metadata, TUNE_HEAD_KEY),
        base_commit: metadata_string(&bead.metadata, TUNE_BASE_KEY),
    })
}

fn matches_spec(bead: &Bead, spec: Option<&SpecLabel>) -> bool {
    let Some(spec) = spec else {
        return true;
    };
    bead.labels
        .iter()
        .any(|label| label.spec_label().as_ref() == Some(spec))
        || metadata_specs(&bead.metadata)
            .iter()
            .any(|label| label == spec.as_str())
}

fn metadata_spec_label(metadata: &BTreeMap<String, serde_json::Value>) -> Option<SpecLabel> {
    metadata_specs(metadata)
        .into_iter()
        .next()
        .map(SpecLabel::new)
}

fn metadata_specs(metadata: &BTreeMap<String, serde_json::Value>) -> Vec<String> {
    let mut specs = Vec::new();
    for key in ["loom.tune.spec", "loom.tune.spec_label"] {
        if let Some(value) = metadata_string(metadata, key) {
            specs.push(value);
        }
    }
    if let Some(serde_json::Value::Array(values)) = metadata.get("loom.tune.specs") {
        for value in values {
            if let Some(s) = value.as_str() {
                specs.push(s.to_owned());
            }
        }
    }
    specs
}

fn metadata_string(metadata: &BTreeMap<String, serde_json::Value>, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn summary_for(bead: &Bead) -> String {
    let parsed = parse_options_in(bead.notes.as_deref(), &bead.description);
    if parsed.summary.is_empty() {
        bead.title.clone()
    } else {
        parsed.summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::identifier::BeadId;
    use serde_json::json;

    fn bead(id: &str, title: &str, desc: &str, labels: &[&str]) -> Bead {
        Bead {
            id: BeadId::new(id).expect("valid bead id"),
            title: title.into(),
            description: desc.into(),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            labels: labels.iter().map(|s| Label::new(*s)).collect(),
            parent: None,
            metadata: Default::default(),
            notes: None,
        }
    }

    #[test]
    fn queue_groups_by_kind_then_input_order() {
        let mut tune = bead("lm-4", "tune", "", &["loom:tune", "spec:skills"]);
        tune.metadata
            .insert(TUNE_STATE_KEY.into(), json!("pending"));
        let beads = vec![
            bead("lm-1", "blocked old", "", &["loom:blocked"]),
            bead("lm-2", "clarify old", "", &["loom:clarify"]),
            tune,
            bead("lm-3", "clarify new", "", &["loom:clarify"]),
        ];
        let queue = build_queue(&beads, None, None, true);
        let ids: Vec<&str> = queue.iter().map(|item| item.bead.id.as_str()).collect();
        assert_eq!(ids, vec!["lm-2", "lm-3", "lm-1", "lm-4"]);
        assert_eq!(queue[0].index, 1);
        assert_eq!(queue[3].index, 4);
    }

    #[test]
    fn queue_excludes_closed_clarify_and_blocked_beads() {
        let mut closed = bead("lm-1", "closed", "", &["loom:clarify"]);
        closed.status = "closed".into();
        let open = bead("lm-2", "open", "", &["loom:blocked"]);
        let queue = build_queue(&[closed, open], None, None, true);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].bead.id, BeadId::new("lm-2").expect("valid"));
    }

    #[test]
    fn kind_filter_narrows_before_numbering() {
        let beads = vec![
            bead("lm-1", "clarify", "", &["loom:clarify"]),
            bead("lm-2", "blocked", "", &["loom:blocked"]),
        ];
        let queue = build_queue(&beads, None, Some(InboxKind::Blocked), true);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].index, 1);
        assert_eq!(queue[0].kind, InboxKind::Blocked);
    }

    #[test]
    fn spec_filter_matches_labels_and_tune_metadata() {
        let mut tune = bead("lm-3", "tune", "", &["loom:tune"]);
        tune.metadata
            .insert(TUNE_STATE_KEY.into(), json!("pending"));
        tune.metadata
            .insert("loom.tune.spec".into(), json!("skills"));
        let beads = vec![
            bead("lm-1", "harness", "", &["loom:clarify", "spec:harness"]),
            tune,
        ];
        let queue = build_queue(&beads, Some(&SpecLabel::new("skills")), None, true);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].kind, InboxKind::Tune);
        assert_eq!(queue[0].spec, Some(SpecLabel::new("skills")));
    }

    #[test]
    fn chat_queue_drops_epic_beads_but_list_queue_keeps_them() {
        let mut epic = bead("lm-epic", "epic bead", "", &["loom:blocked"]);
        epic.issue_type = "epic".into();
        let list = build_queue(&[epic.clone()], None, None, true);
        let chat = build_queue(&[epic], None, None, false);
        assert_eq!(list.len(), 1);
        assert!(chat.is_empty());
    }

    #[test]
    fn tune_with_blocked_label_remains_tune_kind() {
        let mut tune = bead("lm-tune", "tune", "", &["loom:tune", "loom:blocked"]);
        tune.status = "blocked".into();
        let queue = build_queue(&[tune], None, None, true);
        assert_eq!(queue[0].kind, InboxKind::Tune);
        assert_eq!(queue[0].tune.as_ref().expect("tune").state, "blocked");
    }

    #[test]
    fn rows_drop_spec_column_under_filter() {
        let beads = vec![bead("lm-2", "title", "", &["spec:harness", "loom:clarify"])];
        let label = SpecLabel::new("harness");
        let queue = build_queue(&beads, Some(&label), None, true);
        let rows = build_rows(&queue, Some(&label));
        assert_eq!(rows.len(), 1);
        assert!(rows[0].spec.is_none());
    }

    #[test]
    fn summary_prefers_options_header_over_title() {
        let desc = "## Options — chosen summary\n\n### Option 1 — t\nbody\n";
        let beads = vec![bead("lm-2", "fallback title", desc, &["loom:clarify"])];
        let queue = build_queue(&beads, None, None, true);
        assert_eq!(queue[0].summary, "chosen summary");
    }

    #[test]
    fn find_by_proposal_uses_metadata_id() {
        let mut tune = bead("lm-bead", "tune", "", &["loom:tune"]);
        tune.metadata
            .insert(TUNE_STATE_KEY.into(), json!("pending"));
        tune.metadata.insert(TUNE_ID_KEY.into(), json!("prop-1"));
        let queue = build_queue(&[tune], None, None, true);
        assert!(find_by_proposal_id(&queue, "prop-1").is_some());
        assert!(find_by_bead_id(&queue, "lm-bead").is_some());
    }
}

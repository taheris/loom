use loom_driver::bd::{Bead, Label};
use loom_driver::identifier::SpecLabel;

use super::options::parse_options_in;

/// Which `loom:*` flow a bead belongs to in the `loom msg` queue. Drives
/// the printed kind tag and the fast-reply mode (`Clarify` allows option
/// lookup; `Blocked` is always free-form per
/// `specs/harness.md` lines 1202-1206).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgKind {
    Clarify,
    Blocked,
}

impl MsgKind {
    /// Label removed by `-a` / `-d` after the note is written.
    pub fn label(self) -> &'static str {
        match self {
            MsgKind::Clarify => "loom:clarify",
            MsgKind::Blocked => "loom:blocked",
        }
    }

    /// Short tag printed alongside each row.
    pub fn tag(self) -> &'static str {
        match self {
            MsgKind::Clarify => "clarify",
            MsgKind::Blocked => "blocked",
        }
    }
}

/// One row of the outstanding-message list. Built from a [`Bead`] plus
/// (optionally) a spec filter that drops the SPEC column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsgRow {
    /// 1-based sequential index, ordered by bead creation. Stable only
    /// until the visible set changes.
    pub index: u32,
    pub bead_id: String,
    /// `Some(label)` in cross-spec mode; `None` when the list is filtered to
    /// a single spec (the column is dropped).
    pub spec: Option<String>,
    /// Summary from `## Options — <summary>`; falls back to the bead title
    /// when the header is absent or empty.
    pub summary: String,
    pub kind: MsgKind,
}

/// Classify a bead's `loom:*` membership. `loom:blocked` wins when both
/// labels are set so the operator sees the more severe state; in practice
/// the two labels are mutually exclusive (per the verdict gate).
pub fn kind_of(bead: &Bead) -> Option<MsgKind> {
    if bead.labels.iter().any(Label::is_blocked) {
        Some(MsgKind::Blocked)
    } else if bead.labels.iter().any(Label::is_clarify) {
        Some(MsgKind::Clarify)
    } else {
        None
    }
}

/// Filter beads to those carrying `loom:clarify` or `loom:blocked`. The
/// spec filter, when supplied, restricts the result to beads carrying
/// `spec:<label>`. Order is preserved from the input slice (the caller
/// sorts by creation time before calling).
///
/// Epic beads (`issue_type == "epic"`) are dropped: workers target leaf
/// beads, and an epic that carries `loom:blocked` would surface as a
/// non-actionable container in the queue.
pub fn filter_msg_beads<'a>(beads: &'a [Bead], spec: Option<&SpecLabel>) -> Vec<&'a Bead> {
    beads
        .iter()
        .filter(|b| kind_of(b).is_some())
        .filter(|b| b.issue_type != "epic")
        .filter(|b| match spec {
            None => true,
            Some(label) => b
                .labels
                .iter()
                .any(|l| l.spec_label().as_ref() == Some(label)),
        })
        .collect()
}

/// Build the rendered [`MsgRow`] table. `spec_filter` controls whether
/// the SPEC column is populated — `None` keeps it (cross-spec list) and
/// `Some(_)` drops it (the column would carry the same value for every row).
pub fn build_rows(beads: &[&Bead], spec_filter: Option<&SpecLabel>) -> Vec<MsgRow> {
    beads
        .iter()
        .enumerate()
        .map(|(i, bead)| {
            let parsed = parse_options_in(bead.notes.as_deref(), &bead.description);
            let summary = if parsed.summary.is_empty() {
                bead.title.clone()
            } else {
                parsed.summary
            };
            let spec = if spec_filter.is_some() {
                None
            } else {
                Some(
                    spec_label_of(bead)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "—".to_string()),
                )
            };
            let kind = kind_of(bead).unwrap_or(MsgKind::Clarify);
            MsgRow {
                index: u32::try_from(i + 1).unwrap_or(u32::MAX),
                bead_id: bead.id.to_string(),
                spec,
                summary,
                kind,
            }
        })
        .collect()
}

/// Extract the `spec:<label>` value from a bead's labels. `loom msg`'s
/// resume hint reads this on every successful clear.
pub fn spec_label_of(bead: &Bead) -> Option<SpecLabel> {
    bead.labels.iter().find_map(Label::spec_label)
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "tests use panicking helpers")]
mod tests {
    use super::*;
    use loom_driver::identifier::BeadId;

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
    fn filter_keeps_only_clarify_labelled_beads() {
        let beads = vec![
            bead("wx-1", "no clarify", "", &["spec:harness"]),
            bead(
                "wx-2",
                "with clarify",
                "",
                &["spec:harness", "loom:clarify"],
            ),
            bead(
                "wx-3",
                "other spec clarify",
                "",
                &["spec:profiles", "loom:clarify"],
            ),
        ];
        let kept = filter_msg_beads(&beads, None);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].id, BeadId::new("wx-2").expect("valid"));
        assert_eq!(kept[1].id, BeadId::new("wx-3").expect("valid"));
    }

    #[test]
    fn filter_keeps_blocked_alongside_clarify() {
        let beads = vec![
            bead("wx-1", "no msg", "", &["spec:harness"]),
            bead("wx-2", "clarify", "", &["spec:harness", "loom:clarify"]),
            bead("wx-3", "blocked", "", &["spec:harness", "loom:blocked"]),
        ];
        let kept = filter_msg_beads(&beads, None);
        assert_eq!(kept.len(), 2);
        let rows = build_rows(&kept, None);
        assert_eq!(rows[0].kind, MsgKind::Clarify);
        assert_eq!(rows[1].kind, MsgKind::Blocked);
    }

    #[test]
    fn filter_drops_epic_beads_even_when_blocked() {
        let mut epic = bead("wx-epic", "epic bead", "", &["loom:blocked"]);
        epic.issue_type = "epic".into();
        let leaf = bead("wx-leaf", "leaf bead", "", &["loom:blocked"]);
        let beads = vec![epic, leaf];
        let kept = filter_msg_beads(&beads, None);
        assert_eq!(kept.len(), 1, "epic must be filtered out");
        assert_eq!(kept[0].id, BeadId::new("wx-leaf").expect("valid"));
    }

    #[test]
    fn filter_with_spec_label_keeps_only_matching() {
        let beads = vec![
            bead("wx-2", "loom", "", &["spec:harness", "loom:clarify"]),
            bead("wx-3", "profiles", "", &["spec:profiles", "loom:clarify"]),
            bead("wx-4", "blocked", "", &["spec:profiles", "loom:blocked"]),
        ];
        let label = SpecLabel::new("harness");
        let kept = filter_msg_beads(&beads, Some(&label));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, BeadId::new("wx-2").expect("valid"));
    }

    #[test]
    fn rows_drop_spec_column_under_filter() {
        let beads = vec![bead("wx-2", "title", "", &["spec:harness", "loom:clarify"])];
        let label = SpecLabel::new("harness");
        let kept = filter_msg_beads(&beads, Some(&label));
        let rows = build_rows(&kept, Some(&label));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].index, 1);
        assert!(rows[0].spec.is_none(), "spec dropped under filter");
    }

    #[test]
    fn rows_carry_spec_column_when_unfiltered() {
        let beads = vec![bead("wx-2", "title", "", &["spec:harness", "loom:clarify"])];
        let kept = filter_msg_beads(&beads, None);
        let rows = build_rows(&kept, None);
        assert_eq!(rows[0].spec.as_deref(), Some("harness"));
    }

    #[test]
    fn summary_prefers_options_header_over_title() {
        let desc = "## Options — chosen summary\n\n### Option 1 — t\nbody\n";
        let beads = vec![bead("wx-2", "fallback title", desc, &["loom:clarify"])];
        let kept = filter_msg_beads(&beads, None);
        let rows = build_rows(&kept, None);
        assert_eq!(rows[0].summary, "chosen summary");
    }

    #[test]
    fn summary_falls_back_to_title_when_header_absent() {
        let beads = vec![bead(
            "wx-2",
            "the title",
            "no options here",
            &["loom:clarify"],
        )];
        let kept = filter_msg_beads(&beads, None);
        let rows = build_rows(&kept, None);
        assert_eq!(rows[0].summary, "the title");
    }

    #[test]
    fn missing_spec_label_renders_em_dash_in_cross_spec_view() {
        let beads = vec![bead("wx-2", "t", "", &["loom:clarify"])];
        let kept = filter_msg_beads(&beads, None);
        let rows = build_rows(&kept, None);
        assert_eq!(rows[0].spec.as_deref(), Some("—"));
    }

    #[test]
    fn kind_of_prefers_blocked_when_both_labels_present() {
        let b = bead("wx-2", "t", "", &["loom:clarify", "loom:blocked"]);
        assert_eq!(kind_of(&b), Some(MsgKind::Blocked));
    }

    #[test]
    fn msg_kind_label_and_tag_round_trip() {
        assert_eq!(MsgKind::Clarify.label(), "loom:clarify");
        assert_eq!(MsgKind::Blocked.label(), "loom:blocked");
        assert_eq!(MsgKind::Clarify.tag(), "clarify");
        assert_eq!(MsgKind::Blocked.tag(), "blocked");
    }
}

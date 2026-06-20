use std::path::Path;

use loom_templates::SkillIndexMarkdown;
use loom_templates::inbox::{
    ClarifyOption, InboxContext, InboxItem as TemplateItem, ItemKind, TuneItem,
};

use super::list::{InboxItem, InboxKind};
use super::options::parse_options_in;

/// Build the typed [`InboxContext`] consumed by the `inbox.md` Askama template.
pub fn build_inbox_context(
    workspace: &Path,
    pinned_context: String,
    companion_paths: Vec<String>,
    items: &[InboxItem],
    scratchpad_path: String,
    skill_index: SkillIndexMarkdown,
) -> InboxContext {
    InboxContext {
        pinned_context,
        companion_paths,
        inbox_items: items
            .iter()
            .map(|item| to_template_item(workspace, item))
            .collect(),
        scratchpad_path,
        skill_index,
    }
}

fn to_template_item(workspace: &Path, item: &InboxItem) -> TemplateItem {
    let parsed = parse_options_in(item.bead.notes.as_deref(), &item.bead.description);
    let options_summary = if parsed.summary.is_empty() {
        None
    } else {
        Some(parsed.summary)
    };
    let options = parsed
        .options
        .into_iter()
        .map(|opt| ClarifyOption {
            n: opt.n,
            title: option_field(opt.title),
            body: option_field(opt.body),
        })
        .collect();
    TemplateItem {
        index: item.index,
        id: item.durable_id().to_owned(),
        bead_id: item.bead.id.to_string(),
        spec_label: item
            .spec
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| "—".to_owned()),
        title: item.bead.title.clone(),
        body: item.bead.description.clone(),
        notes: item.bead.notes.clone().filter(|notes| !notes.is_empty()),
        options_summary,
        options,
        kind: match item.kind {
            InboxKind::Clarify => ItemKind::Clarify,
            InboxKind::Blocked => ItemKind::Blocked,
            InboxKind::Tune => ItemKind::Tune,
        },
        tune: item.tune.as_ref().map(|tune| {
            let envelope = workspace.join(".loom/tune").join(&tune.proposal_id);
            TuneItem {
                state: tune.state.clone(),
                proposal_branch: tune.proposal_branch.clone(),
                proposal_head: tune.proposal_head.clone(),
                base_commit: tune.base_commit.clone(),
                envelope_path: envelope.to_string_lossy().into_owned(),
                repo_path: envelope.join("repo").to_string_lossy().into_owned(),
                manifest_path: envelope
                    .join("manifest.json")
                    .to_string_lossy()
                    .into_owned(),
                evidence_path: envelope.join("evidence.md").to_string_lossy().into_owned(),
            }
        }),
    }
}

fn option_field(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::bd::{Bead, Label};
    use loom_driver::identifier::{BeadId, SpecLabel};
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
    fn rendered_inbox_template_lists_each_item() {
        let beads = [
            bead(
                "lm-2",
                "Title A",
                "## Options — sum A\n\n### Option 1 — t1\nbody1\n",
                &["spec:harness", "loom:clarify"],
            ),
            bead(
                "lm-3",
                "Title B",
                "no options",
                &["spec:profiles", "loom:blocked"],
            ),
        ];
        let items = super::super::list::build_queue(&beads, None, None, true);
        let ctx = build_inbox_context(
            Path::new("/workspace"),
            "PIN".into(),
            vec!["lib/sandbox/".into()],
            &items,
            "/workspace/.loom/scratch/inbox/scratch.md".into(),
            SkillIndexMarkdown::empty(),
        );
        let body = askama::Template::render(&ctx).expect("render");
        assert!(body.contains("lm-2"), "{body}");
        assert!(body.contains("lm-3"), "{body}");
        assert!(body.contains("spec:harness"), "{body}");
        assert!(body.contains("Title A"), "{body}");
        assert!(body.contains("sum A"), "{body}");
        assert!(body.contains("- lib/sandbox/"), "{body}");
    }

    #[test]
    fn tune_template_item_carries_artifact_paths() {
        let mut tune = bead("lm-tune", "Tune proposal", "body", &["loom:tune"]);
        tune.metadata
            .insert("loom.tune.state".into(), json!("pending"));
        tune.metadata.insert(
            "loom.tune.proposal_branch".into(),
            json!("loom/tune/lm-tune"),
        );
        let items = super::super::list::build_queue(&[tune], None, None, true);
        let item = to_template_item(Path::new("/work"), &items[0]);
        let tune = item.tune.expect("tune metadata");
        assert_eq!(tune.state, "pending");
        assert_eq!(tune.repo_path, "/work/.loom/tune/lm-tune/repo");
        assert_eq!(item.spec_label, "—");
    }

    #[test]
    fn missing_spec_label_renders_em_dash() {
        let beads = [bead("lm-2", "t", "", &["loom:clarify"])];
        let mut items = super::super::list::build_queue(&beads, None, None, true);
        items[0].spec = Some(SpecLabel::new(""));
        let rendered = to_template_item(Path::new("/work"), &items[0]);
        assert_eq!(rendered.spec_label, "");
    }
}

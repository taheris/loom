use loom_driver::bd::Bead;
use loom_driver::identifier::{BeadId, SpecLabel};
use loom_templates::SkillIndexMarkdown;
use loom_templates::msg::{BeadKind, ClarifyBead, ClarifyOption, MsgContext};

use super::list::{MsgKind, kind_of, spec_label_of};
use super::options::parse_options_in;

/// Build the typed [`MsgContext`] consumed by the `msg.md` Askama template.
///
/// The template renders one `### <id> — [spec:<label>] <title>` block per
/// outstanding clarify, plus the `## Options — <summary>` framing and
/// enumerated option subsections. The driver builds one [`ClarifyBead`]
/// per filtered bead via the Options Format Contract parser.
pub fn build_msg_context(
    pinned_context: String,
    companion_paths: Vec<String>,
    beads: &[&Bead],
    scratchpad_path: String,
    skill_index: SkillIndexMarkdown,
) -> MsgContext {
    MsgContext {
        pinned_context,
        companion_paths,
        clarify_beads: beads.iter().map(|b| to_clarify_bead(b)).collect(),
        scratchpad_path,
        skill_index,
    }
}

fn to_clarify_bead(bead: &Bead) -> ClarifyBead {
    let parsed = parse_options_in(bead.notes.as_deref(), &bead.description);
    let spec_label = spec_label_of(bead).unwrap_or_else(|| SpecLabel::new("—"));
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
    let kind = match kind_of(bead).unwrap_or(MsgKind::Clarify) {
        MsgKind::Clarify => BeadKind::Clarify,
        MsgKind::Blocked => BeadKind::Blocked,
    };
    ClarifyBead {
        id: bead.id.clone(),
        spec_label,
        title: bead.title.clone(),
        options_summary,
        options,
        kind,
    }
}

fn option_field(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

/// Resolve the target bead for `-n <N>` or `-i <id>`. Returns the `BeadId`
/// alongside the row's 1-based index (when known) so callers can quote both
/// the index and the id in user-facing output.
pub fn resolve_target<'a>(
    beads: &'a [&'a Bead],
    num: Option<u32>,
    id: Option<&str>,
) -> Result<(BeadId, Option<u32>), super::error::MsgError> {
    if num.is_some() && id.is_some() {
        return Err(super::error::MsgError::AmbiguousTarget);
    }

    let total = u32::try_from(beads.len()).unwrap_or(u32::MAX);

    if let Some(n) = num {
        if n == 0 || n > total {
            return Err(super::error::MsgError::IndexOutOfRange { index: n, total });
        }
        let idx = (n - 1) as usize;
        return Ok((beads[idx].id.clone(), Some(n)));
    }

    if let Some(target_id) = id {
        for (i, bead) in beads.iter().enumerate() {
            if bead.id.as_str() == target_id {
                let pos = u32::try_from(i + 1).ok();
                return Ok((bead.id.clone(), pos));
            }
        }
        return Err(super::error::MsgError::BeadNotFound {
            id: target_id.to_string(),
        });
    }

    Err(super::error::MsgError::TargetRequired)
}

#[cfg(test)]
mod tests {
    use super::*;
    use askama::Template;
    use loom_driver::bd::Label;

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
    fn rendered_msg_template_lists_each_clarify() {
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
                &["spec:profiles", "loom:clarify"],
            ),
        ];
        let refs: Vec<&Bead> = beads.iter().collect();
        let ctx = build_msg_context(
            "PIN".into(),
            vec!["lib/sandbox/".into()],
            &refs,
            "/workspace/.loom/scratch/msg/scratch.md".into(),
            SkillIndexMarkdown::empty(),
        );
        let body = ctx.render().expect("render");
        assert!(body.contains("lm-2"), "{body}");
        assert!(body.contains("lm-3"), "{body}");
        assert!(body.contains("spec:harness"), "{body}");
        assert!(body.contains("Title A"), "{body}");
        assert!(body.contains("sum A"), "{body}");
        assert!(body.contains("- lib/sandbox/"), "{body}");
    }

    #[test]
    fn resolve_by_index_returns_bead_id() {
        let beads = [
            bead("lm-2", "a", "", &["loom:clarify"]),
            bead("lm-3", "b", "", &["loom:clarify"]),
        ];
        let refs: Vec<&Bead> = beads.iter().collect();
        let (id, pos) = resolve_target(&refs, Some(2), None).expect("resolve");
        assert_eq!(id, BeadId::new("lm-3").expect("valid"));
        assert_eq!(pos, Some(2));
    }

    #[test]
    fn resolve_by_id_returns_index() {
        let beads = [
            bead("lm-2", "a", "", &["loom:clarify"]),
            bead("lm-3", "b", "", &["loom:clarify"]),
        ];
        let refs: Vec<&Bead> = beads.iter().collect();
        let (id, pos) = resolve_target(&refs, None, Some("lm-3")).expect("resolve");
        assert_eq!(id, BeadId::new("lm-3").expect("valid"));
        assert_eq!(pos, Some(2));
    }

    #[test]
    fn resolve_missing_id_errors() {
        let beads = [bead("lm-2", "a", "", &["loom:clarify"])];
        let refs: Vec<&Bead> = beads.iter().collect();
        let err = resolve_target(&refs, None, Some("lm-9")).expect_err("expected BeadNotFound");
        assert!(matches!(
            err,
            super::super::error::MsgError::BeadNotFound { .. }
        ));
    }

    #[test]
    fn resolve_zero_index_or_overflow_errors() {
        let beads = [bead("lm-2", "a", "", &["loom:clarify"])];
        let refs: Vec<&Bead> = beads.iter().collect();
        assert!(matches!(
            resolve_target(&refs, Some(0), None).err(),
            Some(super::super::error::MsgError::IndexOutOfRange { .. })
        ));
        assert!(matches!(
            resolve_target(&refs, Some(99), None).err(),
            Some(super::super::error::MsgError::IndexOutOfRange { .. })
        ));
    }

    #[test]
    fn ambiguous_target_when_both_supplied() {
        let beads = [bead("lm-2", "a", "", &["loom:clarify"])];
        let refs: Vec<&Bead> = beads.iter().collect();
        assert!(matches!(
            resolve_target(&refs, Some(1), Some("lm-2")).err(),
            Some(super::super::error::MsgError::AmbiguousTarget)
        ));
    }

    #[test]
    fn missing_spec_label_renders_em_dash_in_clarify_bead() {
        let b = bead("lm-2", "t", "", &["loom:clarify"]);
        let cb = to_clarify_bead(&b);
        assert_eq!(cb.spec_label.as_str(), "—");
    }
}

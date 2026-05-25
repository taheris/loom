use loom_driver::identifier::{MoleculeId, SpecLabel};
use loom_templates::todo::{TodoNewContext, TodoUpdateContext};

use super::tier::{DiffCandidate, TierDecision};

/// Tagged template context — picks the right Askama struct based on the
/// tier. The driver renders this directly.
pub enum TodoTemplateContext {
    New(TodoNewContext),
    Update(TodoUpdateContext),
}

/// Inputs every template needs, regardless of tier.
pub struct TemplateBaseFields {
    pub label: SpecLabel,
    pub spec_path: String,
    pub pinned_context: String,
    pub companion_paths: Vec<String>,
    /// Implementation notes pulled from `notes` rows where
    /// `kind = 'implementation'`, in chronological (id ascending) order.
    /// Rendered into every new bead body so each implementation agent
    /// receives the full planning context independent of external state.
    pub implementation_notes: Vec<String>,
    /// Absolute path to `.wrapix/loom/scratch/<spec-label>/scratch.md` for
    /// this todo session. Embedded in the rendered prompt so the agent can
    /// write to the correct file under compaction recovery.
    pub scratchpad_path: String,
}

/// Build the appropriate template context for a given [`TierDecision`].
///
/// - Tier 1 (Diff) → [`TodoUpdateContext`] with `spec_diff` set to the
///   formatted fan-out (each candidate prefixed with its `=== <path> ===`
///   marker).
/// - Tier 2 (Tasks) → [`TodoUpdateContext`] with `existing_tasks` set; the
///   driver supplies the task list as a pre-rendered string.
/// - Tier 4 (New) → [`TodoNewContext`].
pub fn build_template_context(
    tier: &TierDecision,
    base: TemplateBaseFields,
    existing_tasks: Option<String>,
    molecule_id: Option<MoleculeId>,
) -> TodoTemplateContext {
    let TemplateBaseFields {
        label,
        spec_path,
        pinned_context,
        companion_paths,
        implementation_notes,
        scratchpad_path,
    } = base;

    match tier {
        TierDecision::Diff { candidates, .. } => {
            let spec_diff = render_fanout_block(candidates);
            TodoTemplateContext::Update(TodoUpdateContext {
                pinned_context,
                label,
                spec_path,
                companion_paths,
                spec_diff: Some(spec_diff),
                existing_tasks: None,
                molecule_id,
                implementation_notes,
                scratchpad_path,
            })
        }
        TierDecision::Tasks { molecule } => TodoTemplateContext::Update(TodoUpdateContext {
            pinned_context,
            label,
            spec_path,
            companion_paths,
            spec_diff: None,
            existing_tasks,
            molecule_id: Some(molecule.clone()),
            implementation_notes,
            scratchpad_path,
        }),
        TierDecision::New => TodoTemplateContext::New(TodoNewContext {
            pinned_context,
            label,
            spec_path,
            companion_paths,
            implementation_notes,
            scratchpad_path,
        }),
    }
}

/// Format the per-spec fan-out: `=== <spec_path> ===` header followed by
/// the diff body, each candidate separated by a blank line.
fn render_fanout_block(candidates: &[DiffCandidate]) -> String {
    let mut out = String::new();
    for (idx, cand) in candidates.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        out.push_str("=== ");
        out.push_str(&cand.spec_path.to_string_lossy());
        out.push_str(" ===\n");
        out.push_str(&cand.diff);
        if !cand.diff.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "tests use panicking helpers"
)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn base_fields() -> TemplateBaseFields {
        TemplateBaseFields {
            label: SpecLabel::new("alpha"),
            spec_path: "specs/alpha.md".to_string(),
            pinned_context: "PIN".to_string(),
            companion_paths: vec![],
            implementation_notes: vec![],
            scratchpad_path: "/workspace/.wrapix/loom/scratch/alpha/scratch.md".to_string(),
        }
    }

    #[test]
    fn new_tier_routes_to_todo_new_context() {
        let ctx = build_template_context(&TierDecision::New, base_fields(), None, None);
        assert!(matches!(ctx, TodoTemplateContext::New(_)));
    }

    #[test]
    fn tasks_tier_routes_to_update_with_existing_tasks() {
        let mol = MoleculeId::new("wx-mol");
        let ctx = build_template_context(
            &TierDecision::Tasks {
                molecule: mol.clone(),
            },
            base_fields(),
            Some("- existing".into()),
            Some(mol.clone()),
        );
        match ctx {
            TodoTemplateContext::Update(u) => {
                assert!(u.spec_diff.is_none());
                assert_eq!(u.existing_tasks.as_deref(), Some("- existing"));
                assert_eq!(u.molecule_id, Some(mol));
            }
            _ => panic!("expected Update"),
        }
    }

    #[test]
    fn notes_thread_into_new_tier_context() {
        let mut base = base_fields();
        base.implementation_notes = vec!["note one".into(), "note two".into()];
        let ctx = build_template_context(&TierDecision::New, base, None, None);
        match ctx {
            TodoTemplateContext::New(n) => {
                assert_eq!(n.implementation_notes, vec!["note one", "note two"]);
            }
            _ => panic!("expected New"),
        }
    }

    #[test]
    fn notes_thread_into_update_tier_context() {
        let mut base = base_fields();
        base.implementation_notes = vec!["seeded note".into()];
        let ctx = build_template_context(
            &TierDecision::Tasks {
                molecule: MoleculeId::new("wx-mol"),
            },
            base,
            None,
            Some(MoleculeId::new("wx-mol")),
        );
        match ctx {
            TodoTemplateContext::Update(u) => {
                assert_eq!(u.implementation_notes, vec!["seeded note"]);
            }
            _ => panic!("expected Update"),
        }
    }

    #[test]
    fn diff_tier_renders_fanout_with_path_markers() {
        let candidates = vec![
            DiffCandidate {
                label: SpecLabel::new("alpha"),
                spec_path: PathBuf::from("specs/alpha.md"),
                effective_base: "base".into(),
                diff: "alpha diff line\n".into(),
            },
            DiffCandidate {
                label: SpecLabel::new("beta"),
                spec_path: PathBuf::from("specs/beta.md"),
                effective_base: "base".into(),
                diff: "beta diff line".into(),
            },
        ];
        let ctx = build_template_context(
            &TierDecision::Diff {
                anchor_base: "base".into(),
                candidates,
            },
            base_fields(),
            None,
            Some(MoleculeId::new("wx-mol")),
        );
        match ctx {
            TodoTemplateContext::Update(u) => {
                let diff = u.spec_diff.expect("spec_diff set");
                assert!(diff.contains("=== specs/alpha.md ==="));
                assert!(diff.contains("alpha diff line"));
                assert!(diff.contains("=== specs/beta.md ==="));
                assert!(diff.contains("beta diff line"));
            }
            _ => panic!("expected Update"),
        }
    }
}

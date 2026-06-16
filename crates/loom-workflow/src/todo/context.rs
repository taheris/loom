use loom_driver::identifier::{MoleculeId, SpecLabel};
use loom_templates::criterion_status::CriterionStatus;
use loom_templates::todo::{TodoNewContext, TodoUpdateContext};

use super::touched::{TouchedSpec, render_fanout_block};

/// Tagged template context — picks the right Askama struct based on the
/// resolver outcome. The driver renders this directly.
pub enum TodoTemplateContext {
    New(TodoNewContext),
    Update(TodoUpdateContext),
}

/// Inputs every template needs, regardless of resolver outcome.
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
    /// Absolute path to `.loom/scratch/<spec-label>/scratch.md` for
    /// this todo session. Embedded in the rendered prompt so the agent can
    /// write to the correct file under compaction recovery.
    pub scratchpad_path: String,
}

/// Build the template context for the resolver's outcome.
///
/// - `Some(molecule_id)` → [`TodoUpdateContext`] with `spec_diff` set to
///   the per-spec fan-out across every touched spec.
/// - `None` → [`TodoNewContext`].
pub fn build_template_context(
    molecule_id: Option<MoleculeId>,
    touched: &[TouchedSpec],
    base: TemplateBaseFields,
    criterion_status: Vec<CriterionStatus>,
) -> TodoTemplateContext {
    let TemplateBaseFields {
        label,
        spec_path,
        pinned_context,
        companion_paths,
        implementation_notes,
        scratchpad_path,
    } = base;

    match molecule_id {
        Some(id) => {
            let spec_diff = if touched.is_empty() {
                None
            } else {
                Some(render_fanout_block(touched))
            };
            TodoTemplateContext::Update(TodoUpdateContext {
                pinned_context,
                label,
                spec_path,
                companion_paths,
                spec_diff,
                existing_tasks: None,
                molecule_id: Some(id),
                implementation_notes,
                criterion_status,
                scratchpad_path,
            })
        }
        None => TodoTemplateContext::New(TodoNewContext {
            pinned_context,
            label,
            spec_path,
            companion_paths,
            implementation_notes,
            criterion_status,
            scratchpad_path,
        }),
    }
}

#[cfg(test)]
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
            scratchpad_path: "/workspace/.loom/scratch/alpha/scratch.md".to_string(),
        }
    }

    #[test]
    fn no_molecule_routes_to_todo_new_context() {
        let ctx = build_template_context(None, &[], base_fields(), vec![]);
        assert!(matches!(ctx, TodoTemplateContext::New(_)));
    }

    #[test]
    fn existing_molecule_without_touched_renders_update_with_no_diff() {
        let mol = MoleculeId::new("lm-mol");
        let ctx = build_template_context(Some(mol.clone()), &[], base_fields(), vec![]);
        match ctx {
            TodoTemplateContext::Update(u) => {
                assert!(u.spec_diff.is_none());
                assert_eq!(u.molecule_id, Some(mol));
            }
            _ => panic!("expected Update"),
        }
    }

    #[test]
    fn notes_thread_into_new_tier_context() {
        let mut base = base_fields();
        base.implementation_notes = vec!["note one".into(), "note two".into()];
        let ctx = build_template_context(None, &[], base, vec![]);
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
        let ctx = build_template_context(Some(MoleculeId::new("lm-mol")), &[], base, vec![]);
        match ctx {
            TodoTemplateContext::Update(u) => {
                assert_eq!(u.implementation_notes, vec!["seeded note"]);
            }
            _ => panic!("expected Update"),
        }
    }

    #[test]
    fn criterion_status_threads_into_new_tier_context() {
        use loom_templates::criterion_status::{
            AnnotationTarget, AnnotationTier, CriterionAnnotation, CriterionId, CriterionResult,
            CriterionStatus, EvidenceState,
        };
        let cs = vec![CriterionStatus {
            spec_label: SpecLabel::new("harness"),
            criterion_id: CriterionId::new("criterion-5"),
            criterion_text: "Build succeeds".into(),
            annotation: CriterionAnnotation {
                tier: AnnotationTier::Check,
                target: AnnotationTarget::new("cargo run -p w -- a"),
                pending: false,
            },
            evidence: EvidenceState::Current {
                result: CriterionResult::Pass,
                last_timestamp_ms: 42,
                last_commit: "deadbeef".into(),
                commits_since: 0,
            },
        }];
        let ctx = build_template_context(None, &[], base_fields(), cs.clone());
        match ctx {
            TodoTemplateContext::New(n) => assert_eq!(n.criterion_status, cs),
            _ => panic!("expected New"),
        }
    }

    #[test]
    fn criterion_status_threads_into_update_tier_context() {
        use loom_templates::criterion_status::{
            AnnotationTarget, AnnotationTier, CriterionAnnotation, CriterionId, CriterionStatus,
            EvidenceState,
        };
        let cs = vec![CriterionStatus {
            spec_label: SpecLabel::new("harness"),
            criterion_id: CriterionId::new("criterion-9"),
            criterion_text: "Test succeeds".into(),
            annotation: CriterionAnnotation {
                tier: AnnotationTier::Test,
                target: AnnotationTarget::new("crate::t::b"),
                pending: false,
            },
            evidence: EvidenceState::Missing,
        }];
        let ctx = build_template_context(
            Some(MoleculeId::new("lm-mol")),
            &[],
            base_fields(),
            cs.clone(),
        );
        match ctx {
            TodoTemplateContext::Update(u) => assert_eq!(u.criterion_status, cs),
            _ => panic!("expected Update"),
        }
    }

    #[test]
    fn touched_specs_render_fanout_with_path_markers() {
        let touched = vec![
            TouchedSpec {
                label: SpecLabel::new("alpha"),
                spec_path: PathBuf::from("specs/alpha.md"),
                diff: "alpha diff line\n".into(),
            },
            TouchedSpec {
                label: SpecLabel::new("beta"),
                spec_path: PathBuf::from("specs/beta.md"),
                diff: "beta diff line".into(),
            },
        ];
        let ctx = build_template_context(
            Some(MoleculeId::new("lm-mol")),
            &touched,
            base_fields(),
            vec![],
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

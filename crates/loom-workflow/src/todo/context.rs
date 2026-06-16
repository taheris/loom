use loom_driver::identifier::{BeadId, MoleculeId, SpecLabel};
use loom_protocol::todo::{GitSha, TodoFingerprint};
use loom_templates::criterion_status::CriterionStatus;
use loom_templates::todo::{
    SpecEpicContext, SpecImplementationNotes, TodoChangedSpec, TodoContext,
};

use super::touched::TouchedSpec;

/// Inputs the unified todo template needs from deterministic preflight.
pub struct TemplateBaseFields {
    pub pinned_context: String,
    pub spec_index: String,
    pub changed_specs: Vec<TodoChangedSpec>,
    pub work_epic: BeadId,
    pub todo_head: GitSha,
    pub todo_fingerprint: TodoFingerprint,
    pub spec_epics: Vec<SpecEpicContext>,
    pub companion_paths: Vec<String>,
    pub implementation_notes: Vec<SpecImplementationNotes>,
    pub scratchpad_path: String,
}

/// Build the unified todo template context.
pub fn build_template_context(
    base: TemplateBaseFields,
    criterion_status: Vec<CriterionStatus>,
) -> TodoContext {
    TodoContext {
        pinned_context: base.pinned_context,
        spec_index: base.spec_index,
        changed_specs: base.changed_specs,
        work_epic: base.work_epic,
        todo_head: base.todo_head,
        todo_fingerprint: base.todo_fingerprint,
        spec_epics: base.spec_epics,
        companion_paths: base.companion_paths,
        implementation_notes: base.implementation_notes,
        criterion_status,
        scratchpad_path: base.scratchpad_path,
    }
}

pub fn changed_specs_from_touched(
    anchor: &SpecLabel,
    touched: &[TouchedSpec],
) -> Vec<TodoChangedSpec> {
    if touched.is_empty() {
        return vec![TodoChangedSpec {
            label: anchor.clone(),
            spec_path: format!("specs/{anchor}.md"),
            diff: None,
        }];
    }

    touched
        .iter()
        .map(|spec| TodoChangedSpec {
            label: spec.label.clone(),
            spec_path: spec.spec_path.to_string_lossy().into_owned(),
            diff: Some(spec.diff.clone()),
        })
        .collect()
}

pub fn spec_epic_context(
    label: SpecLabel,
    epic_id: Option<MoleculeId>,
    todo_cursor: Option<String>,
) -> SpecEpicContext {
    SpecEpicContext {
        label,
        epic_id,
        todo_cursor,
    }
}

pub fn implementation_notes_context(
    label: SpecLabel,
    notes: Vec<String>,
) -> SpecImplementationNotes {
    SpecImplementationNotes { label, notes }
}

pub fn todo_fingerprint(
    todo_head: &GitSha,
    work_epic: &BeadId,
    changed_specs: &[TodoChangedSpec],
) -> TodoFingerprint {
    let mut canonical = String::new();
    canonical.push_str(todo_head.as_str());
    canonical.push('\0');
    canonical.push_str(work_epic.as_str());
    for spec in changed_specs {
        canonical.push('\0');
        canonical.push_str(spec.label.as_str());
        canonical.push('\0');
        canonical.push_str(&spec.spec_path);
    }
    let digest = blake3::hash(canonical.as_bytes()).to_hex().to_string();
    match TodoFingerprint::new(&digest) {
        Ok(fingerprint) => fingerprint,
        Err(err) => {
            tracing::error!(target: "loom::bug", error = %err, "blake3 todo fingerprint rejected");
            std::process::abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn base_fields() -> TemplateBaseFields {
        let work_epic = BeadId::new("lm-work").expect("valid bead id");
        let todo_head = GitSha::new(SHA).expect("valid git sha");
        let changed_specs = vec![TodoChangedSpec {
            label: SpecLabel::new("alpha"),
            spec_path: "specs/alpha.md".to_string(),
            diff: None,
        }];
        let todo_fingerprint = todo_fingerprint(&todo_head, &work_epic, &changed_specs);
        TemplateBaseFields {
            pinned_context: "PIN".to_string(),
            spec_index: "INDEX".to_string(),
            changed_specs,
            work_epic,
            todo_head,
            todo_fingerprint,
            spec_epics: vec![],
            companion_paths: vec![],
            implementation_notes: vec![],
            scratchpad_path: "/workspace/.loom/scratch/todo/scratch.md".to_string(),
        }
    }

    #[test]
    fn build_context_returns_unified_todo_context() {
        let ctx = build_template_context(base_fields(), vec![]);
        assert_eq!(ctx.changed_specs[0].label, SpecLabel::new("alpha"));
        assert_eq!(ctx.work_epic.as_str(), "lm-work");
    }

    #[test]
    fn notes_thread_into_unified_context() {
        let mut base = base_fields();
        base.implementation_notes = vec![implementation_notes_context(
            SpecLabel::new("alpha"),
            vec!["note one".into(), "note two".into()],
        )];
        let ctx = build_template_context(base, vec![]);
        assert_eq!(
            ctx.implementation_notes[0].notes,
            vec!["note one", "note two"]
        );
    }

    #[test]
    fn criterion_status_threads_into_unified_context() {
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
                last_commit: GitSha::new(SHA).expect("valid git sha"),
                commits_since: 0,
            },
        }];
        let ctx = build_template_context(base_fields(), cs.clone());
        assert_eq!(ctx.criterion_status, cs);
    }

    #[test]
    fn touched_specs_render_changed_spec_roster_with_path_markers() {
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
        let changed = changed_specs_from_touched(&SpecLabel::new("alpha"), &touched);
        assert_eq!(changed[0].spec_path, "specs/alpha.md");
        assert!(
            changed[0]
                .diff
                .as_deref()
                .is_some_and(|d| d.contains("alpha diff"))
        );
        assert_eq!(changed[1].label, SpecLabel::new("beta"));
    }

    #[test]
    fn empty_touched_set_uses_anchor_changed_spec() {
        let changed = changed_specs_from_touched(&SpecLabel::new("alpha"), &[]);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].spec_path, "specs/alpha.md");
        assert!(changed[0].diff.is_none());
    }
}

use loom_driver::identifier::{BeadId, MoleculeId, SpecLabel};
use loom_protocol::todo::{GitSha, TodoFingerprint};
use loom_templates::SkillIndexMarkdown;
use loom_templates::criterion_status::CriterionStatus;
use loom_templates::todo::{
    SpecEpicContext, SpecImplementationNotes, TodoChangedSpec, TodoContext,
};
use serde::Serialize;

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
    pub skill_index: SkillIndexMarkdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FingerprintSpecInput {
    pub label: SpecLabel,
    pub spec_path: String,
    pub spec_blob_sha: String,
    pub spec_epic_id: BeadId,
    pub todo_cursor: Option<String>,
    pub initialized: bool,
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
        skill_index: base.skill_index,
    }
}

pub fn changed_spec_context(
    label: SpecLabel,
    spec_path: impl Into<String>,
    diff: Option<String>,
) -> TodoChangedSpec {
    TodoChangedSpec {
        label,
        spec_path: spec_path.into(),
        diff,
    }
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
    docs_index_blob_sha: &str,
    changed_specs: &[FingerprintSpecInput],
) -> TodoFingerprint {
    #[derive(Serialize)]
    struct Canonical<'a> {
        head: &'a str,
        docs_index_blob_sha: &'a str,
        specs: Vec<CanonicalSpec<'a>>,
    }

    #[derive(Serialize)]
    struct CanonicalSpec<'a> {
        label: &'a str,
        spec_path: &'a str,
        spec_blob_sha: &'a str,
        spec_epic_id: &'a str,
        todo_cursor: Option<&'a str>,
        initialized: bool,
    }

    let mut sorted = changed_specs.iter().collect::<Vec<_>>();
    sorted.sort_by(|left, right| left.label.as_str().cmp(right.label.as_str()));
    let canonical = Canonical {
        head: todo_head.as_str(),
        docs_index_blob_sha,
        specs: sorted
            .into_iter()
            .map(|spec| CanonicalSpec {
                label: spec.label.as_str(),
                spec_path: &spec.spec_path,
                spec_blob_sha: &spec.spec_blob_sha,
                spec_epic_id: spec.spec_epic_id.as_str(),
                todo_cursor: spec.todo_cursor.as_deref(),
                initialized: spec.initialized,
            })
            .collect(),
    };
    let bytes = match serde_json::to_vec(&canonical) {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::error!(target: "loom::bug", error = %err, "todo fingerprint canonicalization failed");
            std::process::abort();
        }
    };
    let digest = blake3::hash(&bytes).to_hex().to_string();
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
        let changed_specs = vec![changed_spec_context(
            SpecLabel::new("alpha"),
            "specs/alpha.md",
            None,
        )];
        let fingerprint_input = vec![FingerprintSpecInput {
            label: SpecLabel::new("alpha"),
            spec_path: "specs/alpha.md".to_string(),
            spec_blob_sha: SHA.to_string(),
            spec_epic_id: work_epic.clone(),
            todo_cursor: None,
            initialized: true,
        }];
        let todo_fingerprint = todo_fingerprint(&todo_head, SHA, &fingerprint_input);
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
            skill_index: SkillIndexMarkdown::empty(),
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
            criterion_id: CriterionId::for_spec_text(&SpecLabel::new("harness"), "Build succeeds"),
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
    fn fingerprint_is_order_independent_for_changed_specs() {
        let head = GitSha::new(SHA).expect("valid git sha");
        let alpha = FingerprintSpecInput {
            label: SpecLabel::new("alpha"),
            spec_path: "specs/alpha.md".to_string(),
            spec_blob_sha: SHA.to_string(),
            spec_epic_id: BeadId::new("lm-alpha").expect("valid bead id"),
            todo_cursor: None,
            initialized: true,
        };
        let beta = FingerprintSpecInput {
            label: SpecLabel::new("beta"),
            spec_path: "specs/beta.md".to_string(),
            spec_blob_sha: SHA.to_string(),
            spec_epic_id: BeadId::new("lm-beta").expect("valid bead id"),
            todo_cursor: Some(SHA.to_string()),
            initialized: false,
        };
        let left = todo_fingerprint(&head, SHA, &[beta.clone(), alpha.clone()]);
        let right = todo_fingerprint(&head, SHA, &[alpha, beta]);
        assert_eq!(left, right);
    }

    #[test]
    fn changed_spec_context_sets_path_and_diff() {
        let changed = changed_spec_context(
            SpecLabel::new("alpha"),
            PathBuf::from("specs/alpha.md")
                .to_string_lossy()
                .into_owned(),
            Some("alpha diff line\n".into()),
        );
        assert_eq!(changed.spec_path, "specs/alpha.md");
        assert!(
            changed
                .diff
                .as_deref()
                .is_some_and(|d| d.contains("alpha diff"))
        );
    }
}

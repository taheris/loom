//! Native production-builder and backend recovery controls for captured package context.

#[cfg(test)]
mod tests {
    use std::path::Path;

    use askama::Template;
    use loom_driver::agent::{AgentBackend, AgentRuntime};
    use loom_driver::identifier::{BeadId, ProfileName, SpecLabel};
    use loom_driver::spec::context::{Selection, load};
    use loom_protocol::context::Purpose;
    use loom_templates::SkillIndexMarkdown;
    use loom_templates::review::ReviewLane;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("docs")).unwrap();
        std::fs::write(
            dir.path().join("docs/README.md"),
            "- [alpha](../specs/alpha/spec.md)\n- [beta](../specs/beta/spec.md)\n",
        )
        .unwrap();
        for label in ["alpha", "beta"] {
            let root = dir.path().join("specs").join(label);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("spec.md"), format!("# {label}\n\nInvariant: {label} must preserve human intent.\n\nAcceptance: [criteria](tests.md#success-criteria).\n")).unwrap();
            std::fs::write(root.join("tests.md"), format!("# {label} acceptance\n\n## Success Criteria\n\n- {label} assigned obligation is mandatory. [test?](future_{label}_verifier)\n")).unwrap();
            std::fs::write(root.join("model.qnt"), "SUPPORT_BODY_MUST_NOT_BE_PINNED").unwrap();
        }
        dir
    }

    fn label() -> SpecLabel {
        SpecLabel::new("alpha").unwrap()
    }

    fn prompts(workspace: &Path) -> Vec<String> {
        let planning = load(workspace, Purpose::Planning, Selection::All).unwrap();
        let decomposition = load(workspace, Purpose::Decomposition, Selection::All).unwrap();
        let implementation = load(workspace, Purpose::Implementation, Selection::All).unwrap();
        let review = load(workspace, Purpose::Review, Selection::All).unwrap();
        let snapshot = loom_gate::acceptance::load(workspace).unwrap();
        let references = loom_protocol::acceptance::References::new(
            snapshot
                .obligations()
                .filter(|obligation| obligation.reference().spec_label() == &label())
                .map(|obligation| obligation.reference().clone())
                .collect(),
        )
        .unwrap();
        let resolved = snapshot.resolve(&references).unwrap();
        vec![
            crate::plan::render_prompt(crate::plan::PlanPromptInputs {
                package_context: Some(planning),
                anchor_labels: vec![label()],
                pinned_context: String::new(),
                spec_index: String::new(),
                companion_paths: vec![],
                scratchpad_path: "scratch.md".into(),
                spec_conventions: "docs/spec-conventions.md".into(),
                skill_index: SkillIndexMarkdown::empty(),
            })
            .unwrap(),
            crate::todo::build_template_context(
                crate::todo::TemplateBaseFields {
                    package_context: Some(decomposition),
                    pinned_context: String::new(),
                    spec_index: String::new(),
                    changed_specs: vec![crate::todo::changed_spec_context(
                        label(),
                        "specs/alpha/spec.md",
                        "specs/alpha/tests.md",
                        "specs/alpha",
                        None,
                    )],
                    work_epic: BeadId::new("lm-context").unwrap(),
                    todo_head: loom_protocol::todo::GitSha::new(
                        "0123456789abcdef0123456789abcdef01234567",
                    )
                    .unwrap(),
                    todo_fingerprint: loom_protocol::todo::TodoFingerprint::new(&"00".repeat(32))
                        .unwrap(),
                    spec_epics: vec![],
                    companion_paths: vec![],
                    implementation_notes: vec![],
                    scratchpad_path: "scratch.md".into(),
                    skill_index: SkillIndexMarkdown::empty(),
                },
                vec![],
            )
            .render()
            .unwrap(),
            crate::r#loop::render_loop_prompt(crate::r#loop::LoopContextInputs {
                package_context: Some(implementation),
                acceptance: loom_protocol::acceptance::dispatch::Acceptance::Implementation(
                    resolved,
                ),
                acceptance_workspace: workspace.to_path_buf(),
                label: label(),
                spec_path: "specs/alpha/spec.md".into(),
                pinned_context: String::new(),
                companion_paths: vec![],
                molecule_id: None,
                issue_id: BeadId::new("lm-context.1").unwrap(),
                bead_base: loom_protocol::oid::GitOid::new(
                    "0123456789abcdef0123456789abcdef01234567",
                )
                .unwrap(),
                title: "Assigned work".into(),
                description: "Preserve selected context".into(),
                previous_failure: None,
                workspace_recovery: None,
                attempt: 0,
                scratchpad_path: "scratch.md".into(),
                style_rules: "docs/style-rules.md".into(),
                skill_index: SkillIndexMarkdown::empty(),
            })
            .unwrap(),
            crate::review::build_review_context(crate::review::ReviewContextInputs {
                package_context: Some(review),
                label: label(),
                spec_path: "specs/alpha/spec.md".into(),
                pinned_context: String::new(),
                companion_paths: vec![],
                molecule_id: None,
                base_commit: None,
                beads_summary: None,
                test_sources: vec![],
                judge_rubrics: vec![],
                scratchpad_path: "scratch.md".into(),
                style_rules: "docs/style-rules.md".into(),
                lane: ReviewLane::Rubric,
                default_profile: ProfileName::base(),
                skill_index: SkillIndexMarkdown::empty(),
            })
            .render()
            .unwrap(),
        ]
    }

    #[test]
    fn quint_phase_context_preserves_applicable_obligations() {
        let dir = fixture();
        let rendered = prompts(dir.path());
        for body in &rendered {
            assert!(body.contains("alpha must preserve human intent."));
            assert!(
                body.contains("beta must preserve human intent."),
                "uncertainty must not hide sibling invariants"
            );
            assert!(!body.contains("SUPPORT_BODY_MUST_NOT_BE_PINNED"));
        }
        assert!(rendered[0].contains("specs/alpha/tests.md"));
        assert!(rendered[1].contains("beta assigned obligation is mandatory."));
        assert!(rendered[1].contains("historical observations"));
        assert!(rendered[1].contains("coverage is unavailable, not passing"));
        assert!(rendered[2].contains("Purpose: implementation"));
        assert!(rendered[2].contains("future_alpha_verifier"));
        assert!(rendered[3].contains("beta assigned obligation is mandatory."));
    }

    #[test]
    fn quint_context_selection_survives_compaction() {
        let dir = fixture();
        let rendered = prompts(dir.path());
        let manifest = dir.path().join("profiles.json");
        loom_test_support::profile_manifest::write(&manifest, r#"{"base":{"pi":{"ref":"fixture","source":"/fixture/image","source_kind":"nix-descriptor"}}}"#, Path::new("/fixture/wrix")).unwrap();
        let manifest =
            loom_driver::profile_manifest::ProfileImageManifest::from_path(&manifest).unwrap();
        let entry = manifest
            .lookup(&ProfileName::base(), AgentRuntime::Pi)
            .unwrap();
        for (index, body) in rendered.into_iter().enumerate() {
            let scratch = loom_driver::scratch::ScratchSession::open(
                dir.path(),
                &format!("phase-{index}"),
                &body,
                "phase context",
            )
            .unwrap();
            std::fs::write(
                dir.path().join("specs/alpha/spec.md"),
                "NEW_SOURCE_AFTER_DISPATCH",
            )
            .unwrap();
            std::fs::write(
                dir.path().join("specs/alpha/tests.md"),
                "NEW_ACCEPTANCE_AFTER_DISPATCH",
            )
            .unwrap();
            let spawn = crate::spawn::build_spawn_config(
                entry,
                AgentRuntime::Pi,
                dir.path().to_path_buf(),
                body.clone(),
                scratch.path().to_path_buf(),
                vec![],
                vec![],
                vec![],
                vec![],
            );
            let repin = loom_agent::pi::backend::PiBackend::compaction_repin(&spawn)
                .unwrap()
                .unwrap();
            assert!(repin.contains(&body));
            assert!(repin.contains("alpha must preserve human intent."));
            assert!(!repin.contains("NEW_SOURCE_AFTER_DISPATCH"));
            assert!(!repin.contains("NEW_ACCEPTANCE_AFTER_DISPATCH"));
        }
    }

    #[test]
    fn planning_distinguishes_new_anchors_from_missing_indexed_packages() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("docs")).unwrap();
        let index = dir.path().join("docs/README.md");
        std::fs::write(&index, "# New project\n").unwrap();
        let anchors = [SpecLabel::new("future").unwrap()];
        let context = load(dir.path(), Purpose::Planning, Selection::Labels(&anchors)).unwrap();
        assert_eq!(context.packages(), []);
        assert!(context.to_string().contains("New planning anchor `future`"));
        std::fs::write(&index, "- [alpha](../specs/alpha/spec.md)\n").unwrap();
        assert!(
            matches!(load(dir.path(), Purpose::Planning, Selection::All), Err(loom_driver::spec::context::Error::Missing(owner)) if owner == label())
        );
    }

    #[test]
    fn quint_progressive_disclosure_separates_obligations_from_support() {
        let dir = fixture();
        let labels = [label()];
        let captured = load(dir.path(), Purpose::Planning, Selection::Labels(&labels))
            .unwrap()
            .with_snapshot_root("context".into());
        let body = captured.to_string();
        assert!(!body.contains("beta must preserve"));
        assert!(
            !body.contains("future_alpha_verifier"),
            "planning retrieves detailed acceptance rather than pinning it"
        );
        assert!(body.contains("context/alpha/acceptance.md"));
        assert!(!body.contains("SUPPORT_BODY_MUST_NOT_BE_PINNED"));
        let scratch = tempfile::tempdir().unwrap();
        loom_driver::spec::context::write(&captured, scratch.path()).unwrap();
        let before =
            std::fs::read_to_string(scratch.path().join("context/alpha/acceptance.md")).unwrap();
        std::fs::write(dir.path().join("specs/alpha/tests.md"), "changed later").unwrap();
        assert_eq!(
            std::fs::read_to_string(scratch.path().join("context/alpha/acceptance.md")).unwrap(),
            before
        );
        assert!(before.contains("future_alpha_verifier"));
        assert!(
            load(
                dir.path(),
                Purpose::Implementation,
                Selection::Labels(&[SpecLabel::new("missing").unwrap()])
            )
            .is_err()
        );
    }
}

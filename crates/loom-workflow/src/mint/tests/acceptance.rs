#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::r#loop::{acceptance::Resolved, worker};
    use crate::mint::acceptance as remediation;
    use loom_driver::agent::AgentRuntime;
    use loom_driver::config::{LoomTopConfig, SkillsConfig};
    use loom_driver::git::GitOid;
    use loom_driver::identifier::ProfileName;
    use loom_driver::profile_manifest::ProfileImageManifest;
    use loom_protocol::acceptance::{
        Candidates, GoalReferences, METADATA_KEY, Reference, References,
    };
    use loom_protocol::gate::{DispatchScope, RawFinding};
    use std::path::{Path, PathBuf};

    struct Fixture {
        dir: tempfile::TempDir,
        workspace: PathBuf,
        manifest: ProfileImageManifest,
        bd: BdClient<StatefulBdRunner>,
    }

    impl Fixture {
        async fn new(body: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let git = loom_driver::git::init_test_repo_with_integration(dir.path()).unwrap();
            let workspace = git
                .create_worktree(&spec("agent"), &BeadId::new("lm-test").unwrap())
                .await
                .unwrap()
                .path;
            std::fs::create_dir_all(workspace.join("specs")).unwrap();
            std::fs::create_dir_all(workspace.join("docs")).unwrap();
            std::fs::write(workspace.join("specs/agent.md"), body).unwrap();
            std::fs::write(workspace.join("docs/README.md"), "# Specs\n\n| Spec | Epic |\n| --- | --- |\n| [agent](../specs/agent.md) | `lm-mol` |\n").unwrap();
            let manifest_path = dir.path().join("images.json");
            loom_test_support::profile_manifest::write(
            &manifest_path,
            r#"{"rust":{"pi":{"ref":"fixture-image","source":"/fixture/image","source_kind":"nix-descriptor"}}}"#,
            Path::new("/fixture/wrix"),
        ).unwrap();
            Self {
                manifest: ProfileImageManifest::from_path(&manifest_path).unwrap(),
                dir,
                workspace,
                bd: BdClient::with_runner(StatefulBdRunner::molecule()),
            }
        }

        fn criterion_finding(&self, token: ConcernToken, route: FindingRoute) -> Finding {
            let path = self.workspace.join("specs/agent.md");
            let body = std::fs::read_to_string(&path).unwrap();
            let parsed = loom_gate::annotation::parse_content(&path, &body).unwrap();
            let criterion = &parsed.criteria[0];
            let anchor = loom_gate::annotation::criterion_id_for(&spec("agent"), &criterion.text);
            RawFinding {
                token,
                route,
                bonds: vec![spec("agent")],
                target: FindingTarget::Criterion {
                    spec: spec("agent"),
                    anchor,
                },
                evidence: format!(
                    "{}:{}: broken acceptance: {}",
                    path.display(),
                    criterion.line,
                    criterion.text
                ),
            }
            .resolve(
                DispatchScope::Tree,
                &crate::review::WorkspaceFindingValidator::new(&self.workspace),
            )
            .unwrap()
        }

        async fn mint(&self, findings: &[Finding]) -> Bead {
            let summary = mint_findings(&self.bd, findings, "head").await;
            assert_eq!(summary.minted, 1, "{summary:?}");
            let BatchOutcome::Minted { bead_id, .. } = &summary.batches[0] else {
                panic!("not minted");
            };
            self.bd.show(bead_id).await.unwrap()
        }

        async fn prepare(&self, bead: &Bead) -> worker::Preparation {
            worker::prepare_with_bd(
                &self.bd,
                worker::Request {
                    bead,
                    workspace: &self.workspace,
                    loom_workspace: self.dir.path(),
                    manifest: &self.manifest,
                    cli_profile: None,
                    phase_default: &ProfileName::new("rust").unwrap(),
                    runtime: AgentRuntime::Pi,
                    label: &spec("agent"),
                    style_rules: "docs/style-rules.md",
                    loom: &LoomTopConfig::default(),
                    skills: &SkillsConfig::default(),
                    launcher_env: vec![],
                    previous_failure: None,
                    bead_base: GitOid::new("0123456789abcdef0123456789abcdef01234567").unwrap(),
                    workspace_recovery: None,
                    attempt: 0,
                },
            )
            .await
            .unwrap()
        }

        async fn set_acceptance(&self, bead: &Bead, value: &str) -> Bead {
            self.bd
                .update(
                    &bead.id,
                    UpdateOpts {
                        set_metadata: vec![(METADATA_KEY.into(), value.into())],
                        ..UpdateOpts::default()
                    },
                )
                .await
                .unwrap();
            self.bd.show(&bead.id).await.unwrap()
        }
    }

    fn goals(task: &Bead) -> GoalReferences {
        let Candidates::Remediation { goals } =
            Candidates::from_metadata(&task.metadata[METADATA_KEY]).unwrap()
        else {
            panic!("not remediation");
        };
        goals
    }

    fn ready(prepared: worker::Preparation) -> Box<worker::Worker> {
        let worker::Preparation::Ready(worker) = prepared else {
            panic!("worker rejected");
        };
        worker
    }

    fn rejected(prepared: worker::Preparation) {
        let worker::Preparation::Rejected(crate::r#loop::AgentOutcome::Blocked { reason }) =
            prepared
        else {
            panic!("invalid acceptance reached spawn preparation");
        };
        assert!(
            reason.contains("acceptance rejected before spawn"),
            "{reason}"
        );
    }

    #[tokio::test]
    async fn real_mint_dispatches_missing_malformed_and_multiple_annotation_repairs() {
        for bullet in [
            "- Repair missing annotation.\n",
            "- Repair malformed annotation. [test](unclosed\n",
            "- Repair multiple annotations. [test](one) [test](two)\n",
        ] {
            let fixture =
                Fixture::new(&format!("# Agent\n\n## Success Criteria\n\n{bullet}")).await;
            assert!(loom_gate::acceptance::load(&fixture.workspace).is_err());
            let finding = fixture.criterion_finding(
                ConcernToken::SpecConventionsViolation,
                FindingRoute::Deferred,
            );
            let task = fixture.mint(std::slice::from_ref(&finding)).await;
            let prepared = ready(fixture.prepare(&task).await);
            let Some(Resolved::Remediation(acceptance)) = &prepared.acceptance else {
                panic!("wrong purpose");
            };
            assert_eq!(acceptance.goals().len(), 1);
            assert_eq!(acceptance.goals()[0].finding(), &finding);
            assert_eq!(acceptance.goals()[0].task(), &task.id);
            assert_eq!(
                acceptance.goals()[0].work_root(),
                task.parent.as_ref().unwrap()
            );
            assert_eq!(
                acceptance.goals()[0].reference(),
                &goals(&task).as_slice()[0]
            );
        }
    }

    #[tokio::test]
    async fn real_deferred_merge_and_promotion_persist_all_goals_and_preserve_metadata() {
        let fixture = Fixture::new("# Agent\n\n## Success Criteria\n\n- Missing verifier.\n").await;
        let finding = fixture.criterion_finding(
            ConcernToken::SpecConventionsViolation,
            FindingRoute::Deferred,
        );
        let molecule = MoleculeId::new("lm-mol").unwrap();
        assert_eq!(
            route_molecule_findings(&fixture.bd, &molecule, &[finding], &MintOptions::default())
                .await
                .minted,
            1
        );
        let task_id = BeadId::new("lm-mol.1").unwrap();
        fixture
            .bd
            .update(
                &task_id,
                UpdateOpts {
                    set_metadata: vec![("operator.note".into(), "keep".into())],
                    ..UpdateOpts::default()
                },
            )
            .await
            .unwrap();
        let additional = contract_finding(
            vec![spec("agent")],
            "remaining-contract",
            "retained evidence",
        );
        assert_eq!(
            route_molecule_findings(
                &fixture.bd,
                &molecule,
                &[additional],
                &MintOptions::default()
            )
            .await
            .deferred_findings_merged,
            1
        );
        let promoted = promote_deferred(&fixture.bd, &molecule, false).await;
        assert_eq!(promoted.promoted_deferred, 1, "{promoted:?}");
        let task = fixture.bd.show(&task_id).await.unwrap();
        assert_eq!(task.metadata["operator.note"], "keep");
        assert_eq!(task.status, loom_driver::bd::Status::Open);
        let prepared = ready(fixture.prepare(&task).await);
        let Some(Resolved::Remediation(acceptance)) = &prepared.acceptance else {
            panic!("wrong purpose");
        };
        assert_eq!(acceptance.goals().len(), 2);
    }

    #[tokio::test]
    async fn worker_split_retains_source_attribution_after_partial_progress() {
        let fixture = Fixture::new("# Agent\n\n## Success Criteria\n\n- Missing verifier.\n").await;
        std::fs::write(fixture.workspace.join("resolved.rs"), "before repair").unwrap();
        let repaired = RawFinding {
            token: ConcernToken::MockDiscipline,
            route: FindingRoute::Deferred,
            bonds: vec![spec("agent")],
            target: FindingTarget::TestPath {
                path: "resolved.rs".into(),
            },
            evidence: "mock isolation missing".into(),
        }
        .resolve(
            DispatchScope::Tree,
            &crate::review::WorkspaceFindingValidator::new(&fixture.workspace),
        )
        .unwrap();
        let remaining = fixture.criterion_finding(
            ConcernToken::SpecConventionsViolation,
            FindingRoute::Deferred,
        );
        let source = fixture.mint(&[repaired, remaining]).await;
        let original_goals = goals(&source);
        let subset = GoalReferences::new(vec![
            original_goals
                .as_slice()
                .iter()
                .find(|reference| {
                    reference.finding_hash.as_str()
                        == fixture
                            .criterion_finding(
                                ConcernToken::SpecConventionsViolation,
                                FindingRoute::Deferred,
                            )
                            .hash()
                })
                .unwrap()
                .clone(),
        ])
        .unwrap();
        std::fs::remove_file(fixture.workspace.join("resolved.rs")).unwrap();
        fixture
            .bd
            .update(
                &source.id,
                UpdateOpts {
                    status: Some(loom_driver::bd::Status::Closed),
                    ..UpdateOpts::default()
                },
            )
            .await
            .unwrap();
        let split_id = fixture
            .bd
            .create(CreateOpts {
                title: "remaining repair".into(),
                parent: source.parent.clone(),
                issue_type: Some(loom_driver::bd::IssueType::Task),
                labels: vec!["profile:rust".into()],
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        fixture
            .bd
            .update(
                &split_id,
                UpdateOpts {
                    set_metadata: vec![
                        (
                            remediation::FOLLOW_UP_KEY.into(),
                            serde_json::to_string(&remediation::FollowUp {
                                source: source.id.clone(),
                                goals: subset.clone(),
                            })
                            .unwrap(),
                        ),
                        ("operator.note".into(), "keep".into()),
                    ],
                    ..UpdateOpts::default()
                },
            )
            .await
            .unwrap();
        let split = fixture.bd.show(&split_id).await.unwrap();
        let prepared = ready(fixture.prepare(&split).await);
        let Some(Resolved::Remediation(resolved)) = &prepared.acceptance else {
            panic!("wrong purpose");
        };
        assert_eq!(resolved.goals().len(), 1);
        let split = fixture.bd.show(&split_id).await.unwrap();
        assert_eq!(goals(&split), subset);
        assert_eq!(split.metadata["operator.note"], "keep");
        assert!(!split.metadata.contains_key(remediation::FOLLOW_UP_KEY));
        let source = fixture.bd.show(&source.id).await.unwrap();
        assert_eq!(goals(&source), original_goals);
        assert_eq!(source.status, loom_driver::bd::Status::Closed);
        let batch: serde_json::Value =
            serde_json::from_str(source.metadata[remediation::BATCH_KEY].as_str().unwrap())
                .unwrap();
        assert_eq!(
            batch["attribution"][source.id.as_str()],
            serde_json::to_value(original_goals).unwrap()
        );
        assert_eq!(
            batch["attribution"][split_id.as_str()],
            serde_json::to_value(subset).unwrap()
        );
    }

    #[tokio::test]
    async fn missing_forged_wrong_batch_and_unattributed_goals_reject_before_spawn() {
        let fixture = Fixture::new("# Agent\n\n## Success Criteria\n\n- Missing verifier.\n").await;
        let finding = fixture.criterion_finding(
            ConcernToken::SpecConventionsViolation,
            FindingRoute::Deferred,
        );
        let task = fixture.mint(&[finding]).await;
        let encoded = task.metadata[METADATA_KEY].as_str().unwrap().to_owned();
        fixture
            .bd
            .update(
                &task.id,
                UpdateOpts {
                    unset_metadata: vec![METADATA_KEY.into()],
                    ..UpdateOpts::default()
                },
            )
            .await
            .unwrap();
        rejected(
            fixture
                .prepare(&fixture.bd.show(&task.id).await.unwrap())
                .await,
        );
        rejected(
            fixture
                .prepare(&fixture.set_acceptance(&task, "{malformed").await)
                .await,
        );
        rejected(
            fixture
                .prepare(
                    &fixture
                        .set_acceptance(&task, r#"{"purpose":"remediation","goals":[]}"#)
                        .await,
                )
                .await,
        );
        let mut forged: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        forged["goals"][0]["finding_hash"] = serde_json::json!("v1:000000000000");
        rejected(
            fixture
                .prepare(&fixture.set_acceptance(&task, &forged.to_string()).await)
                .await,
        );
        let other = fixture
            .mint(&[contract_finding(
                vec![spec("agent")],
                "other-batch",
                "other evidence",
            )])
            .await;
        rejected(
            fixture
                .prepare(
                    &fixture
                        .set_acceptance(&task, other.metadata[METADATA_KEY].as_str().unwrap())
                        .await,
                )
                .await,
        );
        let sibling_id = fixture
            .bd
            .create(CreateOpts {
                title: "forged sibling".into(),
                parent: task.parent.clone(),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        let sibling = fixture.bd.show(&sibling_id).await.unwrap();
        rejected(
            fixture
                .prepare(&fixture.set_acceptance(&sibling, &encoded).await)
                .await,
        );
    }

    #[tokio::test]
    async fn copied_finding_is_reresolved_and_empty_evidence_is_rejected() {
        let fixture = Fixture::new("# Agent\n\n## Success Criteria\n\n- Missing verifier.\n").await;
        let task = fixture
            .mint(&[fixture.criterion_finding(
                ConcernToken::SpecConventionsViolation,
                FindingRoute::Deferred,
            )])
            .await;
        let mut batch: serde_json::Value =
            serde_json::from_str(task.metadata[remediation::BATCH_KEY].as_str().unwrap()).unwrap();
        batch["findings"][0]["finding"]["evidence"] = serde_json::json!("");
        fixture
            .bd
            .update(
                &task.id,
                UpdateOpts {
                    set_metadata: vec![(remediation::BATCH_KEY.into(), batch.to_string())],
                    ..UpdateOpts::default()
                },
            )
            .await
            .unwrap();
        rejected(
            fixture
                .prepare(&fixture.bd.show(&task.id).await.unwrap())
                .await,
        );
        batch["findings"][0]["finding"]["evidence"] = serde_json::json!("copied evidence");
        fixture
            .bd
            .update(
                &task.id,
                UpdateOpts {
                    set_metadata: vec![(remediation::BATCH_KEY.into(), batch.to_string())],
                    ..UpdateOpts::default()
                },
            )
            .await
            .unwrap();
        std::fs::write(
            fixture.workspace.join("specs/agent.md"),
            "# Agent\n\n## Success Criteria\n\n- Different requirement.\n",
        )
        .unwrap();
        rejected(
            fixture
                .prepare(&fixture.bd.show(&task.id).await.unwrap())
                .await,
        );
    }

    #[tokio::test]
    async fn ordinary_invalid_references_never_fall_back_to_remediation() {
        let fixture = Fixture::new("# Agent\n\n## Success Criteria\n\n- Missing verifier.\n").await;
        let task = fixture
            .mint(&[fixture.criterion_finding(
                ConcernToken::SpecConventionsViolation,
                FindingRoute::Deferred,
            )])
            .await;
        let criteria = References::new(vec![
            Reference::parse("agent", "criterion-0000000000000000").unwrap(),
        ])
        .unwrap();
        let ordinary = fixture
            .set_acceptance(
                &task,
                &serde_json::to_string(&Candidates::Implementation { criteria }).unwrap(),
            )
            .await;
        rejected(fixture.prepare(&ordinary).await);
    }

    #[tokio::test]
    async fn ordinary_pending_criterion_is_resolved_without_a_verifier() {
        let fixture = Fixture::new(
            "# Agent\n\n## Success Criteria\n\n- Pending implementation. [test?](future_test)\n",
        )
        .await;
        let snapshot = loom_gate::acceptance::load(&fixture.workspace).unwrap();
        let reference = snapshot.obligations().next().unwrap().reference().clone();
        let id = fixture
            .bd
            .create(CreateOpts {
                title: "ordinary pending work".into(),
                labels: vec!["profile:rust".into()],
                parent: Some(BeadId::new("lm-mol").unwrap()),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        let task = fixture.bd.show(&id).await.unwrap();
        let task = fixture
            .set_acceptance(
                &task,
                &serde_json::to_string(&Candidates::Implementation {
                    criteria: References::new(vec![reference]).unwrap(),
                })
                .unwrap(),
            )
            .await;
        let prepared = ready(fixture.prepare(&task).await);
        let Some(Resolved::Implementation(acceptance)) = &prepared.acceptance else {
            panic!("wrong purpose");
        };
        assert_eq!(acceptance.obligations().len(), 1);
        assert!(acceptance.obligations()[0].annotation().pending);
    }

    #[tokio::test]
    async fn deferred_merge_and_promotion_never_convert_invalid_ordinary_bindings() {
        let fixture = Fixture::new("# Agent\n\n## Success Criteria\n\n- Missing verifier.\n").await;
        let molecule = MoleculeId::new("lm-mol").unwrap();
        let finding = fixture.criterion_finding(
            ConcernToken::SpecConventionsViolation,
            FindingRoute::Deferred,
        );
        let created =
            route_molecule_findings(&fixture.bd, &molecule, &[finding], &MintOptions::default())
                .await;
        assert_eq!(created.minted, 1);
        let task = fixture
            .bd
            .show(&BeadId::new("lm-mol.1").unwrap())
            .await
            .unwrap();
        let criteria = References::parse([("agent", "criterion-0000000000000000")]).unwrap();
        let ordinary = fixture
            .set_acceptance(
                &task,
                &serde_json::to_string(&Candidates::Implementation { criteria }).unwrap(),
            )
            .await;
        let additional = contract_finding(vec![spec("agent")], "other-goal", "retained evidence");
        let merged = route_molecule_findings(
            &fixture.bd,
            &molecule,
            &[additional],
            &MintOptions::default(),
        )
        .await;
        assert_eq!(merged.refused, 1, "{merged:?}");
        let promoted = promote_deferred(&fixture.bd, &molecule, false).await;
        assert_eq!(promoted.refused, 1, "{promoted:?}");
        assert_eq!(fixture.bd.show(&task.id).await.unwrap(), ordinary);
    }

    #[tokio::test]
    async fn incomplete_goal_assignment_rejects_without_losing_batch_attribution() {
        let fixture = Fixture::new("# Agent\n\n## Success Criteria\n\n- Missing verifier.\n").await;
        let task = fixture
            .mint(&[
                fixture.criterion_finding(
                    ConcernToken::SpecConventionsViolation,
                    FindingRoute::Deferred,
                ),
                contract_finding(vec![spec("agent")], "second-goal", "remaining evidence"),
            ])
            .await;
        let subset = GoalReferences::new(vec![goals(&task).as_slice()[0].clone()]).unwrap();
        let incomplete = fixture
            .set_acceptance(
                &task,
                &serde_json::to_string(&Candidates::Remediation { goals: subset }).unwrap(),
            )
            .await;
        rejected(fixture.prepare(&incomplete).await);
        assert_eq!(
            incomplete.metadata[remediation::BATCH_KEY],
            task.metadata[remediation::BATCH_KEY]
        );
    }

    #[tokio::test]
    async fn finding_labels_and_description_cannot_manufacture_acceptance() {
        let fixture = Fixture::new("# Agent\n\n## Success Criteria\n\n- Missing verifier.\n").await;
        let task = fixture
            .mint(&[fixture.criterion_finding(
                ConcernToken::SpecConventionsViolation,
                FindingRoute::Deferred,
            )])
            .await;
        fixture
            .bd
            .update(
                &task.id,
                UpdateOpts {
                    unset_metadata: vec![METADATA_KEY.into(), remediation::BATCH_KEY.into()],
                    ..UpdateOpts::default()
                },
            )
            .await
            .unwrap();
        rejected(
            fixture
                .prepare(&fixture.bd.show(&task.id).await.unwrap())
                .await,
        );
    }

    #[tokio::test]
    async fn follow_up_cannot_replace_an_ordinary_task_or_borrow_unrelated_goals() {
        let fixture = Fixture::new("# Agent\n\n## Success Criteria\n\n- Missing verifier.\n").await;
        let source = fixture
            .mint(&[fixture.criterion_finding(
                ConcernToken::SpecConventionsViolation,
                FindingRoute::Deferred,
            )])
            .await;
        let unrelated = fixture
            .mint(&[contract_finding(
                vec![spec("agent")],
                "other-batch",
                "other evidence",
            )])
            .await;
        let id = fixture
            .bd
            .create(CreateOpts {
                title: "split".into(),
                parent: source.parent.clone(),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        let task = fixture.bd.show(&id).await.unwrap();
        let proposal = remediation::FollowUp {
            source: source.id.clone(),
            goals: goals(&unrelated),
        };
        assert!(
            remediation::admit_follow_up(&fixture.bd, &task, &proposal, &fixture.workspace)
                .await
                .is_err()
        );
        let criteria = References::parse([("agent", "criterion-0000000000000000")]).unwrap();
        let ordinary = fixture
            .set_acceptance(
                &task,
                &serde_json::to_string(&Candidates::Implementation { criteria }).unwrap(),
            )
            .await;
        let proposal = remediation::FollowUp {
            source: source.id.clone(),
            goals: goals(&source),
        };
        assert!(
            remediation::admit_follow_up(&fixture.bd, &ordinary, &proposal, &fixture.workspace)
                .await
                .is_err()
        );
        assert_eq!(fixture.bd.show(&source.id).await.unwrap(), source);
        assert_eq!(fixture.bd.show(&id).await.unwrap(), ordinary);
    }
}

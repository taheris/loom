#[cfg(test)]
mod cases {
    use super::super::*;

    async fn proposal(f: &Fixture, producer: &BeadId, root: &BeadId, affected: &[BeadId]) {
        f.bd.update(
            &f.decisions[0],
            UpdateOpts {
                status: Some(Status::Blocked),
                set_metadata: vec![(
                    super::super::super::graph::CANDIDATE_KEY.into(),
                    serde_json::json!({"version":1,"producer":producer,"root":root,
                    "affected_work":affected})
                    .to_string(),
                )],
                ..UpdateOpts::default()
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn staged_candidate_binds_producer_root_and_actual_native_affected_work() {
        let f = Fixture::new().await;
        f.bd.dep_add(&f.sibling, &f.decisions[0]).await.unwrap();
        proposal(&f, &f.source.id, &f.root, std::slice::from_ref(&f.sibling)).await;
        assert!(matches!(f.admit().await, AgentOutcome::Waiting { .. }));
        let entry = f.journal().load(&f.source.id).unwrap().unwrap();
        assert!(
            entry
                .context
                .as_ref()
                .unwrap()
                .matches_inventory(&entry.record.decisions)
        );
        let graph: serde_json::Value = serde_json::to_value(entry.context.unwrap()).unwrap();
        assert_eq!(
            graph["decisions"][0]["affected_work"],
            serde_json::json!([f.sibling])
        );
        assert!(
            f.bd.dependency_snapshot(&f.sibling)
                .await
                .unwrap()
                .active_blockers()
                .contains(&f.decisions[0])
        );
        assert_eq!(f.bd.show(&f.source.id).await.unwrap().status, Status::Open);
    }

    #[tokio::test]
    async fn incompatible_candidate_provenance_rejects_the_whole_batch_before_queueing() {
        let f = Fixture::new().await;
        for (producer, root, affected) in [
            (&f.sibling, &f.root, vec![]),
            (&f.source.id, &f.sibling, vec![]),
            (&f.source.id, &f.root, vec![f.sibling.clone()]),
            (
                &f.source.id,
                &f.root,
                vec![f.sibling.clone(), f.sibling.clone()],
            ),
        ] {
            proposal(&f, producer, root, &affected).await;
            assert!(matches!(f.admit().await, AgentOutcome::Failure { .. }));
            assert!(f.journal().load(&f.source.id).unwrap().is_none());
            for id in &f.decisions {
                assert!(!f.bd.show(id).await.unwrap().labels.iter().any(|label| {
                    matches!(
                        label.as_str(),
                        "loom:clarify" | "loom:blocked" | "loom:infra"
                    )
                }));
            }
        }
    }

    #[tokio::test]
    async fn staged_candidate_without_provenance_never_becomes_a_queue_item() {
        let f = Fixture::new().await;
        f.bd.update(
            &f.decisions[0],
            UpdateOpts {
                status: Some(Status::Blocked),
                ..UpdateOpts::default()
            },
        )
        .await
        .unwrap();
        assert!(matches!(f.admit().await, AgentOutcome::Failure { .. }));
        assert!(f.journal().load(&f.source.id).unwrap().is_none());
        assert!(
            !f.bd
                .show(&f.decisions[0])
                .await
                .unwrap()
                .labels
                .iter()
                .any(|label| {
                    matches!(
                        label.as_str(),
                        "loom:clarify" | "loom:blocked" | "loom:infra"
                    )
                })
        );
        assert_eq!(
            f.bd.show(&f.decisions[1]).await.unwrap().status,
            Status::Open
        );
    }

    #[tokio::test]
    async fn malformed_or_unknown_candidate_fields_never_supply_provenance() {
        let f = Fixture::new().await;
        for payload in [
            serde_json::json!({"version":2,"producer":f.source.id,"root":f.root,"affected_work":[]}),
            serde_json::json!({"version":1,"producer":f.source.id,"root":f.root,"affected_work":[],"authority":true}),
            serde_json::json!({"version":1,"producer":"not a bead id","root":f.root,"affected_work":[]}),
        ] {
            f.bd.update(
                &f.decisions[0],
                UpdateOpts {
                    status: Some(Status::Blocked),
                    set_metadata: vec![(
                        super::super::super::graph::CANDIDATE_KEY.into(),
                        payload.to_string(),
                    )],
                    ..UpdateOpts::default()
                },
            )
            .await
            .unwrap();
            let before = f.bd.show(&f.decisions[0]).await.unwrap();
            assert!(matches!(f.admit().await, AgentOutcome::Failure { .. }));
            assert!(f.journal().load(&f.source.id).unwrap().is_none());
            assert_eq!(f.bd.show(&f.decisions[0]).await.unwrap(), before);
            assert_eq!(
                f.bd.show(&f.decisions[1]).await.unwrap().status,
                Status::Open
            );
        }
    }

    #[tokio::test]
    async fn foreign_affected_work_rejects_batch_without_rewriting_native_edges() {
        let f = Fixture::new().await;
        let foreign =
            f.bd.create(CreateOpts {
                title: "foreign work".into(),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        f.bd.dep_add(&foreign, &f.decisions[0]).await.unwrap();
        assert!(matches!(f.admit().await, AgentOutcome::Failure { .. }));
        assert!(f.journal().load(&f.source.id).unwrap().is_none());
        for id in &f.decisions {
            assert_eq!(f.bd.show(id).await.unwrap().status, Status::Open);
        }
        assert!(
            f.bd.dependency_snapshot(&foreign)
                .await
                .unwrap()
                .active_blockers()
                .contains(&f.decisions[0])
        );
    }

    async fn admit_independent(f: &Fixture) {
        let wire = format!(
            "LOOM_CLARIFY: {{\"decisions\":{}}}\nLOOM_COMPLETE",
            serde_json::to_string(&f.decisions).unwrap()
        );
        let outcome = reconcile_session(
            &f.bd,
            &f.journal(),
            &f.source,
            SessionResult::Decoded {
                outcome: SessionOutcome {
                    exit_code: 0,
                    cost_usd: None,
                },
                output: Arc::new(decode(&wire, OutputPhase::Loop)),
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(outcome, AgentOutcome::Success);
    }

    #[tokio::test]
    async fn publication_rechecks_inventory_only_admission_without_minting_replacement_authority() {
        let f = Fixture::new().await;
        admit_independent(&f).await;
        assert!(!f.journal().blocks_scope(&f.bd, &f.root).await.unwrap());
        let before = serde_json::to_value(f.journal().load(&f.source.id).unwrap()).unwrap();
        let late =
            f.bd.create(CreateOpts {
                title: "unreported later discovery".into(),
                issue_type: Some(IssueType::Decision),
                parent: Some(f.source.id.clone()),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        let git = loom_driver::git::GitClient::open(f.dir.path()).unwrap();
        let molecule = f.root.as_str().parse().unwrap();
        let never = std::path::Path::new("/must-not-run-publication-with-an-unreported-decision");
        let result = crate::r#loop::execute_molecule_push_gate(
            &f.bd,
            &SpecLabel::new("loop").unwrap(),
            Some(&molecule),
            crate::r#loop::MoleculePushGateCommands::new(None, never, never),
            f.dir.path(),
            &git,
        )
        .await;
        assert!(matches!(result, Err(LoopError::DecisionWait(ref error))
            if error.to_string().contains(late.as_str())));
        assert_eq!(
            serde_json::to_value(f.journal().load(&f.source.id).unwrap()).unwrap(),
            before
        );
        assert_eq!(f.bd.show(&late).await.unwrap().status, Status::Open);
    }

    #[tokio::test]
    async fn legacy_inventory_graph_recovery_is_explicit_and_publication_stays_read_only() {
        let f = Fixture::new().await;
        admit_independent(&f).await;
        let mut entry = f.journal().load(&f.source.id).unwrap().unwrap();
        entry.context = None;
        f.journal().save(&entry).unwrap();
        let before = serde_json::to_value(f.journal().load(&f.source.id).unwrap()).unwrap();
        assert!(f.journal().blocks_scope(&f.bd, &f.root).await.is_err());
        assert_eq!(
            serde_json::to_value(f.journal().load(&f.source.id).unwrap()).unwrap(),
            before
        );
        let source = f.bd.show(&f.source.id).await.unwrap();
        f.journal()
            .recover_inventory(&f.bd, Some(&f.root), &SpecLabel::new("loop").unwrap())
            .await
            .unwrap();
        assert!(
            f.journal()
                .load(&f.source.id)
                .unwrap()
                .unwrap()
                .context
                .is_some()
        );
        assert!(!f.journal().blocks_scope(&f.bd, &f.root).await.unwrap());
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), source);
    }

    #[tokio::test]
    async fn publication_rejects_changed_recorded_edges_without_upgrading_the_receipt() {
        let f = Fixture::new().await;
        admit_independent(&f).await;
        let before = serde_json::to_value(f.journal().load(&f.source.id).unwrap()).unwrap();
        f.bd.dep_add(&f.sibling, &f.decisions[0]).await.unwrap();
        assert!(f.journal().blocks_scope(&f.bd, &f.root).await.is_err());
        assert_eq!(
            serde_json::to_value(f.journal().load(&f.source.id).unwrap()).unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn changed_affected_work_cannot_reuse_canonical_decision_admission() {
        let f = Fixture::new().await;
        assert!(matches!(f.admit().await, AgentOutcome::Waiting { .. }));
        let before = std::fs::read_dir(f.dir.path().join(".loom/decision-waits"))
            .unwrap()
            .filter_map(|entry| {
                let path = entry.unwrap().path();
                (path.extension().is_some_and(|ext| ext == "json"))
                    .then(|| std::fs::read(path).unwrap())
            })
            .collect::<Vec<_>>();
        f.bd.dep_add(&f.sibling, &f.decisions[0]).await.unwrap();
        assert!(held(&f.bd, &f.journal(), &f.source).await.is_err());
        assert!(matches!(f.admit().await, AgentOutcome::Failure { .. }));
        let after = std::fs::read_dir(f.dir.path().join(".loom/decision-waits"))
            .unwrap()
            .filter_map(|entry| {
                let path = entry.unwrap().path();
                (path.extension().is_some_and(|ext| ext == "json"))
                    .then(|| std::fs::read(path).unwrap())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            before, after,
            "uncertainty cannot replace the retained admission"
        );
        assert!(f.journal().has_pending_for(&f.root).unwrap());
    }
}

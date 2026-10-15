#[cfg(test)]
mod cases {
    use super::super::*;

    fn session(ids: &[BeadId], terminal: &str) -> SessionResult {
        let wire = if ids.is_empty() {
            terminal.to_owned()
        } else {
            format!(
                "LOOM_CLARIFY: {{\"decisions\":{}}}\n{terminal}",
                serde_json::to_string(ids).unwrap()
            )
        };
        SessionResult::Decoded {
            outcome: SessionOutcome {
                exit_code: 0,
                cost_usd: None,
            },
            output: Arc::new(decode(&wire, OutputPhase::Loop)),
        }
    }

    #[tokio::test]
    async fn complete_and_noop_admit_decisions_without_holding_the_producer() {
        for terminal in ["LOOM_COMPLETE", "LOOM_NOOP"] {
            let f = Fixture::new().await;
            let outcome = reconcile_session(
                &f.bd,
                &f.journal(),
                &f.source,
                session(&f.decisions, terminal),
                None,
            )
            .await
            .unwrap();
            assert_eq!(outcome, AgentOutcome::Success);
            let source = f.bd.show(&f.source.id).await.unwrap();
            assert_eq!(source, f.source, "inventory must not mutate the source");
            for (id, label) in f.decisions.iter().zip(["loom:clarify", "loom:blocked"]) {
                let decision = f.bd.show(id).await.unwrap();
                assert_eq!(decision.status, Status::Blocked);
                assert!(
                    decision
                        .labels
                        .iter()
                        .any(|actual| actual.as_str() == label)
                );
            }
            assert_eq!(
                f.journal().load(&source.id).unwrap().unwrap().phase,
                Phase::InventoryAdmitted
            );
            assert!(!f.journal().has_pending_for(&f.root).unwrap());
            assert!(!held(&f.bd, &f.journal(), &source).await.unwrap());
        }
    }

    #[tokio::test]
    async fn positive_outcomes_reject_zero_or_partial_inventory_before_queue_effects() {
        for terminal in ["LOOM_COMPLETE", "LOOM_NOOP", "LOOM_WAITING"] {
            for count in [0, 1] {
                let f = Fixture::new().await;
                let outcome = reconcile_session(
                    &f.bd,
                    &f.journal(),
                    &f.source,
                    session(&f.decisions[..count], terminal),
                    None,
                )
                .await
                .unwrap();
                let AgentOutcome::Failure { error } = outcome else {
                    panic!("unreported inventory accepted: {outcome:?}");
                };
                for id in &f.decisions[count..] {
                    assert!(error.contains(id.as_str()), "missing exact ID: {error}");
                }
                assert!(f.journal().load(&f.source.id).unwrap().is_none());
                for id in &f.decisions {
                    assert_eq!(f.bd.show(id).await.unwrap().status, Status::Open);
                }
            }
        }
    }

    #[tokio::test]
    async fn inventory_replay_preserves_closed_resolution_without_a_new_brief() {
        let f = Fixture::new().await;
        reconcile_session(
            &f.bd,
            &f.journal(),
            &f.source,
            session(&f.decisions, "LOOM_COMPLETE"),
            None,
        )
        .await
        .unwrap();
        for id in &f.decisions {
            f.bd.close(id, Some("human retained outcome"))
                .await
                .unwrap();
            f.bd.update(
                id,
                UpdateOpts {
                    description: Some("resolved; active brief no longer needed".into()),
                    ..UpdateOpts::default()
                },
            )
            .await
            .unwrap();
        }
        let before = f.bd.show(&f.decisions[0]).await.unwrap();
        assert_eq!(
            reconcile_session(
                &f.bd,
                &f.journal(),
                &f.source,
                session(&[], "LOOM_NOOP"),
                None
            )
            .await
            .unwrap(),
            AgentOutcome::Success
        );
        assert_eq!(f.bd.show(&f.decisions[0]).await.unwrap(), before);
    }

    #[tokio::test]
    async fn inventory_restart_finishes_partial_staging_without_projecting_a_wait() {
        let f = Fixture::new().await;
        f.journal()
            .save(&Entry {
                record: Record {
                    version: 1,
                    source: f.source.id.clone(),
                    root: f.root.clone(),
                    decisions: f.decisions.clone(),
                },
                phase: Phase::InventoryPending,
            })
            .unwrap();
        assert!(f.journal().has_pending_for(&f.root).unwrap());
        stage(&f.bd, &[f.bd.show(&f.decisions[0]).await.unwrap()])
            .await
            .unwrap();
        let first = f.bd.show(&f.decisions[0]).await.unwrap();
        assert!(!held(&f.bd, &f.journal(), &f.source).await.unwrap());
        assert_eq!(f.bd.show(&f.decisions[0]).await.unwrap(), first);
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), f.source);
        assert_eq!(
            f.journal().load(&f.source.id).unwrap().unwrap().phase,
            Phase::InventoryAdmitted
        );
        assert!(
            f.bd.show(&f.decisions[1])
                .await
                .unwrap()
                .labels
                .iter()
                .any(|label| label.as_str() == "loom:blocked")
        );
    }

    #[tokio::test]
    async fn independent_inventory_can_become_an_explicit_discovery_child_hold() {
        let f = Fixture::new().await;
        reconcile_session(
            &f.bd,
            &f.journal(),
            &f.source,
            session(&f.decisions, "LOOM_COMPLETE"),
            None,
        )
        .await
        .unwrap();
        assert!(matches!(f.admit().await, AgentOutcome::Waiting { .. }));
        assert!(f.journal().has_pending_for(&f.root).unwrap());
        let before = f.bd.show(&f.source.id).await.unwrap();
        assert!(matches!(
            reconcile_session(
                &f.bd,
                &f.journal(),
                &f.source,
                session(&f.decisions, "LOOM_COMPLETE"),
                None
            )
            .await
            .unwrap(),
            AgentOutcome::Failure { .. }
        ));
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), before);
    }

    async fn closed_with_late_discovery() -> Fixture {
        let mut f = Fixture::new().await;
        for id in &f.decisions {
            f.bd.close(id, Some("human resolved prior discovery"))
                .await
                .unwrap();
        }
        f.bd.close(&f.source.id, Some("worker claims completion"))
            .await
            .unwrap();
        // Exercise supported native writes only: no forced parent closure or
        // ancestry-guard bypass to construct the late-discovery regression.
        let late =
            f.bd.create(CreateOpts {
                title: "late persisted discovery".into(),
                issue_type: Some(IssueType::Decision),
                parent: Some(f.source.id.clone()),
                description: BRIEF.into(),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        f.decisions = vec![late];
        f
    }

    #[tokio::test]
    async fn native_closed_producer_cannot_hide_an_unreported_candidate() {
        let f = closed_with_late_discovery().await;
        let before = f.bd.show(&f.source.id).await.unwrap();
        assert_eq!(before.status, Status::Closed);
        let outcome = reconcile_session(
            &f.bd,
            &f.journal(),
            &f.source,
            session(&[], "LOOM_COMPLETE"),
            None,
        )
        .await
        .unwrap();
        let AgentOutcome::Failure { error } = outcome else {
            panic!("closed producer escaped inventory: {outcome:?}");
        };
        for id in &f.decisions {
            assert!(error.contains(id.as_str()));
            assert_eq!(f.bd.show(id).await.unwrap().status, Status::Open);
        }
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), before);
    }

    #[tokio::test]
    async fn parallel_selection_recovers_staging_for_a_closed_producer() {
        let f = closed_with_late_discovery().await;
        f.journal()
            .save(&Entry {
                record: Record {
                    version: 1,
                    source: f.source.id.clone(),
                    root: f.root.clone(),
                    decisions: f.decisions.clone(),
                },
                phase: Phase::InventoryPending,
            })
            .unwrap();
        let before = f.bd.show(&f.source.id).await.unwrap();
        let batch = crate::r#loop::schedule::parallel_ready_batch(
            &f.bd,
            f.dir.path(),
            &SpecLabel::new("loop").unwrap(),
            Some(&f.root),
            8,
            &[],
            &HashSet::new(),
        )
        .await
        .unwrap();
        assert!(batch.iter().any(|bead| bead.id == f.sibling));
        assert!(!batch.iter().any(|bead| bead.id == f.source.id));
        assert_eq!(
            f.journal().load(&f.source.id).unwrap().unwrap().phase,
            Phase::InventoryAdmitted
        );
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), before);
    }

    #[tokio::test]
    async fn malformed_terminal_never_stages_an_otherwise_valid_record() {
        let f = Fixture::new().await;
        assert!(matches!(
            reconcile_session(
                &f.bd,
                &f.journal(),
                &f.source,
                session(&f.decisions, "LOOM_COMPLETE: {}"),
                None
            )
            .await
            .unwrap(),
            AgentOutcome::Failure { .. }
        ));
        assert!(f.journal().load(&f.source.id).unwrap().is_none());
        for id in &f.decisions {
            assert_eq!(f.bd.show(id).await.unwrap().status, Status::Open);
        }
    }
}

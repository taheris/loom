#[cfg(test)]
mod cases {
    use super::super::*;
    use loom_events::{AgentEvent, DriverKind};

    fn session(ids: &[BeadId]) -> SessionResult {
        let wire = format!(
            "LOOM_CLARIFY: {{\"decisions\":{}}}\nLOOM_NOOP",
            serde_json::to_string(ids).unwrap()
        );
        SessionResult::Decoded {
            outcome: SessionOutcome {
                exit_code: 0,
                cost_usd: None,
            },
            output: Arc::new(decode(&wire, OutputPhase::Loop)),
        }
    }

    fn events(f: &Fixture) -> Vec<AgentEvent> {
        std::fs::read_dir(f.dir.path().join(".loom/logs/decisions"))
            .unwrap()
            .flat_map(|entry| {
                std::fs::read_to_string(entry.unwrap().path())
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect::<Vec<AgentEvent>>()
            })
            .collect()
    }

    #[tokio::test]
    async fn native_decision_dispositions_replay_without_duplicate_effects_or_events() {
        let f = Fixture::new().await;
        let tracker = AdmissionTracker::default();
        let journal = f
            .journal()
            .with_admissions(tracker.clone())
            .with_clock(Arc::new(loom_driver::clock::MockClock::new()));
        assert_eq!(
            reconcile_session(&f.bd, &journal, &f.source, session(&f.decisions), None)
                .await
                .unwrap(),
            AgentOutcome::Success
        );
        let before = [
            f.bd.show(&f.decisions[0]).await.unwrap(),
            f.bd.show(&f.decisions[1]).await.unwrap(),
        ];
        let first = events(&f);
        assert_eq!(first.len(), 3);
        let selected = crate::logs_cmd::select_log(
            &f.dir.path().join(".loom/logs"),
            crate::logs_cmd::LogsOpts {
                bead: Some(&f.decisions[1]),
            },
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(selected).unwrap().lines().count(),
            2
        );
        for event in &first {
            let AgentEvent::DriverEvent {
                envelope,
                driver_kind,
                payload,
                ..
            } = event
            else {
                panic!("driver routing event required");
            };
            assert_eq!(envelope.ts_ms, 0, "timestamps use the injected clock");
            let id = envelope.bead_id.as_ref().unwrap();
            assert!(f.decisions.contains(id));
            assert_eq!(payload["bead_id"], serde_json::to_value(id).unwrap());
            assert_eq!(
                payload["producer"],
                serde_json::to_value(&f.source.id).unwrap()
            );
            if *driver_kind == DriverKind::ClarifyDowngraded {
                assert_eq!(id, &f.decisions[1]);
            }
        }
        let restarted = f.journal().with_admissions(tracker.clone());
        reconcile_session(&f.bd, &restarted, &f.source, session(&f.decisions), None)
            .await
            .unwrap();
        assert_eq!(events(&f), first);
        assert_eq!(tracker.ids().unwrap().len(), 2);
        for (id, snapshot) in f.decisions.iter().zip(before) {
            assert_eq!(f.bd.show(id).await.unwrap(), snapshot);
        }
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), f.source);
    }

    #[tokio::test]
    async fn native_restart_recovers_planned_postimage_and_partial_event_delivery() {
        let f = Fixture::new().await;
        reconcile_session(&f.bd, &f.journal(), &f.source, session(&f.decisions), None)
            .await
            .unwrap();
        let repaired = f.bd.show(&f.decisions[1]).await.unwrap();
        let mut entry = f.journal().load(&f.source.id).unwrap().unwrap();
        entry.phase = Phase::InventoryPending;
        f.journal().save(&entry).unwrap();
        for entry in
            std::fs::read_dir(f.dir.path().join(".loom/decision-waits/dispositions")).unwrap()
        {
            let path = entry.unwrap().path();
            let mut value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            if value["snapshot"]["id"] == serde_json::to_value(&f.decisions[1]).unwrap() {
                value["effect"] = "Planned".into();
                std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
            }
        }
        for entry in std::fs::read_dir(f.dir.path().join(".loom/logs/decisions")).unwrap() {
            let path = entry.unwrap().path();
            let body = std::fs::read_to_string(&path).unwrap();
            if body.lines().count() == 2 {
                let prefix = body.lines().next().unwrap().len() + 1 + 17;
                std::fs::write(path, &body.as_bytes()[..prefix]).unwrap();
            }
        }
        let tracker = AdmissionTracker::default();
        assert!(
            !held(
                &f.bd,
                &f.journal().with_admissions(tracker.clone()),
                &f.source
            )
            .await
            .unwrap()
        );
        assert_eq!(f.bd.show(&f.decisions[1]).await.unwrap(), repaired);
        assert_eq!(events(&f).len(), 3);
        assert_eq!(tracker.ids().unwrap().len(), 2);
        assert_eq!(
            f.journal().load(&f.source.id).unwrap().unwrap().phase,
            Phase::InventoryAdmitted
        );
    }

    #[tokio::test]
    async fn native_compatible_producer_reuses_exact_decision_and_preserves_closed_answer() {
        let f = Fixture::new().await;
        f.bd.dep_add(&f.sibling, &f.decisions[0]).await.unwrap();
        f.bd.update(
            &f.sibling,
            UpdateOpts {
                status: Some(Status::InProgress),
                ..UpdateOpts::default()
            },
        )
        .await
        .unwrap();
        let tracker = AdmissionTracker::default();
        let journal = f.journal().with_admissions(tracker.clone());
        reconcile_session(&f.bd, &journal, &f.source, session(&f.decisions), None)
            .await
            .unwrap();
        let sibling = f.bd.show(&f.sibling).await.unwrap();
        let original = f.bd.show(&f.decisions[0]).await.unwrap();
        assert_eq!(
            reconcile_session(&f.bd, &journal, &sibling, session(&f.decisions[..1]), None)
                .await
                .unwrap(),
            AgentOutcome::Success
        );
        assert_eq!(tracker.ids().unwrap().len(), 2);
        assert_eq!(f.bd.show(&f.decisions[0]).await.unwrap(), original);
        assert_eq!(events(&f).len(), 3);
        f.bd.close(&f.decisions[0], Some("human answered the original choice"))
            .await
            .unwrap();
        let answered = f.bd.show(&f.decisions[0]).await.unwrap();
        let restart = AdmissionTracker::default();
        assert_eq!(
            reconcile_session(
                &f.bd,
                &f.journal().with_admissions(restart.clone()),
                &sibling,
                session(&f.decisions[..1]),
                None
            )
            .await
            .unwrap(),
            AgentOutcome::Success
        );
        assert!(restart.ids().unwrap().is_empty());
        assert_eq!(f.bd.show(&f.decisions[0]).await.unwrap(), answered);
        assert_eq!(events(&f).len(), 3);
    }

    #[tokio::test]
    async fn native_misparented_candidate_claim_cannot_hide_from_producer_inventory() {
        let f = Fixture::new().await;
        let id = f.bd.create(CreateOpts {
            title: "misparented candidate".into(), issue_type: Some(IssueType::Decision),
            status: Some(Status::Blocked), parent: Some(f.sibling.clone()), description: BRIEF.into(),
            metadata: Some(serde_json::json!({"loom.decision_candidate": {"version":1,"producer":f.source.id,"root":f.root,"affected_work":[]}}).to_string()),
            ..CreateOpts::default()
        }).await.unwrap();
        let before = [
            f.bd.show(&f.decisions[0]).await.unwrap(),
            f.bd.show(&f.decisions[1]).await.unwrap(),
            f.bd.show(&id).await.unwrap(),
        ];
        let outcome =
            reconcile_session(&f.bd, &f.journal(), &f.source, session(&f.decisions), None)
                .await
                .unwrap();
        assert!(matches!(outcome, AgentOutcome::Failure { error } if error.contains(id.as_str())));
        for snapshot in before {
            assert_eq!(f.bd.show(&snapshot.id).await.unwrap(), snapshot);
        }
        assert!(f.journal().load(&f.source.id).unwrap().is_none());
    }

    #[tokio::test]
    async fn native_corrupt_brief_disposition_is_retained_and_rejected_without_queue_writes() {
        let f = Fixture::new().await;
        reconcile_session(&f.bd, &f.journal(), &f.source, session(&f.decisions), None)
            .await
            .unwrap();
        let before = f.bd.show(&f.decisions[0]).await.unwrap();
        let mut corrupt = None;
        for entry in
            std::fs::read_dir(f.dir.path().join(".loom/decision-waits/dispositions")).unwrap()
        {
            let path = entry.unwrap().path();
            let mut value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            if value["snapshot"]["id"] == serde_json::to_value(&f.decisions[0]).unwrap() {
                value["report"]["evidence_hash"] = "not-the-admitted-brief".into();
                let bytes = serde_json::to_vec(&value).unwrap();
                std::fs::write(&path, &bytes).unwrap();
                corrupt = Some((path, bytes));
            }
        }
        assert!(f.journal().blocks_scope(&f.bd, &f.root).await.is_err());
        assert!(matches!(
            reconcile_session(&f.bd, &f.journal(), &f.source, session(&f.decisions), None)
                .await
                .unwrap(),
            AgentOutcome::Failure { .. }
        ));
        let (path, bytes) = corrupt.unwrap();
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert_eq!(f.bd.show(&f.decisions[0]).await.unwrap(), before);
        assert_eq!(
            f.journal().load(&f.source.id).unwrap().unwrap().phase,
            Phase::InventoryAdmitted
        );
        assert!(f.journal().blocks_scope(&f.bd, &f.root).await.is_err());
    }

    #[tokio::test]
    async fn native_admitted_disposition_loss_is_not_reconstructed_from_projection() {
        let f = Fixture::new().await;
        reconcile_session(&f.bd, &f.journal(), &f.source, session(&f.decisions), None)
            .await
            .unwrap();
        let directory = f.dir.path().join(".loom/decision-waits/dispositions");
        let path = std::fs::read_dir(directory)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::remove_file(&path).unwrap();
        let before = f.bd.show(&f.decisions[0]).await.unwrap();
        assert!(f.journal().blocks_scope(&f.bd, &f.root).await.is_err());
        assert!(
            f.journal()
                .recover_inventory(
                    &f.bd,
                    Some(&f.root),
                    &loom_driver::identifier::SpecLabel::new("loop").unwrap()
                )
                .await
                .is_err()
        );
        assert!(!path.exists());
        assert_eq!(f.bd.show(&f.decisions[0]).await.unwrap(), before);
        assert_eq!(
            f.journal().load(&f.source.id).unwrap().unwrap().phase,
            Phase::InventoryAdmitted
        );
    }

    #[tokio::test]
    async fn native_todo_inventory_rejects_omission_then_stages_only_dedicated_decisions() {
        let f = Fixture::new().await;
        f.bd.update(
            &f.root,
            UpdateOpts {
                add_labels: vec!["loom:todo".into()],
                ..UpdateOpts::default()
            },
        )
        .await
        .unwrap();
        let epic = f.bd.show(&f.root).await.unwrap();
        let mut ids = Vec::new();
        for description in [BRIEF, "incomplete decision brief"] {
            let id =
                f.bd.create(CreateOpts {
                    title: "Todo implementation decision".into(),
                    issue_type: Some(IssueType::Decision),
                    parent: Some(f.root.clone()),
                    description: description.into(),
                    ..CreateOpts::default()
                })
                .await
                .unwrap();
            f.bd.dep_add(&f.sibling, &id).await.unwrap();
            ids.push(id);
        }
        let snapshots = [
            f.bd.show(&ids[0]).await.unwrap(),
            f.bd.show(&ids[1]).await.unwrap(),
        ];
        assert!(
            super::super::super::admit_todo(&f.bd, &f.journal(), &epic, &HashSet::new())
                .await
                .is_err()
        );
        for (id, before) in ids.iter().zip(snapshots) {
            assert_eq!(f.bd.show(id).await.unwrap(), before);
        }
        let admitted = super::super::super::admit_todo(
            &f.bd,
            &f.journal(),
            &epic,
            &ids.iter().cloned().collect(),
        )
        .await
        .unwrap();
        assert_eq!(admitted.len(), 2);
        assert_eq!(f.bd.show(&f.root).await.unwrap(), epic);
        assert!(
            f.bd.show(&ids[0])
                .await
                .unwrap()
                .labels
                .iter()
                .any(|label| label.as_str() == "loom:clarify")
        );
        assert!(
            f.bd.show(&ids[1])
                .await
                .unwrap()
                .labels
                .iter()
                .any(|label| label.as_str() == "loom:blocked")
        );
        for event in events(&f) {
            let AgentEvent::DriverEvent { payload, .. } = event else {
                panic!("driver event")
            };
            assert_eq!(payload["source_route"], "todo-marker");
        }
    }
}

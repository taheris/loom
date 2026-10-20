#[cfg(test)]
mod cases {
    use super::super::*;
    use crate::r#loop::{AgentOutcome, WaitPrerequisites};
    use crate::test_support::{IsolatedBeads, client};
    use loom_driver::bd::{BdError, CreateOpts, ReadyOpts, RunOutput};
    use std::ffi::OsString;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    struct Fixture {
        directory: tempfile::TempDir,
        bd: BdClient<IsolatedBeads>,
        root: BeadId,
        source: Bead,
        blockers: Vec<BeadId>,
        sibling: BeadId,
    }

    impl Fixture {
        async fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let bd = client(directory.path()).await;
            let root = bd
                .create(CreateOpts {
                    title: "native wait root".into(),
                    issue_type: Some(IssueType::Epic),
                    labels: vec!["spec:loop".into()],
                    ..CreateOpts::default()
                })
                .await
                .unwrap();
            let source = bd
                .create(CreateOpts {
                    title: "waiting work".into(),
                    parent: Some(root.clone()),
                    notes: Some("retained draft".into()),
                    metadata: Some(r#"{"operator.note":"keep"}"#.into()),
                    ..CreateOpts::default()
                })
                .await
                .unwrap();
            let sibling = bd
                .create(CreateOpts {
                    title: "independent ready work".into(),
                    parent: Some(root.clone()),
                    ..CreateOpts::default()
                })
                .await
                .unwrap();
            let mut blockers = Vec::new();
            for title in ["first prerequisite", "second prerequisite"] {
                let id = bd
                    .create(CreateOpts {
                        title: title.into(),
                        parent: Some(root.clone()),
                        ..CreateOpts::default()
                    })
                    .await
                    .unwrap();
                bd.dep_add(&source, &id).await.unwrap();
                blockers.push(id);
            }
            let source = bd.show(&source).await.unwrap();
            Self {
                directory,
                bd,
                root,
                source,
                blockers,
                sibling,
            }
        }

        fn journal(&self) -> DecisionWaitJournal {
            DecisionWaitJournal::new(self.directory.path())
        }

        async fn admit(&self) {
            let outcome = crate::r#loop::reconcile_session(
                &self.bd,
                &self.journal(),
                &self.source,
                crate::r#loop::SessionResult::Decoded {
                    outcome: loom_driver::agent::SessionOutcome {
                        exit_code: 0,
                        cost_usd: None,
                    },
                    output: Arc::new(loom_protocol::output::decode(
                        "LOOM_WAITING",
                        loom_protocol::output::Phase::Loop,
                    )),
                },
                None,
            )
            .await
            .unwrap();
            assert!(
                matches!(outcome, AgentOutcome::Waiting { blockers: WaitPrerequisites::Native(ref blockers) } if blockers.count() == 2),
                "{outcome:?}"
            );
        }

        async fn close(&self, id: &BeadId) {
            self.bd
                .close(id, Some("fixture prerequisite completed"))
                .await
                .unwrap();
        }

        async fn refresh(&self) {
            self.journal()
                .refresh_native_waits(&self.bd, Some(&self.root), None)
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn native_attributed_wait_preserves_ready_siblings() {
        let f = Fixture::new().await;
        let clone = f
            .directory
            .path()
            .join(".loom/beads")
            .join(f.source.id.as_str());
        std::fs::create_dir_all(&clone).unwrap();
        std::fs::write(clone.join("draft"), "uncommitted source").unwrap();
        f.admit().await;
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), f.source);
        assert!(
            !f.bd
                .show(&f.source.id)
                .await
                .unwrap()
                .metadata
                .contains_key(KEY)
        );
        assert_eq!(
            f.journal()
                .db()
                .unwrap()
                .records::<serde_json::Value>(RecordKind::DependencyWait)
                .unwrap(),
            []
        );
        let ready =
            f.bd.ready(ReadyOpts {
                parent: Some(f.root.clone()),
                ..ReadyOpts::default()
            })
            .await
            .unwrap();
        assert!(ready.iter().any(|bead| bead.id == f.sibling));
        assert!(!ready.iter().any(|bead| bead.id == f.source.id));
        for blocker in &f.blockers {
            f.close(blocker).await;
        }
        let ready =
            f.bd.ready(ReadyOpts {
                parent: Some(f.root.clone()),
                ..ReadyOpts::default()
            })
            .await
            .unwrap();
        assert!(ready.iter().any(|bead| bead.id == f.source.id));
        assert_eq!(
            std::fs::read_to_string(clone.join("draft")).unwrap(),
            "uncommitted source"
        );
    }

    #[tokio::test]
    async fn cold_machine_uses_current_dependencies_without_local_permission() {
        let f = Fixture::new().await;
        f.admit().await;
        let second_machine = tempfile::tempdir().unwrap();
        let journal = DecisionWaitJournal::new(second_machine.path());
        assert!(journal.native_held(&f.bd, &f.source).await.unwrap());
        assert!(journal.blocks_scope(&f.bd, &f.root).await.unwrap());
        for blocker in &f.blockers {
            f.close(blocker).await;
        }
        assert!(!journal.native_held(&f.bd, &f.source).await.unwrap());
        assert!(!journal.blocks_scope(&f.bd, &f.root).await.unwrap());
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), f.source);
    }

    #[tokio::test]
    async fn explicit_native_wait_releases_only_the_ended_workers_claim() {
        let mut f = Fixture::new().await;
        f.bd.update(
            &f.source.id,
            UpdateOpts {
                status: Some(Status::InProgress),
                ..UpdateOpts::default()
            },
        )
        .await
        .unwrap();
        f.source = f.bd.show(&f.source.id).await.unwrap();
        f.admit().await;
        let current = f.bd.show(&f.source.id).await.unwrap();
        assert_eq!(current.status, Status::Open);
        assert_eq!(current.notes, f.source.notes);
        assert_eq!(current.metadata, f.source.metadata);
        assert!(f.journal().blocks_scope(&f.bd, &f.root).await.unwrap());
    }

    #[tokio::test]
    async fn cold_machine_never_reopens_independent_human_holds() {
        for status in [Status::Blocked, Status::Deferred, Status::Closed] {
            let f = Fixture::new().await;
            f.admit().await;
            if status == Status::Closed {
                for blocker in &f.blockers {
                    f.close(blocker).await;
                }
            }
            f.bd.update(
                &f.source.id,
                UpdateOpts {
                    status: Some(status),
                    add_labels: vec!["loom:blocked".into()],
                    append_notes: Some("human outcome".into()),
                    ..UpdateOpts::default()
                },
            )
            .await
            .unwrap();
            let held = f.bd.show(&f.source.id).await.unwrap();
            if status != Status::Closed {
                for blocker in &f.blockers {
                    f.close(blocker).await;
                }
            }
            let second_machine = tempfile::tempdir().unwrap();
            DecisionWaitJournal::new(second_machine.path())
                .refresh_native_waits(&f.bd, Some(&f.root), None)
                .await
                .unwrap();
            assert_eq!(f.bd.show(&f.source.id).await.unwrap(), held);
        }
    }

    #[tokio::test]
    async fn current_graph_does_not_require_a_removed_historical_dependency() {
        let f = Fixture::new().await;
        f.admit().await;
        let output = IsolatedBeads::attach(f.directory.path())
            .run(
                [
                    "dep",
                    "remove",
                    f.source.id.as_str(),
                    f.blockers[0].as_str(),
                ]
                .into_iter()
                .map(OsString::from)
                .collect(),
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        assert!(output.success());
        f.close(&f.blockers[1]).await;
        f.refresh().await;
        assert!(!f.journal().blocks_scope(&f.bd, &f.root).await.unwrap());
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), f.source);
    }

    #[tokio::test]
    async fn native_wait_rejects_unowned_projection_without_reconstructing_authority() {
        let f = Fixture::new().await;
        f.bd.update(
            &f.source.id,
            UpdateOpts {
                status: Some(Status::Blocked),
                set_metadata: vec![(KEY.into(), "{}".into())],
                ..UpdateOpts::default()
            },
        )
        .await
        .unwrap();
        let before = f.bd.show(&f.source.id).await.unwrap();
        assert!(f.journal().admit_native_wait(&f.bd, &before).await.is_err());
        assert!(f.journal().native_held(&f.bd, &before).await.is_err());
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), before);
    }

    #[tokio::test]
    async fn closed_legacy_native_wait_preserves_history_without_blocking_loop() {
        for phase in [None, Some(Phase::Cancelled), Some(Phase::Active)] {
            let f = Fixture::new().await;
            let record = Record {
                version: 1,
                source: f.source.id.clone(),
                root: f.root.clone(),
                parent: f.source.parent.clone(),
                kind: f.source.issue_type,
                initial_status: Status::InProgress,
                blockers: f.blockers.clone(),
            };
            for blocker in &f.blockers {
                f.close(blocker).await;
            }
            f.bd.update(
                &f.source.id,
                UpdateOpts {
                    set_metadata: vec![(KEY.into(), serde_json::to_string(&record).unwrap())],
                    ..UpdateOpts::default()
                },
            )
            .await
            .unwrap();
            f.close(&f.source.id).await;
            let journal = f.journal();
            if let Some(phase) = phase {
                journal
                    .db()
                    .unwrap()
                    .put_record(
                        RecordKind::DependencyWait,
                        f.source.id.as_str(),
                        &Entry {
                            record,
                            phase,
                            additional_blockers: vec![],
                        },
                    )
                    .unwrap();
            }
            let before = f.bd.show(&f.source.id).await.unwrap();
            let records = journal
                .db()
                .unwrap()
                .records::<serde_json::Value>(RecordKind::DependencyWait)
                .unwrap();
            f.refresh().await;
            assert!(
                !super::super::super::held(&f.bd, &journal, &before)
                    .await
                    .unwrap()
            );
            assert!(!journal.blocks_scope(&f.bd, &f.root).await.unwrap());
            assert_eq!(f.bd.show(&f.source.id).await.unwrap(), before);
            assert_eq!(
                journal
                    .db()
                    .unwrap()
                    .records::<serde_json::Value>(RecordKind::DependencyWait)
                    .unwrap(),
                records
            );
        }
    }

    struct LostAck {
        native: IsolatedBeads,
        armed: Arc<AtomicBool>,
    }

    impl CommandRunner for LostAck {
        async fn run(&self, args: Vec<OsString>, timeout: Duration) -> Result<RunOutput, BdError> {
            let update = args.first().is_some_and(|arg| arg == "update");
            let output = self.native.run(args, timeout).await?;
            if update && output.success() && self.armed.swap(false, Ordering::SeqCst) {
                return Err(BdError::Cleanup(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "lost acknowledgement after committed native effect",
                )));
            }
            Ok(output)
        }
    }

    #[tokio::test]
    async fn native_wait_restart_reconciles_lost_claim_release_ack() {
        let mut f = Fixture::new().await;
        f.bd.update(
            &f.source.id,
            UpdateOpts {
                status: Some(Status::InProgress),
                ..UpdateOpts::default()
            },
        )
        .await
        .unwrap();
        f.source = f.bd.show(&f.source.id).await.unwrap();
        let bd = BdClient::with_runner(LostAck {
            native: IsolatedBeads::attach(f.directory.path()),
            armed: Arc::new(AtomicBool::new(true)),
        });
        assert!(f.journal().admit_native_wait(&bd, &f.source).await.is_err());
        assert_eq!(f.bd.show(&f.source.id).await.unwrap().status, Status::Open);
        let restarted = DecisionWaitJournal::new(f.directory.path());
        assert_eq!(
            restarted
                .admit_native_wait(&f.bd, &f.source)
                .await
                .unwrap()
                .count(),
            2
        );
        for blocker in &f.blockers {
            f.close(blocker).await;
        }
        assert!(!restarted.blocks_scope(&f.bd, &f.root).await.unwrap());
    }
}

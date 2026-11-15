#[cfg(test)]
mod cases {
    use super::super::*;
    use crate::r#loop::{AgentOutcome, WaitPrerequisites};
    use crate::test_support::{IsolatedBeads, client};
    use loom_driver::bd::{BdClient, BdError, CommandRunner, RunOutput};
    use loom_issues::{
        memory::Backend,
        task::{Create, Label, Ready, Relation},
    };
    use std::ffi::OsString;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    struct Fixture<T: Store = Backend> {
        directory: tempfile::TempDir,
        bd: T,
        root: BeadId,
        source: Bead,
        blockers: Vec<BeadId>,
        sibling: BeadId,
    }

    impl Fixture<Backend> {
        async fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            loom_driver::git::initialize_snapshot(
                directory.path(),
                &loom_driver::clock::SystemClock::new(),
            )
            .await
            .unwrap();
            Self::with_store(directory, Backend::default()).await
        }
    }

    impl Fixture<BdClient<IsolatedBeads>> {
        async fn new_native() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let bd = client(directory.path()).await;
            Self::with_store(directory, bd).await
        }
    }

    impl<T: Store> Fixture<T> {
        async fn with_store(directory: tempfile::TempDir, bd: T) -> Self {
            let root = bd
                .create(Create {
                    title: "dependency wait root".into(),
                    kind: Some(IssueType::Epic),
                    labels: vec![Label::new("spec:loop").unwrap()],
                    ..Create::default()
                })
                .await
                .unwrap();
            let source = bd
                .create(Create {
                    title: "waiting work".into(),
                    parent: Some(root.clone()),
                    notes: Some("retained draft".into()),
                    metadata: [("operator.note".into(), "keep".into())].into(),
                    ..Create::default()
                })
                .await
                .unwrap();
            let sibling = bd
                .create(Create {
                    title: "independent ready work".into(),
                    parent: Some(root.clone()),
                    ..Create::default()
                })
                .await
                .unwrap();
            let mut blockers = Vec::new();
            for title in ["first prerequisite", "second prerequisite"] {
                let id = bd
                    .create(Create {
                        title: title.into(),
                        parent: Some(root.clone()),
                        ..Create::default()
                    })
                    .await
                    .unwrap();
                bd.relate(&source, &id, Relation::Blocks).await.unwrap();
                blockers.push(id);
            }
            let source = bd.get(&source).await.unwrap();
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
        assert_eq!(f.bd.get(&f.source.id).await.unwrap(), f.source);
        assert!(
            !f.bd
                .get(&f.source.id)
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
            f.bd.ready(Ready {
                parent: Some(f.root.clone()),
                ..Ready::default()
            })
            .await
            .unwrap();
        assert!(ready.iter().any(|bead| bead.id == f.sibling));
        assert!(!ready.iter().any(|bead| bead.id == f.source.id));
        for blocker in &f.blockers {
            f.close(blocker).await;
        }
        let ready =
            f.bd.ready(Ready {
                parent: Some(f.root.clone()),
                ..Ready::default()
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
        assert_eq!(f.bd.get(&f.source.id).await.unwrap(), f.source);
    }

    #[tokio::test]
    async fn explicit_native_wait_releases_only_the_ended_workers_claim() {
        let mut f = Fixture::new().await;
        f.bd.update(
            &f.source.id,
            Update {
                status: Some(Status::InProgress),
                ..Update::default()
            },
            Guard::None,
        )
        .await
        .unwrap();
        f.source = f.bd.get(&f.source.id).await.unwrap();
        f.admit().await;
        let current = f.bd.get(&f.source.id).await.unwrap();
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
                Update {
                    status: Some(status),
                    add_labels: vec![Label::new("loom:blocked").unwrap()],
                    append_notes: Some("human outcome".into()),
                    ..Update::default()
                },
                Guard::None,
            )
            .await
            .unwrap();
            let held = f.bd.get(&f.source.id).await.unwrap();
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
            assert_eq!(f.bd.get(&f.source.id).await.unwrap(), held);
        }
    }

    #[tokio::test]
    async fn current_graph_does_not_require_a_removed_historical_dependency() {
        // Dependency removal is a native adapter edge, not a Store operation.
        let f = Fixture::<BdClient<IsolatedBeads>>::new_native().await;
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
        assert_eq!(f.bd.get(&f.source.id).await.unwrap(), f.source);
    }

    #[tokio::test]
    async fn native_wait_rejects_unowned_projection_without_reconstructing_authority() {
        let f = Fixture::new().await;
        f.bd.update(
            &f.source.id,
            Update {
                status: Some(Status::Blocked),
                set_metadata: [(KEY.into(), "{}".into())].into(),
                ..Update::default()
            },
            Guard::None,
        )
        .await
        .unwrap();
        let before = f.bd.get(&f.source.id).await.unwrap();
        assert!(f.journal().admit_native_wait(&f.bd, &before).await.is_err());
        assert!(f.journal().native_held(&f.bd, &before).await.is_err());
        assert_eq!(f.bd.get(&f.source.id).await.unwrap(), before);
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
                Update {
                    set_metadata: [(KEY.into(), serde_json::to_string(&record).unwrap().into())]
                        .into(),
                    ..Update::default()
                },
                Guard::None,
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
            let before = f.bd.get(&f.source.id).await.unwrap();
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
            assert_eq!(f.bd.get(&f.source.id).await.unwrap(), before);
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
        let mut f = Fixture::<BdClient<IsolatedBeads>>::new_native().await;
        Store::update(
            &f.bd,
            &f.source.id,
            Update {
                status: Some(Status::InProgress),
                ..Update::default()
            },
            Guard::None,
        )
        .await
        .unwrap();
        f.source = f.bd.get(&f.source.id).await.unwrap();
        let bd = BdClient::with_runner(LostAck {
            native: IsolatedBeads::attach(f.directory.path()),
            armed: Arc::new(AtomicBool::new(true)),
        });
        assert!(f.journal().admit_native_wait(&bd, &f.source).await.is_err());
        assert_eq!(f.bd.get(&f.source.id).await.unwrap().status, Status::Open);
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

#[cfg(test)]
mod cases {
    mod affected;
    mod ownership;
    mod safety;

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
    async fn native_attributed_wait_parks_and_releases_without_queue_labels() {
        let f = Fixture::new().await;
        f.admit().await;
        let parked = f.bd.show(&f.source.id).await.unwrap();
        assert_eq!(parked.status, Status::Blocked);
        assert_eq!(parked.notes, f.source.notes);
        assert_eq!(
            parked.metadata["operator.note"],
            f.source.metadata["operator.note"]
        );
        assert_eq!(parked.labels, f.source.labels);
        assert!(parked.metadata.contains_key(KEY));
        assert!(f.journal().blocks_scope(&f.bd, &f.root).await.unwrap());
        let repeat = f
            .journal()
            .admit_native_wait(&f.bd, &f.source)
            .await
            .unwrap();
        assert_eq!(repeat.count(), 2);
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), parked);
        f.close(&f.blockers[0]).await;
        f.refresh().await;
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), parked);
        let third =
            f.bd.create(CreateOpts {
                title: "late prerequisite".into(),
                parent: Some(f.root.clone()),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        f.bd.dep_add(&f.source.id, &third).await.unwrap();
        f.close(&f.blockers[1]).await;
        f.refresh().await;
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), parked);
        f.close(&third).await;
        f.refresh().await;
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), f.source);
        assert!(!f.journal().blocks_scope(&f.bd, &f.root).await.unwrap());
        f.refresh().await;
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), f.source);
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
        let batch = crate::r#loop::schedule::parallel_ready_batch(
            &f.bd,
            f.directory.path(),
            &SpecLabel::new("loop").unwrap(),
            Some(&f.root),
            20,
            &[],
            &HashSet::new(),
        )
        .await
        .unwrap();
        assert!(batch.iter().any(|bead| bead.id == f.sibling));
        assert!(!batch.iter().any(|bead| bead.id == f.source.id));
        f.close(&f.blockers[0]).await;
        f.close(&f.blockers[1]).await;
        let batch = crate::r#loop::schedule::parallel_ready_batch(
            &f.bd,
            f.directory.path(),
            &SpecLabel::new("loop").unwrap(),
            Some(&f.root),
            20,
            &[],
            &HashSet::new(),
        )
        .await
        .unwrap();
        assert!(batch.iter().any(|bead| bead.id == f.source.id));
        assert_eq!(
            std::fs::read_to_string(clone.join("draft")).unwrap(),
            "uncommitted source"
        );
        let ready =
            f.bd.ready(ReadyOpts {
                parent: Some(f.root),
                ..ReadyOpts::default()
            })
            .await
            .unwrap();
        assert!(ready.iter().any(|bead| bead.id == f.source.id));
    }

    #[tokio::test]
    async fn native_wait_release_preserves_human_takeover_and_unrelated_metadata() {
        for takeover in ["label", "cancel", "replace", "defer"] {
            let f = Fixture::new().await;
            f.admit().await;
            let update = match takeover {
                "label" => UpdateOpts {
                    add_labels: vec!["loom:blocked".into()],
                    ..UpdateOpts::default()
                },
                "cancel" => UpdateOpts {
                    unset_metadata: vec![KEY.into()],
                    ..UpdateOpts::default()
                },
                "replace" => UpdateOpts {
                    set_metadata: vec![(KEY.into(), "{}".into())],
                    ..UpdateOpts::default()
                },
                "defer" => UpdateOpts {
                    status: Some(Status::Deferred),
                    ..UpdateOpts::default()
                },
                _ => panic!("closed takeover set"),
            };
            f.bd.update(
                &f.source.id,
                UpdateOpts {
                    append_notes: Some("human outcome".into()),
                    ..update
                },
            )
            .await
            .unwrap();
            f.close(&f.blockers[0]).await;
            f.close(&f.blockers[1]).await;
            let before = f.bd.show(&f.source.id).await.unwrap();
            let result = f.journal().refresh_native_waits(&f.bd, None, None).await;
            if takeover == "replace" {
                assert!(result.is_err());
            } else {
                result.unwrap();
            }
            assert_eq!(f.bd.show(&f.source.id).await.unwrap(), before);
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
    async fn native_wait_restart_recovers_projection_and_release_after_lost_ack() {
        let f = Fixture::new().await;
        let armed = Arc::new(AtomicBool::new(true));
        let bd = BdClient::with_runner(LostAck {
            native: IsolatedBeads::attach(f.directory.path()),
            armed: armed.clone(),
        });
        assert!(f.journal().admit_native_wait(&bd, &f.source).await.is_err());
        assert_eq!(
            f.bd.show(&f.source.id).await.unwrap().status,
            Status::Blocked
        );
        f.refresh().await;
        assert_eq!(
            f.bd.show(&f.source.id).await.unwrap().status,
            Status::Blocked
        );
        f.close(&f.blockers[0]).await;
        f.close(&f.blockers[1]).await;
        armed.store(true, Ordering::SeqCst);
        assert!(
            f.journal()
                .refresh_native_waits(&bd, None, None)
                .await
                .is_err()
        );
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), f.source);
        f.refresh().await;
        assert!(!f.journal().blocks_scope(&f.bd, &f.root).await.unwrap());
    }

    #[tokio::test]
    async fn native_wait_rejects_unowned_projection_without_reconstructing_authority() {
        let f = Fixture::new().await;
        f.bd.update(
            &f.source.id,
            UpdateOpts {
                set_metadata: vec![(KEY.into(), "{}".into())],
                ..UpdateOpts::default()
            },
        )
        .await
        .unwrap();
        let forged = f.bd.show(&f.source.id).await.unwrap();
        assert!(f.journal().native_held(&f.bd, &forged).await.is_err());
        assert!(f.journal().admit_native_wait(&f.bd, &forged).await.is_err());
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), forged);
        assert!(
            !f.journal().native().directory.exists()
                || std::fs::read_dir(f.journal().native().directory)
                    .unwrap()
                    .all(|entry| entry
                        .unwrap()
                        .path()
                        .extension()
                        .is_none_or(|ext| ext != "json"))
        );
    }
}

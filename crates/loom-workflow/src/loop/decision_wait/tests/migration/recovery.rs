#[cfg(test)]
mod cases {
    use super::super::*;

    struct LostMigrationAck {
        inner: IsolatedBeads,
        dependency: bool,
        fired: std::sync::atomic::AtomicBool,
    }
    impl CommandRunner for LostMigrationAck {
        async fn run(&self, args: Vec<OsString>, timeout: Duration) -> Result<RunOutput, BdError> {
            let target = if self.dependency {
                args.first().is_some_and(|arg| arg == "dep")
                    && args.get(1).is_some_and(|arg| arg == "add")
            } else {
                args.first().is_some_and(|arg| arg == "update")
                    && args.iter().any(|arg| arg == "--parent")
            };
            let output = self.inner.run(args, timeout).await?;
            if target
                && output.success()
                && !self.fired.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                return Ok(RunOutput {
                    status: 1,
                    stdout: Vec::new(),
                    stderr: b"lost native migration acknowledgement".to_vec(),
                });
            }
            Ok(output)
        }
    }

    #[tokio::test]
    async fn migration_replays_lost_reparent_and_prerequisite_acknowledgements() {
        for dependency in [false, true] {
            let f = Fixture::new().await;
            let failing = BdClient::with_runner(LostMigrationAck {
                inner: IsolatedBeads::attach(f.dir.path()),
                dependency,
                fired: std::sync::atomic::AtomicBool::new(false),
            });
            assert!(
                reconcile_session(
                    &failing,
                    &f.journal(),
                    &f.source,
                    Fixture::session(&f.decisions),
                    None
                )
                .await
                .is_err()
            );
            let journal = DecisionWaitJournal::new(f.dir.path());
            journal
                .recover_inventory(&f.bd, Some(&f.root), &SpecLabel::new("loop").unwrap())
                .await
                .unwrap();
            journal
                .recover_inventory(&f.bd, Some(&f.root), &SpecLabel::new("loop").unwrap())
                .await
                .unwrap();
            assert_eq!(f.bd.show(&f.source.id).await.unwrap().status, Status::Open);
            assert_eq!(
                f.bd.dependency_snapshot(&f.source.id)
                    .await
                    .unwrap()
                    .active_blockers()
                    .into_iter()
                    .collect::<HashSet<_>>(),
                f.decisions.iter().cloned().collect::<HashSet<_>>()
            );
            for id in &f.decisions {
                assert_eq!(f.bd.show(id).await.unwrap().parent.as_ref(), Some(&f.root));
            }
        }
    }

    #[tokio::test]
    async fn conflicting_human_edit_stops_interrupted_migration_without_overwrite() {
        let f = Fixture::new().await;
        let failing = BdClient::with_runner(InterruptReparent(IsolatedBeads::attach(f.dir.path())));
        assert!(
            reconcile_session(
                &failing,
                &f.journal(),
                &f.source,
                Fixture::session(&f.decisions),
                None
            )
            .await
            .is_err()
        );
        f.bd.update(
            &f.decisions[0],
            UpdateOpts {
                notes: Some("human changed migration context".into()),
                ..UpdateOpts::default()
            },
        )
        .await
        .unwrap();
        let before = f.bd.show(&f.decisions[0]).await.unwrap();
        assert!(
            f.journal()
                .recover_inventory(&f.bd, Some(&f.root), &SpecLabel::new("loop").unwrap())
                .await
                .is_err()
        );
        assert_eq!(f.bd.show(&f.decisions[0]).await.unwrap(), before);
        assert!(f.journal().has_pending_for(&f.root).unwrap());
    }

    #[tokio::test]
    async fn legacy_hold_migration_preserves_canonical_dispositions_and_event_identity() {
        let f = Fixture::new().await;
        let mut decisions = Vec::new();
        for id in &f.decisions {
            decisions.push(f.bd.show(id).await.unwrap());
        }
        let context = crate::r#loop::decision_wait::graph::resolve(
            &f.bd, &f.source, &f.root, &decisions, None,
        )
        .await
        .unwrap();
        let entry = Entry {
            record: Record {
                version: 1,
                source: f.source.id.clone(),
                root: f.root.clone(),
                decisions: f.decisions.clone(),
            },
            phase: Phase::Active,
            context: Some(context),
            native_wait: false,
        };
        f.journal().save(&entry).unwrap();
        f.bd.update(
            &f.source.id,
            UpdateOpts {
                add_labels: vec![HOLD_LABEL.into()],
                set_metadata: vec![(
                    HOLD_KEY.into(),
                    serde_json::to_string(&entry.record).unwrap(),
                )],
                ..UpdateOpts::default()
            },
        )
        .await
        .unwrap();
        crate::r#loop::decision_wait::disposition::stage(
            &f.bd,
            &f.journal(),
            &f.source.id,
            &decisions,
        )
        .await
        .unwrap();
        let directory = f.dir.path().join(".loom/logs/decisions");
        let events = || {
            let mut files: Vec<_> = std::fs::read_dir(&directory)
                .unwrap()
                .map(|file| {
                    let path = file.unwrap().path();
                    (path.clone(), std::fs::read(path).unwrap())
                })
                .collect();
            files.sort();
            files
        };
        let before = events();
        assert!(held(&f.bd, &f.journal(), &f.source).await.unwrap());
        assert_eq!(events(), before);
        for id in &f.decisions {
            assert!(f.journal().has_disposition(&f.source.id, id).unwrap());
            assert_eq!(f.bd.show(id).await.unwrap().parent.as_ref(), Some(&f.root));
        }
        assert!(
            !f.bd
                .show(&f.source.id)
                .await
                .unwrap()
                .metadata
                .contains_key(HOLD_KEY)
        );
        assert_eq!(f.bd.show(&f.source.id).await.unwrap().status, Status::Open);
    }
}

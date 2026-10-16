#[cfg(test)]
mod cases {
    use super::super::*;

    #[tokio::test]
    async fn native_wait_removed_unresolved_prerequisite_cannot_authorize_release() {
        let f = Fixture::new().await;
        f.admit().await;
        let before = f.bd.show(&f.source.id).await.unwrap();
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
        assert!(
            output.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        f.bd.close(
            &f.blockers[1],
            Some("remaining current prerequisite resolved"),
        )
        .await
        .unwrap();
        let changed = f.bd.show(&f.source.id).await.unwrap();
        assert_eq!(before.status, changed.status);
        assert!(
            f.journal()
                .refresh_native_waits(&f.bd, Some(&f.root), None)
                .await
                .is_err()
        );
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), changed);
        assert_eq!(
            f.journal()
                .native()
                .read_native(&f.source.id)
                .unwrap()
                .unwrap()
                .phase,
            Phase::Active
        );
    }

    #[tokio::test]
    async fn native_wait_restart_retains_late_unresolved_prerequisites() {
        let f = Fixture::new().await;
        f.admit().await;
        let late =
            f.bd.create(CreateOpts {
                title: "late prerequisite".into(),
                parent: Some(f.root.clone()),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        f.bd.dep_add(&f.source.id, &late).await.unwrap();
        f.refresh().await;
        let parked = f.bd.show(&f.source.id).await.unwrap();
        let output = IsolatedBeads::attach(f.directory.path())
            .run(
                ["dep", "remove", f.source.id.as_str(), late.as_str()]
                    .into_iter()
                    .map(OsString::from)
                    .collect(),
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        assert!(
            output.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        for blocker in &f.blockers {
            f.close(blocker).await;
        }
        let restarted = DecisionWaitJournal::new(f.directory.path());
        assert!(
            restarted
                .refresh_native_waits(&f.bd, Some(&f.root), None)
                .await
                .is_err()
        );
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), parked);
        assert!(restarted.blocks_scope(&f.bd, &f.root).await.unwrap());
        f.close(&late).await;
        f.refresh().await;
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), f.source);
    }

    #[tokio::test]
    async fn native_wait_preserves_human_closed_source_after_prerequisites_resolve() {
        let f = Fixture::new().await;
        f.admit().await;
        for blocker in &f.blockers {
            f.bd.close(blocker, Some("resolved")).await.unwrap();
        }
        f.bd.close(&f.source.id, Some("human completed work"))
            .await
            .unwrap();
        let closed = f.bd.show(&f.source.id).await.unwrap();
        f.refresh().await;
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), closed);
        assert_eq!(
            f.journal()
                .native()
                .read_native(&f.source.id)
                .unwrap()
                .unwrap()
                .phase,
            Phase::Cancelled
        );
    }
}

#[cfg(test)]
mod cases {
    use super::super::*;

    #[tokio::test]
    async fn moved_native_wait_cannot_evade_publication_or_release() {
        let f = Fixture::new().await;
        f.admit().await;
        let root =
            f.bd.create(CreateOpts {
                title: "new work root".into(),
                issue_type: Some(IssueType::Epic),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        let moved = IsolatedBeads::attach(f.directory.path())
            .run(
                ["update", f.source.id.as_str(), "--parent", root.as_str()]
                    .into_iter()
                    .map(OsString::from)
                    .collect(),
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        assert!(
            moved.success(),
            "{}",
            String::from_utf8_lossy(&moved.stderr)
        );
        let moved = f.bd.show(&f.source.id).await.unwrap();
        assert!(f.journal().blocks_scope(&f.bd, &root).await.is_err());
        assert!(f.journal().blocks_scope(&f.bd, &f.root).await.is_err());
        assert!(
            f.journal()
                .refresh_native_waits(&f.bd, None, None)
                .await
                .is_err()
        );
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), moved);
        assert_eq!(
            f.journal()
                .native()
                .read_native(&f.source.id)
                .unwrap()
                .unwrap()
                .record
                .root,
            f.root
        );
    }

    #[tokio::test]
    async fn released_native_wait_can_admit_a_new_current_prerequisite() {
        let f = Fixture::new().await;
        f.admit().await;
        for blocker in &f.blockers {
            f.close(blocker).await;
        }
        f.refresh().await;
        let next =
            f.bd.create(CreateOpts {
                title: "new prerequisite".into(),
                parent: Some(f.root.clone()),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        f.bd.dep_add(&f.source.id, &next).await.unwrap();
        let source = f.bd.show(&f.source.id).await.unwrap();
        let blockers = f.journal().admit_native_wait(&f.bd, &source).await.unwrap();
        assert_eq!(blockers.count(), 1);
        assert!(blockers.ids().any(|id| *id == next));
        assert_eq!(
            f.bd.show(&f.source.id).await.unwrap().status,
            Status::Blocked
        );
        f.close(&next).await;
        f.refresh().await;
        assert_eq!(f.bd.show(&f.source.id).await.unwrap(), source);
    }
}

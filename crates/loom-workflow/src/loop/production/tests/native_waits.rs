#[cfg(test)]
mod cases {
    use super::super::*;
    use crate::test_support::{IsolatedBeads, client};
    use loom_driver::bd::{CreateOpts, IssueType, Status};
    use loom_protocol::output::{Phase, decode};

    struct Fixture {
        directory: tempfile::TempDir,
        bd: Arc<BdClient<IsolatedBeads>>,
        git: loom_driver::git::GitClient,
        root: BeadId,
        source: Bead,
        sibling: Bead,
    }

    impl Fixture {
        async fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let bd = Arc::new(client(directory.path()).await);
            let git = git_workspace(directory.path());
            seed_spec(directory.path(), "harness");
            let root = bd
                .create(CreateOpts {
                    title: "native wait root".into(),
                    issue_type: Some(IssueType::Epic),
                    ..CreateOpts::default()
                })
                .await
                .unwrap();
            let mut work = Vec::new();
            for title in ["waiting implementation", "independent implementation"] {
                let id = bd.create(CreateOpts {
                title: title.into(), parent: Some(root.clone()),
                metadata: Some(serde_json::json!({loom_protocol::acceptance::METADATA_KEY: loom_test_support::acceptance::metadata("harness").unwrap()}).to_string()),
                ..CreateOpts::default()
            }).await.unwrap();
                work.push(id);
            }
            let blocker = bd
                .create(CreateOpts {
                    title: "real prerequisite".into(),
                    parent: Some(root.clone()),
                    ..CreateOpts::default()
                })
                .await
                .unwrap();
            bd.dep_add(&work[0], &blocker).await.unwrap();
            let source = bd.show(&work[0]).await.unwrap();
            let sibling = bd.show(&work[1]).await.unwrap();
            Self {
                directory,
                bd,
                git,
                root,
                source,
                sibling,
            }
        }
    }

    fn decoded(marker: &str) -> SessionResult {
        SessionResult::Decoded {
            outcome: SessionOutcome {
                exit_code: 0,
                cost_usd: None,
            },
            output: Arc::new(decode(marker, Phase::Loop)),
        }
    }

    #[tokio::test]
    async fn attributed_dependency_waits_park_without_integration_or_retry() {
        let f = Fixture::new().await;
        let source_id = f.source.id.clone();
        let source_path = Arc::new(std::sync::Mutex::new(None));
        let path = Arc::clone(&source_path);
        let agent = Arc::clone(&f.bd);
        let before = loom_driver::git::sync_head_commit_sha(&f.git.loom_workspace()).unwrap();
        let mut controller = ProductionAgentLoopController::new(
            BdClient::with_runner(IsolatedBeads::attach(f.directory.path())),
            SpecLabel::new("harness").unwrap(),
            PathBuf::from("must-not-run-gate"),
            f.directory.path().to_path_buf(),
            f.git,
            write_manifest(f.directory.path()),
            None,
            ProfileName::new("base").unwrap(),
            move |cfg: SpawnConfig, id| {
                let agent = Arc::clone(&agent);
                let path = Arc::clone(&path);
                let waiting = id == source_id;
                async move {
                    if waiting {
                        std::fs::write(cfg.workspace.join("retained-draft.txt"), "unmerged draft")
                            .unwrap();
                        *path.lock().unwrap() = Some(cfg.workspace);
                        (decoded("LOOM_WAITING"), None)
                    } else {
                        agent
                            .close(&id, Some("independent no-work acceptance"))
                            .await
                            .unwrap();
                        (decoded("LOOM_NOOP"), Some(ExitSignal::Noop))
                    }
                }
            },
        )
        .with_fixed_queue(VecDeque::from([f.source.clone(), f.sibling.clone()]))
        .with_ready_parent(f.root.clone());
        let summary = crate::r#loop::run_loop(
            &mut controller,
            crate::r#loop::RetryPolicy { max_retries: 0 },
            2,
        )
        .await
        .unwrap();
        assert_eq!(summary.beads_waiting, 1);
        assert_eq!(summary.beads_processed, 2);
        assert_eq!(
            f.bd.show(&f.source.id).await.unwrap().status,
            Status::Blocked
        );
        assert_eq!(
            f.bd.show(&f.sibling.id).await.unwrap().status,
            Status::Closed
        );
        assert_eq!(
            loom_driver::git::sync_head_commit_sha(&controller.git.loom_workspace()).unwrap(),
            before
        );
        let path = source_path.lock().unwrap().clone().unwrap();
        assert_eq!(
            std::fs::read_to_string(path.join("retained-draft.txt")).unwrap(),
            "unmerged draft"
        );
        assert!(!controller.workspace.join("retained-draft.txt").exists());
        assert!(
            DecisionWaitJournal::new(f.directory.path())
                .blocks_scope(&f.bd, &f.root)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn parallel_attributed_wait_preserves_workspace_and_ready_siblings() {
        let f = Fixture::new().await;
        let agent = Arc::clone(&f.bd);
        let waiting = f.source.id.clone();
        let workspace = f.directory.path().to_path_buf();
        let source_path = Arc::new(std::sync::Mutex::new(None));
        let path = Arc::clone(&source_path);
        let outcome = crate::r#loop::run_parallel_batch(
            &f.git,
            &SpecLabel::new("harness").unwrap(),
            vec![f.source.clone(), f.sibling.clone()],
            move |slot| {
                let agent = Arc::clone(&agent);
                let workspace = workspace.clone();
                let path = Arc::clone(&path);
                let waiting = slot.bead.id == waiting;
                async move {
                    if waiting {
                        std::fs::write(
                            slot.worktree.path.join("retained-draft.txt"),
                            "parallel draft",
                        )
                        .unwrap();
                        *path.lock().unwrap() = Some(slot.worktree.path.clone());
                        crate::r#loop::reconcile_session(
                            &*agent,
                            &DecisionWaitJournal::new(&workspace),
                            &slot.bead,
                            decoded("LOOM_WAITING"),
                            None,
                        )
                        .await
                        .unwrap()
                    } else {
                        std::fs::write(
                            slot.worktree.path.join("sibling.txt"),
                            "independent progress",
                        )
                        .unwrap();
                        loom_driver::git::commit_all_in(
                            &slot.worktree.path,
                            "independent fixture work",
                        )
                        .unwrap();
                        agent
                            .close(&slot.bead.id, Some("independent fixture work complete"))
                            .await
                            .unwrap();
                        AgentOutcome::Success
                    }
                }
            },
        )
        .await
        .unwrap();
        assert!(outcome.results.iter().any(|result| matches!(result, crate::r#loop::BatchResult::Waiting { bead, .. } if *bead == f.source.id)));
        assert!(outcome.results.iter().any(|result| matches!(result, crate::r#loop::BatchResult::Merged { bead } if *bead == f.sibling.id)));
        assert_eq!(
            f.bd.show(&f.source.id).await.unwrap().status,
            Status::Blocked
        );
        let path = source_path.lock().unwrap().clone().unwrap();
        assert_eq!(
            std::fs::read_to_string(path.join("retained-draft.txt")).unwrap(),
            "parallel draft"
        );
        assert!(!f.git.loom_workspace().join("retained-draft.txt").exists());
        assert!(f.git.loom_workspace().join("sibling.txt").is_file());
    }
}

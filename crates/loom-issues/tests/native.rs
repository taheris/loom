#[cfg(test)]
mod common;

#[cfg(test)]
mod native {
    use super::common;
    use loom_issues::{
        beads::{BdClient, BdError, CommandRunner, RunOutput},
        memory::{Fault, Operation, Timing},
        task::{Create, Guard, Query, Update},
        tracking::{Error, Store, Synchronization},
    };
    use std::{
        ffi::OsString,
        path::PathBuf,
        sync::{Arc, Mutex},
        time::Duration,
    };
    use tempfile::TempDir;
    use tokio::process::Command;

    /// Isolated embedded Dolt fixtures never use the operator's server or credentials.
    #[derive(Clone)]
    struct Native {
        root: Arc<TempDir>,
        binary: PathBuf,
        fault: Arc<Mutex<Option<Fault>>>,
    }

    impl Native {
        async fn new() -> Self {
            let binary = PathBuf::from(
                std::env::var_os("LOOM_ISSUES_BD_BIN")
                    .or_else(|| option_env!("LOOM_ISSUES_BD_BIN").map(OsString::from))
                    .expect("set LOOM_ISSUES_BD_BIN to the pinned Beads executable"),
            );
            assert!(
                binary.is_absolute(),
                "native conformance requires an explicit executable"
            );
            let root = Arc::new(tempfile::tempdir().unwrap());
            std::fs::create_dir(root.path().join("home")).unwrap();
            let fixture = Self {
                root,
                binary,
                fault: Arc::default(),
            };
            let output = fixture
                .command("git")
                .args(["init", "--quiet"])
                .output()
                .await
                .unwrap();
            assert!(output.status.success());
            let output = fixture
                .raw(&[
                    "init",
                    "--prefix=ct",
                    "--skip-hooks",
                    "--skip-agents",
                    "--non-interactive",
                ])
                .await;
            assert!(
                output.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            fixture
        }

        fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
            let mut command = Command::new(program);
            command.current_dir(self.root.path()).kill_on_drop(true);
            for (key, _) in std::env::vars_os() {
                let name = key.to_string_lossy();
                if name.starts_with("BEADS_")
                    || name.starts_with("BD_")
                    || name.starts_with("DOLT_")
                    || name.starts_with("GIT_")
                {
                    command.env_remove(key);
                }
            }
            command
                .env("HOME", self.root.path().join("home"))
                .env("XDG_CONFIG_HOME", self.root.path().join("home/.config"))
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("GIT_AUTHOR_NAME", "Conformance")
                .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
                .env("GIT_COMMITTER_NAME", "Conformance")
                .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
                .env("BD_JSON_ENVELOPE", "1")
                .env("BD_DISABLE_METRICS", "1")
                .env("BD_DISABLE_EVENT_FLUSH", "1");
            command
        }

        async fn raw(&self, args: &[&str]) -> RunOutput {
            self.run(
                args.iter().map(OsString::from).collect(),
                Duration::from_secs(60),
            )
            .await
            .unwrap()
        }

        fn inject(&self, fault: Fault) {
            *self.fault.lock().unwrap() = Some(fault);
        }

        fn take_fault(&self, args: &[OsString]) -> Option<Fault> {
            let operation = match args.first()?.to_str()? {
                "create" => Operation::Create,
                "update" => Operation::Update,
                "close" => Operation::Close,
                "dep" => Operation::Relate,
                "dolt" => Operation::Synchronize,
                _ => return None,
            };
            let mut queued = self.fault.lock().unwrap();
            match queued.as_mut() {
                Some(fault) if fault.operation == operation && fault.skip == 0 => queued.take(),
                Some(fault) if fault.operation == operation => {
                    fault.skip -= 1;
                    None
                }
                _ => None,
            }
        }
    }

    impl CommandRunner for Native {
        async fn run(&self, args: Vec<OsString>, timeout: Duration) -> Result<RunOutput, BdError> {
            let fault = self.take_fault(&args);
            if fault.is_some_and(|fault| fault.timing == Timing::BeforeWrite) {
                return Err(BdError::Timeout {
                    args: "before native invocation".into(),
                });
            }
            let output =
                tokio::time::timeout(timeout, self.command(&self.binary).args(&args).output())
                    .await
                    .map_err(|_| BdError::Timeout {
                        args: format!("{args:?}"),
                    })?
                    .map_err(BdError::Spawn)?;
            if fault.is_some_and(|fault| fault.timing == Timing::AfterWrite) {
                assert!(
                    output.status.success(),
                    "fault must follow a real write: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                return Err(BdError::Timeout {
                    args: "receipt lost after native write".into(),
                });
            }
            Ok(RunOutput {
                status: output.status.code().unwrap_or(-1),
                stdout: output.stdout,
                stderr: output.stderr,
            })
        }

        async fn publish(&self, timeout: Duration) -> Result<(), BdError> {
            let output = self
                .run(vec!["dolt".into(), "push".into()], timeout)
                .await?;
            if output.success() {
                Ok(())
            } else {
                Err(BdError::Cli {
                    args: "dolt push".into(),
                    status: output.status,
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                })
            }
        }
    }

    #[tokio::test]
    async fn native_fixture_keeps_telemetry_disabled() {
        let native = Native::new().await;
        let output = native.raw(&["metrics"]).await;
        assert!(output.success());
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("Anonymous usage metrics: OFF"),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    #[tokio::test]
    async fn tracking_conformance_covers_lightweight_and_pinned_beads() {
        let native = Native::new().await;
        let store = BdClient::with_runner(native.clone());
        common::round_trip(&store).await;
        common::stale_writers(&store, &BdClient::with_runner(native.clone())).await;
        common::graph_guards(&store).await;
        common::coherent_observation(&store).await;
        common::inherited_readiness(&store).await;
        common::interruptions(&store, |fault| native.inject(fault)).await;
    }

    struct Interleaving {
        native: Native,
        source: loom_issues::task::Id,
        blocker: loom_issues::task::Id,
        reads: std::sync::atomic::AtomicUsize,
    }

    impl CommandRunner for Interleaving {
        async fn run(&self, args: Vec<OsString>, timeout: Duration) -> Result<RunOutput, BdError> {
            assert_eq!(
                args,
                ["show", self.source.as_str(), "--json"].map(OsString::from)
            );
            let output = self.native.run(args, timeout).await?;
            if self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                let peer = BdClient::with_runner(self.native.clone());
                Store::update(
                    &peer,
                    &self.source,
                    Update {
                        status: Some(loom_issues::task::Status::Blocked),
                        set_metadata: std::collections::BTreeMap::from([(
                            "epoch".into(),
                            serde_json::json!("after"),
                        )]),
                        ..Update::default()
                    },
                    Guard::None,
                )
                .await
                .unwrap();
                Store::close(&peer, &self.blocker, None).await.unwrap();
            }
            Ok(output)
        }
    }

    #[tokio::test]
    async fn observation_keeps_one_native_response_despite_interleaved_writes() {
        use loom_issues::task::{Relation, Status};
        let native = Native::new().await;
        let peer = BdClient::with_runner(native.clone());
        let source = common::create(&peer, "source", None).await;
        let blocker = common::create(&peer, "blocker", None).await;
        peer.relate(&source, &blocker, Relation::Blocks)
            .await
            .unwrap();
        let store = BdClient::with_runner(Interleaving {
            native,
            source: source.clone(),
            blocker: blocker.clone(),
            reads: std::sync::atomic::AtomicUsize::new(0),
        });
        let before = store.observe(&source).await.unwrap();
        assert_eq!(before.record.id, source);
        assert_eq!(before.record.status, Status::Open);
        assert_eq!(before.graph.status, Status::Open);
        assert!(!before.record.metadata.contains_key("epoch"));
        assert_eq!(before.graph.dependencies[0].id, blocker);
        assert_eq!(before.graph.dependencies[0].status, Status::Open);
        let after = store.observe(&source).await.unwrap();
        assert_eq!(after.record.status, Status::Blocked);
        assert_eq!(after.graph.status, Status::Blocked);
        assert_eq!(after.record.metadata["epoch"], serde_json::json!("after"));
        assert_eq!(after.graph.dependencies[0].status, Status::Closed);
    }

    #[tokio::test]
    async fn native_multi_id_status_predicates_do_not_make_updates_atomic() {
        let native = Native::new().await;
        let store = BdClient::with_runner(native.clone());
        let first = common::create(&store, "first", None).await;
        let second = Store::create(
            &store,
            Create {
                title: "second".into(),
                status: Some(loom_issues::task::Status::Blocked),
                ..Create::default()
            },
        )
        .await
        .unwrap();
        let output = native
            .raw(&[
                "update",
                first.as_str(),
                second.as_str(),
                "--if-status",
                "open",
                "--title",
                "written",
            ])
            .await;
        assert_eq!(output.status, 13);
        assert_eq!(store.get(&first).await.unwrap().title, "written");
        assert_eq!(store.get(&second).await.unwrap().title, "second");
    }

    #[tokio::test]
    async fn beads_tracking_adapter_retains_native_interoperability_coverage() {
        let native = Native::new().await;
        let store = BdClient::with_runner(native.clone());
        let id = Store::create(
            &store,
            Create {
                title: "shared authoritative state".into(),
                ..Create::default()
            },
        )
        .await
        .unwrap();
        let peer = BdClient::with_runner(native.clone());
        Store::update(
            &peer,
            &id,
            Update {
                title: Some("visible from second client".into()),
                ..Update::default()
            },
            Guard::None,
        )
        .await
        .unwrap();
        assert_eq!(
            store.get(&id).await.unwrap().title,
            "visible from second client"
        );
        native.inject(Fault {
            operation: Operation::Create,
            timing: Timing::AfterWrite,
            skip: 0,
        });
        assert!(matches!(
            Store::create(
                &store,
                Create {
                    title: "uncertain receipt".into(),
                    ..Create::default()
                }
            )
            .await,
            Err(Error::Uncertain { .. })
        ));
        assert_eq!(peer.query(Query::default()).await.unwrap().len(), 2);
        store
            .synchronize(Synchronization::SharedState)
            .await
            .unwrap();
        let invalid_remote = native.root.path().join("not-a-directory");
        std::fs::create_dir(&invalid_remote).unwrap();
        let invalid_url = format!("file://{}", invalid_remote.display());
        assert!(
            native
                .raw(&["dolt", "remote", "add", "origin", &invalid_url])
                .await
                .success()
        );
        std::fs::remove_dir(&invalid_remote).unwrap();
        std::fs::write(&invalid_remote, b"not a Dolt remote").unwrap();
        let unavailable_remote = store.synchronize(Synchronization::Publish).await;
        assert!(
            matches!(unavailable_remote, Err(Error::Uncertain { .. })),
            "{unavailable_remote:?}"
        );
        assert!(
            native
                .raw(&["dolt", "remote", "remove", "origin"])
                .await
                .success()
        );
        let remote = native.root.path().join("remote");
        std::fs::create_dir(&remote).unwrap();
        let url = format!("file://{}", remote.display());
        let output = native.raw(&["dolt", "remote", "add", "origin", &url]).await;
        assert!(
            output.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        store.synchronize(Synchronization::Publish).await.unwrap();
        native.inject(Fault {
            operation: Operation::Synchronize,
            timing: Timing::AfterWrite,
            skip: 0,
        });
        assert!(matches!(
            store.synchronize(Synchronization::Publish).await,
            Err(Error::Uncertain { .. })
        ));
        assert_eq!(
            peer.get(&id).await.unwrap().title,
            "visible from second client"
        );
        store.synchronize(Synchronization::Publish).await.unwrap();
    }
}

#[cfg(test)]
mod native {
    use loom_driver::{
        bd::{BdClient, BdError, CommandRunner, RunOutput},
        clock::SystemClock,
    };
    use loom_issues::{
        task::{self, Guard, Status},
        tracking::{Error, Store},
    };
    use std::{collections::BTreeMap, ffi::OsString, path::PathBuf, time::Duration};

    struct Native(PathBuf);
    impl CommandRunner for Native {
        async fn run(&self, args: Vec<OsString>, timeout: Duration) -> Result<RunOutput, BdError> {
            let mut command = tokio::process::Command::new("bd");
            command
                .args(["--sandbox", "--dolt-auto-commit", "off"])
                .args(args)
                .current_dir(&self.0)
                .env("HOME", self.0.join("home"))
                .env("GOMAXPROCS", "2")
                .env("BD_JSON_ENVELOPE", "1");
            for (name, _) in std::env::vars_os() {
                if name.to_string_lossy().starts_with("BEADS_") {
                    command.env_remove(name);
                }
            }
            let output = loom_driver::process::output(&mut command, &SystemClock::new(), timeout)
                .await
                .unwrap();
            Ok(RunOutput {
                status: output.status.code().unwrap(),
                stdout: output.stdout,
                stderr: output.stderr,
            })
        }
    }

    #[tokio::test]
    async fn native_status_predicate_is_not_revision_cas_and_metadata_is_targeted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("home")).unwrap();
        loom_driver::git::initialize_snapshot(dir.path(), &SystemClock::new())
            .await
            .unwrap();
        let runner = Native(dir.path().into());
        let initialized = runner
            .run(
                [
                    "init",
                    "--prefix",
                    "fixture",
                    "--skip-agents",
                    "--skip-hooks",
                    "--non-interactive",
                ]
                .map(OsString::from)
                .into(),
                Duration::from_secs(60),
            )
            .await
            .unwrap();
        assert!(
            initialized.success(),
            "{}",
            String::from_utf8_lossy(&initialized.stderr)
        );
        let adapter = BdClient::with_runner(runner);
        let id = Store::create(
            &adapter,
            task::Create {
                title: "typed boundary".into(),
                metadata: BTreeMap::from([("preserve".into(), serde_json::json!("original"))]),
                ..task::Create::default()
            },
        )
        .await
        .unwrap();
        for value in ["first", "second"] {
            Store::update(
                &adapter,
                &id,
                task::Update {
                    set_metadata: BTreeMap::from([("field".into(), serde_json::json!(value))]),
                    ..task::Update::default()
                },
                Guard::Status(Status::Open),
            )
            .await
            .unwrap();
        }
        let record = adapter.get(&id).await.unwrap();
        assert_eq!(record.metadata["preserve"], "original");
        assert_eq!(record.metadata["field"], "second");
        let refused = Store::update(
            &adapter,
            &id,
            task::Update {
                title: Some("must not apply".into()),
                ..task::Update::default()
            },
            Guard::Status(Status::Closed),
        )
        .await;
        assert!(
            matches!(refused, Err(Error::Conflict { .. })),
            "{refused:?}"
        );
        assert_eq!(adapter.get(&id).await.unwrap().title, "typed boundary");
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use loom_driver::bd::{
        BdClient, BdError, CommandRunner, CreateOpts, IssueType, RunOutput, UpdateOpts,
    };
    use loom_driver::identifier::BeadId;
    use loom_gate::{IntegrityFinding, Tier};

    use super::super::tests::{IsolatedBeads, children, isolated_client};
    use crate::r#loop::DecisionWaitJournal;
    use crate::mint::integrity::escalate;
    use crate::review::WorkspaceFindingValidator;

    struct Fixture {
        directory: tempfile::TempDir,
        bd: BdClient<IsolatedBeads>,
        root: BeadId,
        findings: [IntegrityFinding; 2],
    }

    impl Fixture {
        async fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let bd = isolated_client(directory.path()).await;
            let root = bd
                .create(CreateOpts {
                    title: "driver integrity work root".into(),
                    issue_type: Some(IssueType::Epic),
                    labels: vec!["spec:gate".into()],
                    ..CreateOpts::default()
                })
                .await
                .unwrap();
            std::fs::create_dir(directory.path().join("specs")).unwrap();
            std::fs::write(directory.path().join("specs/gate.md"), "# Gate\n## Success Criteria\n- first [check](missing-one)\n- second [check](missing-two)\n").unwrap();
            let findings = ["missing-one", "missing-two"].map(|target| {
                IntegrityFinding::UnresolvedAnnotation {
                    spec: "specs/gate.md".into(),
                    line: 3,
                    tier: Tier::Check,
                    target: target.into(),
                }
            });
            Self {
                directory,
                bd,
                root,
                findings,
            }
        }

        fn journal(&self) -> DecisionWaitJournal {
            DecisionWaitJournal::new(self.directory.path())
        }

        async fn escalate(&self, count: usize) -> Result<(), crate::mint::integrity::Error> {
            escalate(
                &self.bd,
                &self.journal(),
                &self.root,
                &self.findings[..count],
                &WorkspaceFindingValidator::new(self.directory.path()),
                self.directory.path(),
            )
            .await
        }
    }

    fn canonical_files(directory: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.extend(canonical_files(&path));
            } else if path
                .extension()
                .is_some_and(|extension| extension == "json" || extension == "jsonl")
            {
                files.push((path.clone(), std::fs::read(path).unwrap()));
            }
        }
        files.sort_by(|left, right| left.0.cmp(&right.0));
        files
    }

    #[tokio::test]
    async fn integrity_driver_replay_preserves_canonical_dispositions_and_events() {
        let f = Fixture::new().await;
        f.escalate(2).await.unwrap();
        let before = canonical_files(&f.directory.path().join(".loom"));
        let snapshots = children(&f.bd, &f.root).await;
        for decision in &snapshots {
            assert!(f.journal().has_disposition(&f.root, &decision.id).unwrap());
        }
        let root_before = f.bd.show(&f.root).await.unwrap();
        f.escalate(2).await.unwrap();
        assert_eq!(children(&f.bd, &f.root).await, snapshots);
        assert_eq!(f.bd.show(&f.root).await.unwrap(), root_before);
        assert_eq!(canonical_files(&f.directory.path().join(".loom")), before);
    }

    #[tokio::test]
    async fn integrity_driver_rejects_unresolved_extension_before_new_queue_effects() {
        for acceptance in [None, Some(r#"{"purpose":"unknown"}"#)] {
            let f = Fixture::new().await;
            f.escalate(1).await.unwrap();
            let decision = children(&f.bd, &f.root).await.remove(0);
            let task =
                f.bd.create(CreateOpts {
                    title: "unattributed affected task".into(),
                    parent: Some(f.root.clone()),
                    metadata: acceptance
                        .map(|value| serde_json::json!({"loom.acceptance":value}).to_string()),
                    ..CreateOpts::default()
                })
                .await
                .unwrap();
            f.bd.dep_add(&task, &decision.id).await.unwrap();
            f.bd.update(
                &decision.id,
                UpdateOpts {
                    set_metadata: vec![(
                        "loom.decision_candidate".into(),
                        serde_json::json!({
                            "version":1,"producer":f.root,"root":f.root,"affected_work":[task]
                        })
                        .to_string(),
                    )],
                    ..UpdateOpts::default()
                },
            )
            .await
            .unwrap();
            let before = canonical_files(&f.directory.path().join(".loom"));
            let decision_before = f.bd.show(&decision.id).await.unwrap();
            let task_before = f.bd.show(&task).await.unwrap();
            let error = f.escalate(2).await.unwrap_err();
            assert!(
                matches!(error, crate::mint::integrity::Error::Admission(_)),
                "{error:?}"
            );
            assert_eq!(canonical_files(&f.directory.path().join(".loom")), before);
            assert_eq!(f.bd.show(&decision.id).await.unwrap(), decision_before);
            assert_eq!(f.bd.show(&task).await.unwrap(), task_before);
            let staged = children(&f.bd, &f.root)
                .await
                .into_iter()
                .find(|bead| bead.issue_type == IssueType::Decision && bead.id != decision.id)
                .unwrap();
            assert_eq!(staged.status, loom_driver::bd::Status::Blocked);
            assert!(
                !staged
                    .labels
                    .iter()
                    .any(|label| label.as_str() == "loom:clarify")
            );
            assert!(f.journal().has_disposition(&f.root, &staged.id).is_err());
        }
    }

    #[tokio::test]
    async fn integrity_driver_rechecks_persisted_goal_identity_before_publication() {
        let f = Fixture::new().await;
        let validator = WorkspaceFindingValidator::new(f.directory.path());
        let summary =
            crate::mint::mint_integrity_recovery(&f.bd, &f.findings, "fixture-head", &validator)
                .await
                .unwrap();
        assert_eq!(summary.errors, 0, "{}", summary.render());
        assert_eq!(summary.refused, 0, "{}", summary.render());
        let task = children(&f.bd, &f.root)
            .await
            .into_iter()
            .find(|bead| bead.issue_type == IssueType::Task)
            .unwrap();
        f.bd.update(
            &task.id,
            UpdateOpts {
                status: Some(loom_driver::bd::Status::InProgress),
                ..UpdateOpts::default()
            },
        )
        .await
        .unwrap();
        let task = f.bd.show(&task.id).await.unwrap();
        f.escalate(1).await.unwrap();
        let decision = children(&f.bd, &f.root)
            .await
            .into_iter()
            .find(|bead| bead.issue_type == IssueType::Decision)
            .unwrap();
        f.bd.dep_add(&task.id, &decision.id).await.unwrap();
        f.bd.update(
            &decision.id,
            UpdateOpts {
                set_metadata: vec![(
                    "loom.decision_candidate".into(),
                    serde_json::json!({
                        "version":1,"producer":f.root,"root":f.root,"affected_work":[task.id]
                    })
                    .to_string(),
                )],
                ..UpdateOpts::default()
            },
        )
        .await
        .unwrap();
        f.escalate(2).await.unwrap();
        assert!(f.journal().blocks_scope(&f.bd, &f.root).await.unwrap());
        let before = canonical_files(&f.directory.path().join(".loom"));
        for key in ["loom.acceptance", "loom.finding_batch"] {
            let original = task.metadata[key].as_str().unwrap();
            let mut candidate: serde_json::Value = serde_json::from_str(original).unwrap();
            if key == "loom.acceptance" {
                candidate["purpose"] = "unknown".into();
            } else {
                candidate["findings"][0]["finding"]["evidence"] =
                    "different source evidence".into();
            }
            f.bd.update(
                &task.id,
                UpdateOpts {
                    set_metadata: vec![(key.into(), candidate.to_string())],
                    ..UpdateOpts::default()
                },
            )
            .await
            .unwrap();
            let changed = f.bd.show(&task.id).await.unwrap();
            assert!(f.journal().blocks_scope(&f.bd, &f.root).await.is_err());
            assert!(f.escalate(2).await.is_err());
            assert_eq!(f.bd.show(&task.id).await.unwrap(), changed);
            assert_eq!(canonical_files(&f.directory.path().join(".loom")), before);
            f.bd.update(
                &task.id,
                UpdateOpts {
                    set_metadata: vec![(key.into(), original.into())],
                    ..UpdateOpts::default()
                },
            )
            .await
            .unwrap();
        }
        let replacement =
            f.bd.create(CreateOpts {
                title: "different valid finding batch".into(),
                parent: Some(f.root.clone()),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        let mut purpose: serde_json::Value =
            serde_json::from_str(task.metadata["loom.acceptance"].as_str().unwrap()).unwrap();
        for goal in purpose["goals"].as_array_mut().unwrap() {
            goal["batch"] = replacement.as_str().into();
        }
        let mut batch: serde_json::Value =
            serde_json::from_str(task.metadata["loom.finding_batch"].as_str().unwrap()).unwrap();
        batch["attribution"] = serde_json::json!({(task.id.as_str()): purpose["goals"]});
        f.bd.update(
            &replacement,
            UpdateOpts {
                set_metadata: vec![("loom.finding_batch".into(), batch.to_string())],
                ..UpdateOpts::default()
            },
        )
        .await
        .unwrap();
        f.bd.update(
            &task.id,
            UpdateOpts {
                set_metadata: vec![("loom.acceptance".into(), purpose.to_string())],
                ..UpdateOpts::default()
            },
        )
        .await
        .unwrap();
        crate::r#loop::acceptance::resolve(
            &f.bd,
            &f.bd.show(&task.id).await.unwrap(),
            f.directory.path(),
        )
        .await
        .unwrap();
        assert!(f.journal().blocks_scope(&f.bd, &f.root).await.is_err());
        assert!(f.escalate(2).await.is_err());
        assert_eq!(canonical_files(&f.directory.path().join(".loom")), before);
        f.bd.update(
            &task.id,
            UpdateOpts {
                set_metadata: vec![(
                    "loom.acceptance".into(),
                    task.metadata["loom.acceptance"].as_str().unwrap().into(),
                )],
                ..UpdateOpts::default()
            },
        )
        .await
        .unwrap();
        f.escalate(2).await.unwrap();
        assert_eq!(canonical_files(&f.directory.path().join(".loom")), before);
    }

    #[tokio::test]
    async fn integrity_extension_requires_explicit_reusable_source_readmission() {
        let f = Fixture::new().await;
        let validator = WorkspaceFindingValidator::new(f.directory.path());
        let summary =
            crate::mint::mint_integrity_recovery(&f.bd, &f.findings, "fixture-head", &validator)
                .await
                .unwrap();
        assert_eq!(summary.errors, 0, "{}", summary.render());
        let source = children(&f.bd, &f.root)
            .await
            .into_iter()
            .find(|bead| bead.issue_type == IssueType::Task)
            .unwrap();
        f.bd.update(
            &source.id,
            UpdateOpts {
                status: Some(loom_driver::bd::Status::InProgress),
                ..UpdateOpts::default()
            },
        )
        .await
        .unwrap();
        let source = f.bd.show(&source.id).await.unwrap();
        f.escalate(1).await.unwrap();
        let decision = children(&f.bd, &f.root)
            .await
            .into_iter()
            .find(|bead| bead.issue_type == IssueType::Decision)
            .unwrap();
        f.bd.dep_add(&source.id, &decision.id).await.unwrap();
        let proposal = |affected: &[BeadId]| UpdateOpts {
            set_metadata: vec![(
                "loom.decision_candidate".into(),
                serde_json::json!({
                    "version":1,"producer":f.root,"root":f.root,"affected_work":affected
                })
                .to_string(),
            )],
            ..UpdateOpts::default()
        };
        f.bd.update(&decision.id, proposal(std::slice::from_ref(&source.id)))
            .await
            .unwrap();
        f.escalate(1).await.unwrap();
        let session = |terminal: &str| crate::r#loop::SessionResult::Decoded {
            outcome: loom_driver::agent::SessionOutcome {
                exit_code: 0,
                cost_usd: None,
            },
            output: Arc::new(loom_protocol::output::decode(
                &format!(
                    "LOOM_CLARIFY: {{\"decisions\":[\"{}\"]}}\n{terminal}",
                    decision.id
                ),
                loom_protocol::output::Phase::Loop,
            )),
        };
        assert!(matches!(
            crate::r#loop::reconcile_session(
                &f.bd,
                &f.journal(),
                &source,
                session("LOOM_RETRY: {\"reason\":\"retain unresolved prerequisite\"}"),
                None,
            )
            .await
            .unwrap(),
            crate::r#loop::AgentOutcome::Retry { .. }
        ));
        let followup =
            f.bd.create(CreateOpts {
                title: "independently bound follow-up".into(),
                parent: Some(f.root.clone()),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        let loom_protocol::acceptance::Candidates::Remediation { goals } =
            loom_protocol::acceptance::Candidates::from_metadata(
                &source.metadata["loom.acceptance"],
            )
            .unwrap()
        else {
            panic!("minted remediation");
        };
        let finding = f.findings[0]
            .to_raw_finding()
            .unwrap()
            .resolve(loom_protocol::gate::DispatchScope::PushGate, &validator)
            .unwrap();
        let reference = goals
            .as_slice()
            .iter()
            .find(|reference| reference.finding_hash.as_str() == finding.hash())
            .unwrap()
            .clone();
        crate::mint::acceptance::admit_follow_up(
            &f.bd,
            &f.bd.show(&followup).await.unwrap(),
            &crate::mint::acceptance::FollowUp {
                source: source.id.clone(),
                goals: loom_protocol::acceptance::GoalReferences::new(vec![reference.clone()])
                    .unwrap(),
            },
            f.directory.path(),
        )
        .await
        .unwrap();
        f.bd.dep_add(&followup, &decision.id).await.unwrap();
        f.bd.update(
            &decision.id,
            proposal(&[source.id.clone(), followup.clone()]),
        )
        .await
        .unwrap();
        let before = canonical_files(&f.directory.path().join(".loom"));
        assert!(f.journal().blocks_scope(&f.bd, &f.root).await.is_err());
        assert!(matches!(
            crate::r#loop::reconcile_session(
                &f.bd,
                &f.journal(),
                &source,
                session("LOOM_WAITING"),
                None,
            )
            .await
            .unwrap(),
            crate::r#loop::AgentOutcome::Failure { .. }
        ));
        assert_eq!(canonical_files(&f.directory.path().join(".loom")), before);
        f.escalate(2).await.unwrap();
        let before = canonical_files(&f.directory.path().join(".loom"));
        assert!(f.journal().blocks_scope(&f.bd, &f.root).await.is_err());
        assert_eq!(canonical_files(&f.directory.path().join(".loom")), before);
        assert!(matches!(
            crate::r#loop::reconcile_session(
                &f.bd,
                &f.journal(),
                &source,
                session("LOOM_WAITING"),
                None,
            )
            .await
            .unwrap(),
            crate::r#loop::AgentOutcome::Waiting { .. }
        ));
        assert!(f.journal().blocks_scope(&f.bd, &f.root).await.unwrap());
        let waiting = f.bd.show(&source.id).await.unwrap();
        assert_eq!(waiting.status, loom_driver::bd::Status::Blocked);
        assert!(waiting.metadata.contains_key("loom.dependency_wait"));
        assert_eq!(
            f.bd.show(&decision.id).await.unwrap().status,
            decision.status
        );
        let continuation =
            f.bd.create(CreateOpts {
                title: "later bound follow-up".into(),
                parent: Some(f.root.clone()),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        crate::mint::acceptance::admit_follow_up(
            &f.bd,
            &f.bd.show(&continuation).await.unwrap(),
            &crate::mint::acceptance::FollowUp {
                source: source.id.clone(),
                goals: loom_protocol::acceptance::GoalReferences::new(vec![reference]).unwrap(),
            },
            f.directory.path(),
        )
        .await
        .unwrap();
        f.bd.dep_add(&continuation, &decision.id).await.unwrap();
        f.bd.update(
            &decision.id,
            proposal(&[source.id.clone(), followup, continuation]),
        )
        .await
        .unwrap();
        f.escalate(2).await.unwrap();
        let held_source = f.bd.show(&source.id).await.unwrap();
        let before = canonical_files(&f.directory.path().join(".loom"));
        assert!(matches!(
            crate::r#loop::reconcile_session(
                &f.bd,
                &f.journal(),
                &source,
                session("LOOM_WAITING"),
                None,
            )
            .await
            .unwrap(),
            crate::r#loop::AgentOutcome::Failure { .. }
        ));
        assert!(f.journal().blocks_scope(&f.bd, &f.root).await.unwrap());
        assert_eq!(f.bd.show(&source.id).await.unwrap(), held_source);
        assert_eq!(canonical_files(&f.directory.path().join(".loom")), before);
    }

    struct LostAcknowledgement {
        native: IsolatedBeads,
        interrupted: Arc<AtomicBool>,
    }

    impl CommandRunner for LostAcknowledgement {
        async fn run(&self, args: Vec<OsString>, timeout: Duration) -> Result<RunOutput, BdError> {
            let update = args.first().is_some_and(|arg| arg == "update")
                && args.iter().any(|arg| arg == "--add-label");
            let output = self.native.run(args, timeout).await?;
            if update && output.success() && !self.interrupted.swap(true, Ordering::SeqCst) {
                return Err(BdError::Cleanup(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "lost acknowledgement after actual native update",
                )));
            }
            Ok(output)
        }
    }

    #[tokio::test]
    async fn integrity_driver_recovers_lost_native_acknowledgement_without_source_rewrite() {
        let f = Fixture::new().await;
        let interrupted = Arc::new(AtomicBool::new(false));
        let bd = BdClient::with_runner(LostAcknowledgement {
            native: IsolatedBeads {
                root: f.directory.path().to_path_buf(),
            },
            interrupted: interrupted.clone(),
        });
        let root_before = f.bd.show(&f.root).await.unwrap();
        let error = escalate(
            &bd,
            &f.journal(),
            &f.root,
            &f.findings,
            &WorkspaceFindingValidator::new(f.directory.path()),
            f.directory.path(),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, crate::mint::integrity::Error::Admission(_)),
            "{error:?}"
        );
        assert!(interrupted.load(Ordering::SeqCst));
        let staged = children(&f.bd, &f.root).await;
        assert_eq!(staged.len(), 2);
        assert_eq!(
            staged
                .iter()
                .filter(|bead| bead
                    .labels
                    .iter()
                    .any(|label| label.as_str() == "loom:clarify"))
                .count(),
            1,
            "fault injection follows the real committed native effect"
        );
        f.escalate(2).await.unwrap();
        for decision in &staged {
            let current = f.bd.show(&decision.id).await.unwrap();
            assert!(
                current
                    .labels
                    .iter()
                    .any(|label| label.as_str() == "loom:clarify")
            );
            assert!(f.journal().has_disposition(&f.root, &decision.id).unwrap());
        }
        assert_eq!(f.bd.show(&f.root).await.unwrap(), root_before);
        let before = canonical_files(&f.directory.path().join(".loom"));
        f.escalate(2).await.unwrap();
        assert_eq!(canonical_files(&f.directory.path().join(".loom")), before);
    }
}

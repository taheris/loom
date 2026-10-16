#[cfg(test)]
mod admission;

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use loom_driver::bd::{CreateOpts, IssueType, ReadyOpts, Status};
    use loom_gate::Tier;

    use super::super::*;

    #[derive(Clone)]
    pub(super) struct IsolatedBeads {
        pub(super) root: PathBuf,
    }

    impl CommandRunner for IsolatedBeads {
        async fn run(&self, args: Vec<OsString>, timeout: Duration) -> Result<RunOutput, BdError> {
            let mut command = Command::new("bd");
            command
                .args(["--sandbox", "--dolt-auto-commit", "off"])
                .args(args)
                .current_dir(&self.root)
                .env("HOME", self.root.join("home"))
                .env("GOMAXPROCS", "2");
            for (name, _) in std::env::vars_os() {
                if name.to_string_lossy().starts_with("BEADS_") {
                    command.env_remove(name);
                }
            }
            let output = loom_driver::process::output(&mut command, &SystemClock::new(), timeout)
                .await
                .expect("isolated Beads process completed");
            Ok(RunOutput {
                status: output.status.code().expect("Beads exit code"),
                stdout: output.stdout,
                stderr: output.stderr,
            })
        }
    }

    pub(super) async fn children(bd: &BdClient<IsolatedBeads>, parent: &BeadId) -> Vec<Bead> {
        bd.list(ListOpts {
            all: true,
            limit: Some(0),
            parent: Some(parent.clone()),
            ..ListOpts::default()
        })
        .await
        .expect("children")
    }

    pub(super) async fn isolated_client(root: &Path) -> BdClient<IsolatedBeads> {
        std::fs::create_dir(root.join("home")).unwrap();
        loom_driver::git::initialize_snapshot(root, &SystemClock::new())
            .await
            .unwrap();
        let runner = IsolatedBeads {
            root: root.to_path_buf(),
        };
        let init = runner
            .run(
                [
                    "init",
                    "--prefix",
                    "cap",
                    "--skip-agents",
                    "--skip-hooks",
                    "--non-interactive",
                ]
                .into_iter()
                .map(OsString::from)
                .collect(),
                Duration::from_secs(60),
            )
            .await
            .unwrap();
        assert!(init.success(), "{}", String::from_utf8_lossy(&init.stderr));
        BdClient::with_runner(runner)
    }

    #[tokio::test]
    async fn integrity_cap_decisions_preserve_full_options_and_partial_resolution() {
        use crate::inbox::{build_queue, parse_options_in};
        use crate::r#loop::{AgentOutcome, validate_waiting_outcome};

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let bd = isolated_client(root).await;
        let parent = bd
            .create(CreateOpts {
                title: "cap root".into(),
                issue_type: Some(IssueType::Epic),
                labels: vec!["spec:gate".into()],
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        let state = seeded_state(root, "gate", parent.as_str());
        std::fs::write(root.join("specs/gate.md"), "# Gate\n\n## Success Criteria\n- missing [check](missing-one)\n- missing [check](missing-two)\n- stub [test](stub-test)\n- stale [test?](landed-test)\n- provider [check](provider-check)\n").unwrap();
        std::fs::write(root.join("loom.toml"), "[runner.check.provider]\nmatch = '^provider-check$'\ncommand = 'bash verifier.sh'\ninputs = 'bash query.sh {print_inputs}'\nparse = 'exit-code'\n").unwrap();
        std::fs::write(
            root.join("query.sh"),
            "set -euo pipefail\nprintf 'definition schema mismatch' >&2\nexit 17\n",
        )
        .unwrap();
        std::fs::write(
            root.join("verifier.sh"),
            "set -euo pipefail\nprintf 'verified provider\\n'\n",
        )
        .unwrap();
        let annotations = annotation::parse_content(
            Path::new("specs/gate.md"),
            &std::fs::read_to_string(root.join("specs/gate.md")).unwrap(),
        )
        .unwrap()
        .annotations;
        let config = LoomConfig::load(root.join("loom.toml")).unwrap();
        let runners = loom_gate::runner::integrity_runner_specs(&config).unwrap();
        let mut resolver = InputResolver::new(root.to_path_buf()).with_runners(runners.clone());
        let query_findings = loom_gate::check_inputs_protocol(&annotations, &mut resolver);
        assert_eq!(query_findings.len(), 1);
        let mut findings = vec![
            IntegrityFinding::UnresolvedAnnotation {
                spec: "specs/gate.md".into(),
                line: 4,
                tier: Tier::Check,
                target: "missing-one".into(),
            },
            IntegrityFinding::UnresolvedAnnotation {
                spec: "specs/gate.md".into(),
                line: 5,
                tier: Tier::Check,
                target: "missing-two".into(),
            },
            IntegrityFinding::StubTestFunction {
                spec: "specs/gate.md".into(),
                line: 6,
                tier: Tier::Test,
                target: "stub-test".into(),
                test_name: "stub-test".into(),
            },
            IntegrityFinding::UnneededPendingMarker {
                spec: "specs/gate.md".into(),
                line: 7,
                tier: Tier::Test,
                target: "landed-test".into(),
            },
        ];
        findings.extend(query_findings);
        let validator = WorkspaceFindingValidator::new(root);
        let summary =
            crate::mint::mint_integrity_recovery(&bd, &findings, "fixture-head", &validator)
                .await
                .unwrap();
        assert_eq!(summary.errors, 0, "{}", summary.render());
        assert_eq!(summary.refused, 0, "{}", summary.render());
        let remediation = children(&bd, &parent).await;
        assert_ne!(remediation, [] as [loom_driver::bd::Bead; 0]);
        let recovery_context = remediation
            .iter()
            .map(|bead| bead.description.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for finding in &findings {
            assert!(recovery_context.contains(&finding.clarify_options().unwrap()));
        }
        let epic_before = bd.show(&parent).await.unwrap();
        let mut controller = ProductionReviewController::new(
            bd,
            SpecLabel::new("gate").unwrap(),
            root.join("must-not-spawn-loom"),
            root.to_path_buf(),
            state,
            stub_manifest(root),
            ProfileName::new("base").unwrap(),
            noop_spawn,
        );
        controller.set_iteration_count(3).await.unwrap();
        controller.apply_integrity_clarify(&findings).await.unwrap();
        let all = children(&controller.bd, &parent).await;
        let decisions: Vec<_> = all
            .iter()
            .filter(|bead| {
                bead.labels
                    .iter()
                    .any(|label| label.as_str().starts_with("decision:integrity:"))
            })
            .collect();
        assert_eq!(
            decisions.len(),
            5,
            "each affected identity gets its own decision"
        );
        assert_eq!(build_queue(&all, None, None, true).len(), 5);
        let journal = crate::r#loop::DecisionWaitJournal::new(root);
        for decision in &decisions {
            assert!(journal.has_disposition(&parent, &decision.id).unwrap());
            assert_eq!(decision.issue_type, IssueType::Decision);
            let brief = parse_options_in(decision.notes.as_deref(), &decision.description).unwrap();
            let expected = if decision.description.contains("unneeded-pending-marker")
                || decision.description.contains("inputs-protocol-error")
            {
                2
            } else {
                4
            };
            assert_eq!(brief.options().len(), expected);
            assert_eq!(decision.status, Status::Blocked);
            assert!(
                !decision
                    .labels
                    .iter()
                    .any(|label| label.as_str().starts_with("finding:"))
            );
            assert!(decision.description.contains("specs/gate.md:"));
            assert!(
                brief
                    .options()
                    .iter()
                    .all(|option| option.body.contains("Cost:"))
            );
        }
        let provider = decisions
            .iter()
            .find(|bead| bead.description.contains("inputs-protocol-error"))
            .unwrap();
        for context in [
            "provider-check",
            "runner `provider`",
            "bash query.sh --print-inputs",
            "17",
            "definition schema mismatch",
        ] {
            assert!(
                provider.description.contains(context),
                "missing {context}: {}",
                provider.description
            );
        }
        assert_eq!(controller.bd.show(&parent).await.unwrap(), epic_before);
        assert_eq!(
            controller
                .bd
                .dependency_snapshot(&parent)
                .await
                .unwrap()
                .active_blockers(),
            [] as [loom_driver::identifier::BeadId; 0]
        );
        let first = decisions[0].id.clone();
        let second = decisions[1].id.clone();
        let mut work = Vec::new();
        for decision in [&first, &second] {
            let id = controller
                .bd
                .create(CreateOpts {
                    title: "explicitly bound follow-up".into(),
                    parent: Some(parent.clone()),
                    ..CreateOpts::default()
                })
                .await
                .unwrap();
            let decision_bead = controller.bd.show(decision).await.unwrap();
            let encoded = decision_bead.metadata["loom.integrity_decision"]
                .as_str()
                .unwrap();
            let record: serde_json::Value = serde_json::from_str(encoded).unwrap();
            let raw: loom_protocol::gate::RawFinding =
                serde_json::from_value(record["finding"].clone()).unwrap();
            let finding = raw.resolve(DispatchScope::PushGate, &validator).unwrap();
            let (batch, reference) = remediation
                .iter()
                .find_map(|task| {
                    let candidate = loom_protocol::acceptance::Candidates::from_metadata(
                        task.metadata.get(loom_protocol::acceptance::METADATA_KEY)?,
                    )
                    .unwrap();
                    let loom_protocol::acceptance::Candidates::Remediation { goals } = candidate
                    else {
                        return None;
                    };
                    goals
                        .as_slice()
                        .iter()
                        .find(|reference| reference.finding_hash.as_str() == finding.hash())
                        .map(|reference| (task.id.clone(), reference.clone()))
                })
                .unwrap();
            crate::mint::acceptance::admit_follow_up(
                &controller.bd,
                &controller.bd.show(&id).await.unwrap(),
                &crate::mint::acceptance::FollowUp {
                    source: batch,
                    goals: loom_protocol::acceptance::GoalReferences::new(vec![reference]).unwrap(),
                },
                root,
            )
            .await
            .unwrap();
            controller.bd.dep_add(&id, decision).await.unwrap();
            controller
                .bd
                .update(
                    decision,
                    UpdateOpts {
                        set_metadata: vec![("loom.decision_candidate".into(), serde_json::json!({
                    "version": 1, "producer": parent, "root": parent, "affected_work": [id]
                }).to_string())],
                        ..UpdateOpts::default()
                    },
                )
                .await
                .unwrap();
            assert!(matches!(
                validate_waiting_outcome(&controller.bd, &id, AgentOutcome::WaitingRequested)
                    .await
                    .unwrap(),
                AgentOutcome::Waiting { .. }
            ));
            work.push(id);
        }
        let mut bound_remediation = Vec::new();
        for bead in &remediation {
            bound_remediation.push(controller.bd.show(&bead.id).await.unwrap());
        }
        controller
            .bd
            .update(
                &first,
                UpdateOpts {
                    notes: Some("Human chose repair; implementation still required".into()),
                    ..UpdateOpts::default()
                },
            )
            .await
            .unwrap();
        controller
            .bd
            .close(&first, Some("human intent resolved"))
            .await
            .unwrap();
        let answered = controller.bd.show(&first).await.unwrap();
        controller.apply_integrity_clarify(&findings).await.unwrap();
        assert_eq!(controller.bd.show(&first).await.unwrap(), answered);
        assert_eq!(controller.iteration_count().await.unwrap(), 3);
        let after = children(&controller.bd, &parent).await;
        assert_eq!(build_queue(&after, None, None, true).len(), 4);
        let ready = controller
            .bd
            .ready(ReadyOpts {
                limit: Some(0),
                parent: Some(parent.clone()),
                ..ReadyOpts::default()
            })
            .await
            .unwrap();
        assert!(ready.iter().any(|bead| bead.id == work[0]));
        assert!(!ready.iter().any(|bead| bead.id == work[1]));
        assert_eq!(
            controller.bd.show(&work[0]).await.unwrap().status,
            Status::Open
        );
        assert_eq!(
            controller.bd.show(&second).await.unwrap().status,
            Status::Blocked
        );
        for bead in bound_remediation {
            assert_eq!(
                controller.bd.show(&bead.id).await.unwrap(),
                bead,
                "decision answers cannot resolve remediation"
            );
        }
        std::fs::write(
            root.join("query.sh"),
            "set -euo pipefail\nprintf '{\"inputs\":[\"verifier.sh\"]}\\n'\n",
        )
        .unwrap();
        let mut corrected = InputResolver::new(root.to_path_buf()).with_runners(runners.clone());
        assert_eq!(
            loom_gate::check_inputs_protocol(&annotations, &mut corrected),
            [] as [loom_gate::IntegrityFinding; 0]
        );
        let provider_annotation = annotations
            .iter()
            .find(|ann| ann.target == "provider-check")
            .unwrap()
            .clone();
        let outcomes = loom_gate::run_check(
            &[provider_annotation],
            &runners,
            &DispatchOptions::default(),
            root,
            &TierCwds::default(),
        );
        assert_eq!(outcomes.len(), 1);
        assert!(outcomes[0].as_ref().unwrap().verdict.pass);
        assert_eq!(controller.iteration_count().await.unwrap(), 3);
    }

    #[tokio::test]
    async fn integrity_cap_refuses_nondecision_provenance_without_replacing_identity() {
        for issue_type in [IssueType::Task, IssueType::Bug] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let bd = isolated_client(root).await;
            let parent = bd
                .create(CreateOpts {
                    title: "cap root".into(),
                    issue_type: Some(IssueType::Epic),
                    ..CreateOpts::default()
                })
                .await
                .unwrap();
            std::fs::create_dir(root.join("specs")).unwrap();
            std::fs::write(root.join("specs/gate.md"), "# Gate\n## Success Criteria\n- first [check](missing-one)\n- second [check](missing-two)\n").unwrap();
            let validator = WorkspaceFindingValidator::new(root);
            let findings = ["missing-one", "missing-two"].map(|target| {
                IntegrityFinding::UnresolvedAnnotation {
                    spec: "specs/gate.md".into(),
                    line: 3,
                    tier: Tier::Check,
                    target: target.into(),
                }
            });
            let finding = findings[0]
                .to_raw_finding()
                .unwrap()
                .resolve(DispatchScope::PushGate, &validator)
                .unwrap();
            let record = serde_json::json!({
                "work_root": parent,
                "finding": finding.clone().into_raw(),
                "disposition": "candidate"
            });
            let legacy = bd
                .create(CreateOpts {
                    title: "persisted mislabeled cap candidate".into(),
                    issue_type: Some(issue_type),
                    status: Some(Status::Blocked),
                    parent: Some(parent.clone()),
                    notes: Some("Human context must survive failed admission".into()),
                    labels: vec![format!("decision:integrity:{}", finding.hash())],
                    metadata: Some(
                        serde_json::json!({
                            "loom.integrity_decision": serde_json::to_string(&record).unwrap()
                        })
                        .to_string(),
                    ),
                    ..CreateOpts::default()
                })
                .await
                .unwrap();
            let before = children(&bd, &parent).await;
            let epic_before = bd.show(&parent).await.unwrap();
            let error = crate::mint::integrity::escalate(
                &bd,
                &crate::r#loop::DecisionWaitJournal::new(root),
                &parent,
                &findings,
                &validator,
                root,
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains(legacy.as_str()), "{error}");
            assert!(
                error.to_string().contains("identity-preserving repair"),
                "{error}"
            );
            assert_eq!(children(&bd, &parent).await, before);
            assert_eq!(bd.show(&parent).await.unwrap(), epic_before);
        }
    }

    #[tokio::test]
    async fn integrity_cap_refuses_duplicate_decisions_before_new_queue_effects() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let bd = isolated_client(root).await;
        let parent = bd
            .create(CreateOpts {
                title: "cap root".into(),
                issue_type: Some(IssueType::Epic),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        std::fs::create_dir(root.join("specs")).unwrap();
        std::fs::write(root.join("specs/gate.md"), "# Gate\n## Success Criteria\n- first [check](missing-one)\n- second [check](missing-two)\n").unwrap();
        let validator = WorkspaceFindingValidator::new(root);
        let findings =
            ["missing-one", "missing-two"].map(|target| IntegrityFinding::UnresolvedAnnotation {
                spec: "specs/gate.md".into(),
                line: 3,
                tier: Tier::Check,
                target: target.into(),
            });
        crate::mint::integrity::escalate(
            &bd,
            &crate::r#loop::DecisionWaitJournal::new(root),
            &parent,
            &findings[..1],
            &validator,
            root,
        )
        .await
        .unwrap();
        let existing = children(&bd, &parent).await.remove(0);
        let existing = bd.show(&existing.id).await.unwrap();
        let duplicate = bd
            .create(CreateOpts {
                issue_type: Some(IssueType::Decision),
                title: "duplicate indexed decision".into(),
                status: Some(Status::Blocked),
                parent: Some(parent.clone()),
                description: existing.description.clone(),
                labels: existing.labels.iter().map(ToString::to_string).collect(),
                metadata: Some(serde_json::to_string(&existing.metadata).unwrap()),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        let before = children(&bd, &parent).await;
        let error = crate::mint::integrity::escalate(
            &bd,
            &crate::r#loop::DecisionWaitJournal::new(root),
            &parent,
            &findings,
            &validator,
            root,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("multiple decisions"));
        assert!(error.to_string().contains(existing.id.as_str()));
        assert!(error.to_string().contains(duplicate.as_str()));
        assert_eq!(children(&bd, &parent).await, before);
    }
}

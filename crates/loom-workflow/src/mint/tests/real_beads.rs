#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use loom_driver::bd::{IssueType, ReadyOpts, Status};
    use loom_driver::clock::SystemClock;
    use tokio::process::Command;

    use super::super::*;

    struct IsolatedBeads {
        root: PathBuf,
    }

    impl CommandRunner for IsolatedBeads {
        async fn run(&self, args: Vec<OsString>, timeout: Duration) -> Result<RunOutput, BdError> {
            let mut command = Command::new("bd");
            command
                .args(["--sandbox", "--dolt-auto-commit", "off"])
                .args(args)
                .current_dir(&self.root)
                .env("HOME", self.root.join("home"));
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

    async fn client(root: &Path) -> BdClient<IsolatedBeads> {
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
                    "mint",
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

    async fn work_epic(bd: &BdClient<IsolatedBeads>) -> BeadId {
        bd.create(CreateOpts {
            title: "molecule findings".into(),
            issue_type: Some(IssueType::Epic),
            labels: vec!["loom:active".into(), "spec:agent".into()],
            ..CreateOpts::default()
        })
        .await
        .unwrap()
    }

    async fn children(bd: &BdClient<IsolatedBeads>, parent: &BeadId) -> Vec<Bead> {
        bd.list(ListOpts {
            all: true,
            limit: Some(0),
            parent: Some(parent.clone()),
            ..ListOpts::default()
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn molecule_findings_use_parentage_without_ancestor_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let bd = client(dir.path()).await;
        let parent = work_epic(&bd).await;
        let molecule = MoleculeId::new(parent.as_str()).unwrap();
        let mut blocking = style_finding(
            vec![spec("agent"), spec("harness")],
            "RS-9",
            "pushed behavior needs remediation",
        )
        .into_raw();
        blocking.route = FindingRoute::Blocking;
        let findings = [
            loom_test_support::finding::resolve(blocking).unwrap(),
            style_finding(vec![spec("agent")], "RS-11", "adjacent drift"),
            decision_finding("valid-decision", DECISION_BRIEF),
            decision_finding("missing-options", "decision evidence without a brief"),
        ];
        let opts = MintOptions {
            suppress_closed_same_molecule: true,
            ..MintOptions::default()
        };

        let summary = route_molecule_findings(&bd, &molecule, &findings, &opts).await;
        assert_eq!(summary.errors, 0, "{summary:?}");
        assert_eq!(summary.refused, 0, "{summary:?}");
        assert_eq!(summary.minted, 4, "{summary:?}");
        assert_eq!(summary.blocking_findings, 1);
        assert_eq!(summary.ready_remediation_batches, 1);
        assert_eq!(summary.deferred_findings_merged, 1);
        assert_eq!(summary.clarify_findings_raised, 1);
        let materialized = children(&bd, &parent).await;
        assert_eq!(materialized.len(), findings.len());
        let ready = bd
            .ready(ReadyOpts {
                parent: Some(parent.clone()),
                ..ReadyOpts::default()
            })
            .await
            .unwrap();
        assert_eq!(ready.len(), 1, "{ready:?}");

        for (finding, status) in findings.iter().zip([
            Status::Open,
            Status::Deferred,
            Status::Blocked,
            Status::Blocked,
        ]) {
            let child = materialized
                .iter()
                .find(|bead| {
                    bead.labels
                        .iter()
                        .any(|label| label.as_str() == finding_label(finding))
                })
                .unwrap();
            let persisted = bd.show(&child.id).await.unwrap();
            assert_eq!(persisted.parent.as_ref(), Some(&parent));
            assert_eq!(persisted.status, status);
            assert!(persisted.description.contains(&finding.id()));
            assert!(persisted.description.contains(&finding.hash()));
            assert!(
                persisted
                    .description
                    .contains(finding.evidence().trim_end())
            );
            for bond in finding.bonds() {
                assert!(
                    persisted
                        .labels
                        .iter()
                        .any(|label| { label.as_str() == format!("spec:{bond}") })
                );
            }
            assert!(
                persisted
                    .labels
                    .iter()
                    .any(|label| label.as_str() == "profile:rust")
            );
            assert_eq!(
                bd.dependency_snapshot(&child.id)
                    .await
                    .unwrap()
                    .active_blockers(),
                [] as [BeadId; 0]
            );
            if finding.route() == FindingRoute::Blocking {
                assert_eq!(ready[0].id, child.id);
            }
            if finding.route() == FindingRoute::Deferred {
                assert!(persisted.labels.iter().any(Label::is_deferred));
            }
            if finding.route() == FindingRoute::Clarify {
                let valid_brief = parse_options(finding.evidence()).is_ok();
                assert_eq!(persisted.labels.iter().any(Label::is_clarify), valid_brief);
                assert_eq!(persisted.labels.iter().any(Label::is_blocked), !valid_brief);
            }
        }

        let rerun = route_molecule_findings(&bd, &molecule, &findings, &opts).await;
        assert_eq!(rerun.errors, 0, "{rerun:?}");
        assert_eq!(rerun.minted, 0);
        assert_eq!(rerun.skipped, findings.len());
        assert_eq!(children(&bd, &parent).await, materialized);

        bd.close(&ready[0].id, None).await.unwrap();
        let closed = bd.show(&ready[0].id).await.unwrap();
        let rerun = route_molecule_findings(&bd, &molecule, &findings, &opts).await;
        assert_eq!(rerun.errors, 0, "{rerun:?}");
        assert_eq!(rerun.minted, 0);
        assert_eq!(rerun.skipped, findings.len());
        assert!(rerun.batches.iter().any(|batch| matches!(
            batch,
            BatchOutcome::SkippedClosed { existing_bead, .. } if *existing_bead == closed.id
        )));
        assert_eq!(bd.show(&closed.id).await.unwrap(), closed);
        assert_eq!(children(&bd, &parent).await.len(), findings.len());
    }

    #[tokio::test]
    async fn deferred_merge_and_promotion_retain_finding_context() {
        let dir = tempfile::tempdir().unwrap();
        let bd = client(dir.path()).await;
        let parent = work_epic(&bd).await;
        let molecule = MoleculeId::new(parent.as_str()).unwrap();
        let findings = [
            style_finding(vec![spec("agent")], "RS-9", "first finding evidence"),
            style_finding(
                vec![spec("agent"), spec("harness")],
                "RS-11",
                "second finding evidence",
            ),
        ];
        for finding in &findings {
            let summary = route_molecule_findings(
                &bd,
                &molecule,
                std::slice::from_ref(finding),
                &MintOptions::default(),
            )
            .await;
            assert_eq!(summary.errors, 0, "{summary:?}");
            assert_eq!(summary.deferred_findings_merged, 1, "{summary:?}");
        }
        let batches = children(&bd, &parent).await;
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].status, Status::Deferred);
        let summary = promote_deferred(&bd, &molecule, false).await;
        assert_eq!(summary.errors, 0, "{summary:?}");
        assert_eq!(summary.promoted_deferred, 1);
        let promoted = bd.show(&batches[0].id).await.unwrap();
        assert_eq!(promoted.parent.as_ref(), Some(&parent));
        assert_eq!(promoted.status, Status::Open);
        assert!(!promoted.labels.iter().any(Label::is_deferred));
        for finding in &findings {
            assert!(
                promoted
                    .labels
                    .iter()
                    .any(|label| label.as_str() == finding_label(finding))
            );
            assert!(promoted.description.contains(&finding.id()));
            assert!(promoted.description.contains(finding.evidence()));
        }
        let ready = bd
            .ready(ReadyOpts {
                parent: Some(parent.clone()),
                ..ReadyOpts::default()
            })
            .await
            .unwrap();
        assert_eq!(ready.len(), 1, "{ready:?}");
        assert_eq!(ready[0].id, promoted.id);
        assert_eq!(children(&bd, &parent).await.len(), 1);
    }
}

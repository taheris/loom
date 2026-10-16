//! Production preparation with real isolated native Beads and package reads.

#[cfg(test)]
mod cases {
    use crate::r#loop::AgentOutcome;
    use crate::r#loop::worker::*;
    use crate::test_support::{IsolatedBeads, acceptance, client};
    use loom_driver::agent::AgentBackend;
    use loom_driver::agent::AgentRuntime;
    use loom_driver::bd::{BdClient, Bead};
    use loom_driver::bd::{CreateOpts, UpdateOpts};
    use loom_driver::config::{LoomTopConfig, Phase, SkillsConfig};
    use loom_driver::git::GitClient;
    use loom_driver::identifier::{ProfileName, SpecLabel};
    use loom_driver::profile_manifest::ProfileImageManifest;
    use loom_driver::scratch::{ScratchSession, resolve_scratch_key};
    use loom_protocol::acceptance::{Candidates, METADATA_KEY};
    use std::path::Path;

    const REQUIREMENT: &str = "Workers retain the dispatched acceptance while current bindings are resolved anew for subsequent dispatches.";

    struct Fixture {
        dir: tempfile::TempDir,
        workspace: std::path::PathBuf,
        bd: BdClient<IsolatedBeads>,
        task: Bead,
        manifest: ProfileImageManifest,
        label: SpecLabel,
    }

    impl Fixture {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let bd = client(dir.path()).await;
            let git = loom_driver::git::init_test_repo_with_integration(dir.path()).unwrap();
            let id = bd
                .create(CreateOpts {
                    title: "ordinary explicit acceptance".into(),
                    ..CreateOpts::default()
                })
                .await
                .unwrap();
            let label = SpecLabel::new("harness").unwrap();
            let workspace = git.create_worktree(&label, &id).await.unwrap().path;
            acceptance::write_contract(&workspace, &label, REQUIREMENT);
            let manifest_path = dir.path().join("images.json");
            loom_test_support::profile_manifest::write(&manifest_path, r#"{"base":{"pi":{"ref":"fixture","source":"/fixture/image","source_kind":"nix-descriptor"}},"rust":{"pi":{"ref":"fixture-rust","source":"/fixture/rust-image","source_kind":"nix-descriptor"}}}"#, Path::new("/fixture/wrix")).unwrap();
            let task = bd.show(&id).await.unwrap();
            Self {
                manifest: ProfileImageManifest::from_path(&manifest_path).unwrap(),
                dir,
                workspace,
                bd,
                task,
                label,
            }
        }

        async fn set(&mut self, value: Option<&str>) {
            self.bd
                .update(
                    &self.task.id,
                    UpdateOpts {
                        set_metadata: value
                            .into_iter()
                            .map(|value| (METADATA_KEY.to_owned(), value.to_owned()))
                            .collect(),
                        unset_metadata: if value.is_none() {
                            vec![METADATA_KEY.to_owned()]
                        } else {
                            Vec::new()
                        },
                        ..UpdateOpts::default()
                    },
                )
                .await
                .unwrap();
            self.task = self.bd.show(&self.task.id).await.unwrap();
        }

        async fn bind(&mut self) {
            self.set(Some(
                &serde_json::to_string(&acceptance::candidates(&self.workspace, &self.label))
                    .unwrap(),
            ))
            .await;
        }

        async fn prepare(&self) -> Preparation {
            prepare_with_bd(
                &self.bd,
                Request {
                    bead: &self.task,
                    workspace: &self.workspace,
                    loom_workspace: self.dir.path(),
                    manifest: &self.manifest,
                    cli_profile: None,
                    phase_default: &ProfileName::base(),
                    runtime: AgentRuntime::Pi,
                    label: &self.label,
                    style_rules: "docs/style-rules.md",
                    loom: &LoomTopConfig::default(),
                    skills: &SkillsConfig::default(),
                    launcher_env: Vec::new(),
                    previous_failure: None,
                    bead_base: GitClient::open(&self.workspace)
                        .unwrap()
                        .head_commit_sha()
                        .await
                        .unwrap(),
                    workspace_recovery: None,
                    attempt: 0,
                },
            )
            .await
            .unwrap()
        }

        async fn rejected(&self) {
            let Preparation::Rejected(AgentOutcome::Blocked { reason }) = self.prepare().await
            else {
                panic!("invalid metadata admitted worker preparation")
            };
            assert!(
                reason.contains("acceptance rejected before spawn"),
                "{reason}"
            );
            assert_eq!(
                self.bd.show(&self.task.id).await.unwrap(),
                self.task,
                "rejection preserves native human state"
            );
        }
    }

    fn ready(prepared: Preparation) -> Box<Worker> {
        match prepared {
            Preparation::Ready(worker) => worker,
            Preparation::Rejected(outcome) => panic!("valid acceptance rejected: {outcome:?}"),
        }
    }

    #[tokio::test]
    async fn native_worker_rejects_unbound_mixed_empty_unknown_and_malformed_acceptance_before_scratch()
     {
        let mut fixture = Fixture::new().await;
        let encoded =
            serde_json::to_string(&acceptance::candidates(&fixture.workspace, &fixture.label))
                .unwrap();
        let mut mixed: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        mixed["goals"] = serde_json::json!([]);
        for value in [
            None,
            Some("{broken".to_owned()),
            Some(r#"{"purpose":"unknown","criteria":[]}"#.to_owned()),
            Some(r#"{"purpose":"implementation","criteria":[]}"#.to_owned()),
            Some(r#"{"purpose":"remediation","goals":[]}"#.to_owned()),
            Some(mixed.to_string()),
        ] {
            fixture.set(value.as_deref()).await;
            fixture.rejected().await;
            let key = resolve_scratch_key(
                Phase::Loop,
                std::slice::from_ref(&fixture.label),
                Some(&fixture.task.id),
            );
            assert!(!ScratchSession::scratchpad_path_for(&fixture.workspace, &key).exists());
        }
    }

    #[tokio::test]
    async fn native_worker_acceptance_reaches_controlled_external_agent_only_after_explicit_binding()
     {
        let mut fixture = Fixture::new().await;
        let mock = fixture.dir.path().join("mock-pi.sh");
        loom_test_support::write_executable_bash_script(
            &mock,
            include_str!("../../../../../tests/mock-pi/pi.sh"),
        )
        .unwrap();
        let sentinel = fixture.dir.path().join("agent-started");
        let launcher = fixture.dir.path().join("wrix");
        loom_test_support::write_executable_bash_script(&launcher, &format!("set -euo pipefail\nprintf started > {sentinel:?}\nprintf '[wrix] Starting container (isolated fixture)\\n' >&2\nexport MOCK_PI_SCENARIO=task-acceptance\nexec \"$BASH\" {mock:?}\n")).unwrap();
        let manifest = fixture.dir.path().join("external-agent.json");
        loom_test_support::profile_manifest::write(&manifest, r#"{"base":{"pi":{"ref":"fixture","source":"/fixture/image","source_kind":"nix-descriptor"}}}"#, &launcher).unwrap();
        fixture.manifest = ProfileImageManifest::from_path(&manifest).unwrap();
        fixture.rejected().await;
        assert!(
            !sentinel.exists(),
            "unbound task must not reach external agent"
        );
        fixture.bind().await;
        let worker = ready(fixture.prepare().await);
        let mut text = String::new();
        let result = crate::agent::run_agent_classified::<loom_agent::pi::backend::PiBackend>(
            &worker.spawn,
            None,
            None,
            Some(&mut text),
            None,
        )
        .await;
        let (result, terminal) =
            crate::agent::admit_session_output(result, &text, loom_protocol::output::Phase::Loop);
        let proposal = crate::r#loop::classify_session(result, terminal.as_ref());
        assert_eq!(
            proposal,
            AgentOutcome::Success,
            "external fixture checks the actual resolved prompt before proposing completion"
        );
        assert!(sentinel.exists());
        assert_eq!(
            fixture.bd.show(&fixture.task.id).await.unwrap(),
            fixture.task,
            "agent proposal is not source completion or publication authority"
        );
    }

    #[tokio::test]
    async fn native_worker_never_prepares_decisions_or_epics_with_ordinary_bindings() {
        let mut fixture = Fixture::new().await;
        fixture.bind().await;
        for kind in [
            loom_driver::bd::IssueType::Decision,
            loom_driver::bd::IssueType::Epic,
        ] {
            let id = fixture
                .bd
                .create(CreateOpts {
                    title: "not an implementation task".into(),
                    issue_type: Some(kind),
                    metadata: Some(serde_json::to_string(&fixture.task.metadata).unwrap()),
                    ..CreateOpts::default()
                })
                .await
                .unwrap();
            fixture.task = fixture.bd.show(&id).await.unwrap();
            fixture.rejected().await;
        }
    }

    #[tokio::test]
    async fn native_minted_repair_goals_render_attribution_and_survive_compaction_without_ordinary_acceptance()
     {
        assert_minted_repair_context().await;
    }

    async fn assert_minted_repair_context() {
        let mut fixture = Fixture::new().await;
        std::fs::write(
            fixture.workspace.join("specs/harness.md"),
            format!("# Harness\n\n## Success Criteria\n\n- {REQUIREMENT}\n"),
        )
        .unwrap();
        assert!(
            loom_gate::acceptance::load(&fixture.workspace).is_err(),
            "actual missing annotation, not an invented criterion status"
        );
        let root = fixture
            .bd
            .create(CreateOpts {
                title: "active fixture work".into(),
                issue_type: Some(loom_driver::bd::IssueType::Epic),
                labels: vec!["loom:active".into(), "spec:harness".into()],
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        let finding = loom_protocol::gate::RawFinding {
            token: loom_protocol::gate::ConcernToken::SpecConventionsViolation,
            route: loom_protocol::gate::FindingRoute::Deferred,
            bonds: vec![fixture.label.clone()],
            target: loom_protocol::gate::FindingTarget::Criterion {
                spec: fixture.label.clone(),
                anchor: loom_protocol::criterion::CriterionId::for_spec_text(
                    &fixture.label,
                    REQUIREMENT,
                )
                .to_string(),
            },
            evidence: "specs/harness.md: the assigned requirement has no verifier annotation"
                .into(),
        }
        .resolve(
            loom_protocol::gate::DispatchScope::Tree,
            &crate::review::WorkspaceFindingValidator::new(&fixture.workspace),
        )
        .unwrap();
        let summary =
            crate::mint::mint_findings(&fixture.bd, std::slice::from_ref(&finding), "fixture-head")
                .await;
        assert_eq!(summary.minted, 1, "{summary:?}");
        let crate::mint::BatchOutcome::Minted { bead_id, .. } = &summary.batches[0] else {
            panic!("native batch not minted")
        };
        fixture.task = fixture.bd.show(bead_id).await.unwrap();
        let worker = ready(fixture.prepare().await);
        let crate::r#loop::acceptance::Resolved::Remediation(resolved) = &worker.acceptance else {
            panic!("repair silently became implementation")
        };
        assert_eq!(resolved.goals().len(), 1);
        assert_eq!(resolved.goals()[0].work_root(), &root);
        assert_eq!(resolved.goals()[0].finding(), &finding);
        let prompt = worker.spawn.initial_prompt.clone();
        assert!(prompt.contains("Applicable Package Snapshot"));
        let frozen =
            std::fs::read_to_string(worker.scratch.path().join("context/harness/acceptance.md"))
                .unwrap();
        assert!(frozen.contains(REQUIREMENT));
        for expected in [
            "Purpose: remediation",
            fixture.task.id.as_str(),
            root.as_str(),
            finding.evidence(),
            finding.hash().as_str(),
        ] {
            assert!(prompt.contains(expected), "missing goal context {expected}");
        }
        acceptance::write_contract(
            &fixture.workspace,
            &fixture.label,
            "Later workspace wording must not replace dispatched repair goals.",
        );
        let repin = loom_agent::pi::backend::PiBackend::compaction_repin(&worker.spawn)
            .unwrap()
            .unwrap();
        assert!(repin.contains(&prompt));
        assert!(!repin.contains("Later workspace wording"));
        assert_eq!(
            std::fs::read_to_string(worker.scratch.path().join("context/harness/acceptance.md"))
                .unwrap(),
            frozen
        );
        assert_eq!(
            fixture.bd.show(&fixture.task.id).await.unwrap(),
            fixture.task
        );
    }

    #[tokio::test]
    async fn native_ordinary_split_requires_explicit_current_references_before_worker_preparation()
    {
        let mut fixture = Fixture::new().await;
        fixture.bind().await;
        let source = fixture.task.clone();
        fixture
            .bd
            .update(
                &source.id,
                UpdateOpts {
                    status: Some(loom_driver::bd::Status::Closed),
                    ..UpdateOpts::default()
                },
            )
            .await
            .unwrap();
        let id = fixture
            .bd
            .create(CreateOpts {
                title: "Remaining ordinary split work".into(),
                parent: Some(source.id.clone()),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        fixture.task = fixture.bd.show(&id).await.unwrap();
        fixture.rejected().await;
        fixture.bind().await;
        let worker = ready(fixture.prepare().await);
        assert!(
            worker
                .spawn
                .initial_prompt
                .contains("Purpose: implementation")
        );
        assert!(worker.spawn.initial_prompt.contains(REQUIREMENT));
        let preserved = fixture.bd.show(&source.id).await.unwrap();
        assert_eq!(preserved.status, loom_driver::bd::Status::Closed);
        assert_eq!(preserved.metadata, source.metadata);
    }

    #[tokio::test]
    async fn native_partial_remediation_split_persists_checked_subset_then_prepares_frozen_worker_context()
     {
        let mut fixture = Fixture::new().await;
        std::fs::write(
            fixture.workspace.join("specs/harness.md"),
            format!("# Harness\n\n## Success Criteria\n\n- {REQUIREMENT}\n"),
        )
        .unwrap();
        let repair_file = fixture.workspace.join("partly-fixed.rs");
        std::fs::write(&repair_file, "fixture repair target").unwrap();
        fixture
            .bd
            .create(CreateOpts {
                title: "Active native split root".into(),
                issue_type: Some(loom_driver::bd::IssueType::Epic),
                labels: vec!["loom:active".into(), "spec:harness".into()],
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        let remaining = loom_protocol::gate::RawFinding {
            token: loom_protocol::gate::ConcernToken::SpecConventionsViolation,
            route: loom_protocol::gate::FindingRoute::Deferred,
            bonds: vec![fixture.label.clone()],
            target: loom_protocol::gate::FindingTarget::Criterion {
                spec: fixture.label.clone(),
                anchor: loom_protocol::criterion::CriterionId::for_spec_text(
                    &fixture.label,
                    REQUIREMENT,
                )
                .to_string(),
            },
            evidence: "Remaining missing annotation must survive partial progress.".into(),
        }
        .resolve(
            loom_protocol::gate::DispatchScope::Tree,
            &crate::review::WorkspaceFindingValidator::new(&fixture.workspace),
        )
        .unwrap();
        let repaired = loom_protocol::gate::RawFinding {
            token: loom_protocol::gate::ConcernToken::MockDiscipline,
            route: loom_protocol::gate::FindingRoute::Deferred,
            bonds: vec![fixture.label.clone()],
            target: loom_protocol::gate::FindingTarget::TestPath {
                path: "partly-fixed.rs".into(),
            },
            evidence: "ALREADY_PROCESSED_FINDING_NOT_ASSIGNED_TO_SPLIT".into(),
        }
        .resolve(
            loom_protocol::gate::DispatchScope::Tree,
            &crate::review::WorkspaceFindingValidator::new(&fixture.workspace),
        )
        .unwrap();
        let summary = crate::mint::mint_findings(
            &fixture.bd,
            &[remaining.clone(), repaired],
            "native-split-fixture",
        )
        .await;
        assert_eq!(summary.minted, 1, "{summary:?}");
        let crate::mint::BatchOutcome::Minted { bead_id, .. } = &summary.batches[0] else {
            panic!("native split source was not minted")
        };
        let source = fixture.bd.show(bead_id).await.unwrap();
        let Candidates::Remediation { goals: original } =
            Candidates::from_metadata(&source.metadata[METADATA_KEY]).unwrap()
        else {
            panic!("source lost remediation purpose")
        };
        assert_eq!(original.as_slice().len(), 2);
        let subset = loom_protocol::acceptance::GoalReferences::new(
            original
                .as_slice()
                .iter()
                .filter(|reference| reference.finding_hash.as_str() == remaining.hash())
                .cloned()
                .collect(),
        )
        .unwrap();
        std::fs::remove_file(repair_file).unwrap();
        fixture
            .bd
            .update(
                &source.id,
                UpdateOpts {
                    status: Some(loom_driver::bd::Status::Closed),
                    ..UpdateOpts::default()
                },
            )
            .await
            .unwrap();
        let id = fixture.bd.create(CreateOpts {
            title: "Remaining native remediation split".into(), parent: source.parent.clone(),
            metadata: Some(serde_json::json!({ crate::mint::acceptance::FOLLOW_UP_KEY: serde_json::to_string(&crate::mint::acceptance::FollowUp { source: source.id.clone(), goals: subset.clone() }).unwrap(), "operator.note": "preserve human intent" }).to_string()),
            ..CreateOpts::default()
        }).await.unwrap();
        fixture.task = fixture.bd.show(&id).await.unwrap();
        assert!(!fixture.task.metadata.contains_key(METADATA_KEY));
        let worker = ready(fixture.prepare().await);
        let crate::r#loop::acceptance::Resolved::Remediation(resolved) = &worker.acceptance else {
            panic!("split changed purpose")
        };
        assert_eq!(resolved.goals().len(), 1);
        assert_eq!(resolved.goals()[0].finding(), &remaining);
        assert!(worker.spawn.initial_prompt.contains(remaining.evidence()));
        assert!(
            !worker
                .spawn
                .initial_prompt
                .contains("ALREADY_PROCESSED_FINDING_NOT_ASSIGNED_TO_SPLIT")
        );
        let prompt = worker.spawn.initial_prompt.clone();
        let snapshot = worker.scratch.path().join("context/harness/acceptance.md");
        let frozen = std::fs::read_to_string(&snapshot).unwrap();
        std::fs::write(
            fixture.workspace.join("specs/harness.md"),
            "Changed source after split dispatch.",
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&snapshot).unwrap(), frozen);
        let repin = loom_agent::pi::backend::PiBackend::compaction_repin(&worker.spawn)
            .unwrap()
            .unwrap();
        assert!(repin.contains(&prompt));
        assert!(!repin.contains("Changed source after split dispatch"));
        let admitted = fixture.bd.show(&id).await.unwrap();
        assert_eq!(admitted.metadata["operator.note"], "preserve human intent");
        assert!(
            !admitted
                .metadata
                .contains_key(crate::mint::acceptance::FOLLOW_UP_KEY)
        );
        assert_eq!(
            Candidates::from_metadata(&admitted.metadata[METADATA_KEY]).unwrap(),
            Candidates::Remediation { goals: subset }
        );
        let preserved = fixture.bd.show(&source.id).await.unwrap();
        assert_eq!(preserved.status, loom_driver::bd::Status::Closed);
        assert_eq!(
            Candidates::from_metadata(&preserved.metadata[METADATA_KEY]).unwrap(),
            Candidates::Remediation { goals: original }
        );
    }

    #[tokio::test]
    async fn worker_context_consumes_resolved_acceptance() {
        let mut fixture = Fixture::new().await;
        fixture.bind().await;
        let spec = fixture.workspace.join("specs/harness.md");
        let body = std::fs::read_to_string(&spec).unwrap();
        std::fs::write(
            &spec,
            format!("{body}\nInvariant: dispatched human intent is immutable.\n"),
        )
        .unwrap();
        let worker = ready(fixture.prepare().await);
        let prompt = worker.spawn.initial_prompt.clone();
        assert!(prompt.contains("Invariant: dispatched human intent is immutable."));
        assert!(prompt.contains("Assigned worker goals remain mandatory"));
        assert!(prompt.contains("Purpose: implementation"));
        let snapshot = worker.scratch.path().join("context/harness/acceptance.md");
        let frozen = std::fs::read_to_string(&snapshot).unwrap();
        std::fs::write(&spec, "Later source must not rewrite dispatched intent.").unwrap();
        assert_eq!(std::fs::read_to_string(&snapshot).unwrap(), frozen);
        let repin = loom_agent::pi::backend::PiBackend::compaction_repin(&worker.spawn)
            .unwrap()
            .unwrap();
        assert!(repin.contains(&prompt));
        assert!(!repin.contains("Later source must not rewrite"));
        assert_eq!(
            fixture.bd.show(&fixture.task.id).await.unwrap(),
            fixture.task
        );
        assert_minted_repair_context().await;
    }

    #[tokio::test]
    async fn native_worker_refreshes_current_bindings_but_repin_retains_dispatched_acceptance() {
        let mut fixture = Fixture::new().await;
        fixture.bind().await;
        let worker = ready(fixture.prepare().await);
        let crate::r#loop::acceptance::Resolved::Implementation(resolved) = &worker.acceptance
        else {
            panic!("ordinary work changed purpose")
        };
        assert_eq!(resolved.obligations().len(), 1);
        assert!(resolved.obligations()[0].annotation().pending);
        assert!(worker.spawn.initial_prompt.contains(REQUIREMENT));
        assert!(
            worker
                .spawn
                .initial_prompt
                .contains("Purpose: implementation")
        );
        assert!(
            !worker
                .spawn
                .initial_prompt
                .contains(&fixture.dir.path().display().to_string())
        );
        let old_prompt = worker.spawn.initial_prompt.clone();
        let spec = fixture.workspace.join("specs/harness.md");
        let body = std::fs::read_to_string(&spec).unwrap();
        std::fs::write(
            &spec,
            body.replace("fixture_worker_contract", "updated_pending_verifier"),
        )
        .unwrap();
        let repin = loom_agent::pi::backend::PiBackend::compaction_repin(&worker.spawn)
            .unwrap()
            .unwrap();
        assert!(repin.contains(&old_prompt));
        assert!(repin.contains("fixture_worker_contract"));
        assert!(!repin.contains("updated_pending_verifier"));
        drop(worker);
        let refreshed = ready(fixture.prepare().await);
        assert!(
            refreshed
                .spawn
                .initial_prompt
                .contains("updated_pending_verifier")
        );
        assert!(
            !refreshed
                .spawn
                .initial_prompt
                .contains("fixture_worker_contract")
        );
        drop(refreshed);
        acceptance::write_contract(
            &fixture.workspace,
            &fixture.label,
            "Changed requirement requires explicit rebinding.",
        );
        fixture.rejected().await;
        fixture.bind().await;
        let rebound = ready(fixture.prepare().await);
        assert!(
            rebound
                .spawn
                .initial_prompt
                .contains("Changed requirement requires explicit rebinding.")
        );
        assert!(!rebound.spawn.initial_prompt.contains(REQUIREMENT));
        drop(rebound);
        std::fs::rename(&spec, fixture.workspace.join("specs/secondary.md")).unwrap();
        std::fs::write(
            fixture.workspace.join("docs/README.md"),
            "- [secondary](../specs/secondary.md)\n",
        )
        .unwrap();
        fixture.rejected().await;
        assert!(matches!(
            Candidates::from_metadata(&fixture.task.metadata[METADATA_KEY]).unwrap(),
            Candidates::Implementation { .. }
        ));
    }
}

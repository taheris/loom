#[cfg(test)]
mod cases {
    use super::super::*;
    use crate::test_support::{IsolatedBeads, client};
    use loom_driver::bd::{CreateOpts, IssueType, Status};
    use loom_protocol::output::{Phase, decode};

    #[tokio::test]
    async fn native_controller_counts_one_noop_source_and_five_distinct_decisions() {
        let dir = tempfile::tempdir().unwrap();
        let bd = client(dir.path()).await;
        let git = git_workspace(dir.path());
        seed_spec(dir.path(), "loop");
        let root = bd
            .create(CreateOpts {
                title: "work".into(),
                issue_type: Some(IssueType::Epic),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        let source = bd
            .create(CreateOpts {
                title: "source".into(),
                parent: Some(root.clone()),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        let affected = bd
            .create(CreateOpts {
                title: "affected implementation".into(),
                parent: Some(root.clone()),
                ..CreateOpts::default()
            })
            .await
            .unwrap();
        let source = bd.show(&source).await.unwrap();
        let agent = Arc::new(BdClient::with_runner(IsolatedBeads::attach(dir.path())));
        let manifest = write_manifest(dir.path());
        let spawn_root = root.clone();
        let mut controller = ProductionAgentLoopController::new(bd, SpecLabel::new("loop").unwrap(),
            PathBuf::from("unused-loom"),dir.path().to_path_buf(),git,manifest,None,ProfileName::new("base").unwrap(),
            move |_, id| {
                let agent=Arc::clone(&agent); let root=spawn_root.clone(); let affected=affected.clone();
                async move {
                    agent.close(&id, Some("completed independent source work")).await.unwrap();
                    let mut decisions=Vec::new();
                    for index in 0..5 {
                        let decision=agent.create(CreateOpts {
                            title:format!("decision {index}"), issue_type:Some(IssueType::Decision),
                            status:Some(Status::Blocked),parent:Some(id.clone()),
                            description:if index == 4 {"needs a brief".into()} else {"## Options — Choose\n\n### Option 1 — Retain\nKeep the current contract.".into()},
                            metadata:Some(serde_json::json!({"loom.decision_candidate":serde_json::json!({"version":1,"producer":id,"root":root,"affected_work":[affected]}).to_string()}).to_string()),
                            ..CreateOpts::default()
                        }).await.unwrap();
                        agent.dep_add(&affected,&decision).await.unwrap();
                        decisions.push(decision);
                    }
                    let record=format!("LOOM_CLARIFY: {{\"decisions\":{}}}\n",serde_json::to_string(&decisions).unwrap());
                    let wire=format!("{record}{record}LOOM_NOOP");
                    (SessionResult::Decoded { outcome:SessionOutcome {exit_code:0,cost_usd:None},output:Arc::new(decode(&wire,Phase::Loop))},Some(ExitSignal::Noop))
                }
            }).with_fixed_queue(VecDeque::from([source.clone()])).with_ready_parent(root);
        let summary = crate::r#loop::run_loop(
            &mut controller,
            crate::r#loop::RetryPolicy { max_retries: 0 },
            1,
        )
        .await
        .unwrap();
        assert_eq!(summary.beads_processed, 1);
        assert_eq!(summary.beads_clarified, 5);
        assert_eq!(summary.beads_waiting, 0);
        let current = controller.bd.show(&source.id).await.unwrap();
        assert_eq!(current.status, Status::Closed);
        assert!(!current.labels.iter().any(|label| matches!(
            label.as_str(),
            "loom:clarify" | "loom:blocked" | "loom:decision-wait"
        )));
        let next = crate::r#loop::run_loop(
            &mut controller,
            crate::r#loop::RetryPolicy { max_retries: 0 },
            1,
        )
        .await
        .unwrap();
        assert_eq!(next.beads_clarified, 0);
    }
}

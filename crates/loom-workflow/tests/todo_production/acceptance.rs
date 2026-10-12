use super::*;
use loom_driver::identifier::BeadId;
use loom_protocol::acceptance::{Candidates, METADATA_KEY, Reference, References};
use loom_protocol::criterion::CriterionId;
use loom_protocol::todo::{Assignment, NonEmptyVec, TodoSpecOutcome, TodoSuccess};

const REQUIREMENT: &str = "Persist pending work";
const BRIEF: &str = "## Options — Select implementation\n\n### Option 1 — Keep\nCost: debt.\n\n### Option 2 — Change\nCost: churn.\n";

struct Fixture {
    dir: tempfile::TempDir,
    base: String,
    head: String,
    runner: StatefulRunner,
    state: Arc<CacheDb>,
    controller: ProductionTodoController<StatefulRunner>,
    success: TodoSuccess,
}

impl Fixture {
    async fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let (base, _) = init_two_changed_workspace(dir.path())?;
        for label in ["alpha", "beta"] {
            std::fs::create_dir_all(dir.path().join(format!("specs/{label}")))?;
            std::fs::write(
                dir.path().join(format!("specs/{label}/spec.md")),
                format!("# {label}\n"),
            )?;
            std::fs::write(
                dir.path().join(format!("specs/{label}/tests.md")),
                format!("## Success Criteria\n\n- {REQUIREMENT} [test?](pending_verifier)\n"),
            )?;
            std::fs::remove_file(dir.path().join(format!("specs/{label}.md")))?;
        }
        std::fs::write(
            dir.path().join("docs/README.md"),
            "- [Alpha](../specs/alpha/spec.md)\n- [Beta](../specs/beta/spec.md)\n",
        )?;
        run_git(dir.path(), &["add", "specs", "docs/README.md"])?;
        run_git(
            dir.path(),
            &["commit", "-q", "-m", "add acceptance packages"],
        )?;
        let head = git_output(dir.path(), &["rev-parse", "HEAD"])?;
        let runner = StatefulRunner::new(&base, None);
        let state = Arc::new(CacheDb::open(dir.path().join(".loom/cache.db"))?);
        state.notes_add(
            &SpecLabel::new("alpha")?,
            "implementation",
            "keep until finalized",
            1,
        )?;
        let mut controller = ProductionTodoController::for_workspace(
            dir.path().to_path_buf(),
            Arc::clone(&state),
            manifest(dir.path())?,
            ProfileName::new("base")?,
            Arc::new(GitClient::open(dir.path())?),
            Arc::new(BdClient::with_runner(runner.clone())),
        );
        let session = controller.build_session().await?;
        let mut success = todo_success(&session.config.initial_prompt, &["alpha", "beta"])?;
        for (row, task) in success
            .specs
            .as_slice()
            .iter()
            .zip(["lm-task.1", "lm-task.2"])
        {
            insert_task(&runner, task, "lm-work")?;
            assert!(row.label.as_str() == "alpha" || row.label.as_str() == "beta");
        }
        let mut rows = success.specs.clone().into_vec();
        for (row, task) in rows.iter_mut().zip(["lm-task.1", "lm-task.2"]) {
            row.outcome = TodoSpecOutcome::Decomposed {
                beads: NonEmptyVec::new(vec![BeadId::new(task)?])?,
                assignments: NonEmptyVec::new(vec![Assignment {
                    task: BeadId::new(task)?,
                    criteria: references(row.label.as_str())?,
                }])?,
            };
        }
        success.specs = NonEmptyVec::new(rows)?;
        drop(session);
        Ok(Self {
            dir,
            base,
            head,
            runner,
            state,
            controller,
            success,
        })
    }

    async fn finalize(&mut self) -> Result<(), TodoError> {
        self.controller
            .record_outcome(
                &SessionOutcome {
                    exit_code: 0,
                    cost_usd: None,
                },
                None,
                Some(&self.success),
            )
            .await
            .map(|_| ())
    }

    fn assert_pending(&self) -> Result<()> {
        for id in ["lm-alpha", "lm-beta"] {
            assert_eq!(
                self.runner.bead(id)?.metadata["loom.todo_cursor"],
                self.base
            );
        }
        let pending = self.runner.bead("lm-work")?;
        assert!(pending.labels.iter().any(|label| label == "loom:todo"));
        assert!(!pending.labels.iter().any(|label| label == "loom:active"));
        assert!(
            self.runner
                .bead("lm-oldactive")?
                .labels
                .iter()
                .any(|label| label == "loom:active")
        );
        assert_eq!(
            self.state
                .notes_list(Some(&SpecLabel::new("alpha")?), Some("implementation"))?[0]
                .text,
            "keep until finalized"
        );
        for task in ["lm-task.1", "lm-task.2"] {
            assert_eq!(
                self.runner.bead(task)?.notes.as_deref(),
                Some("original note")
            );
        }
        Ok(())
    }
}

fn references(label: &str) -> Result<References> {
    let label = SpecLabel::new(label)?;
    Ok(References::new(vec![Reference::new(
        label.clone(),
        CriterionId::for_spec_text(&label, REQUIREMENT),
    )])?)
}

fn insert_task(runner: &StatefulRunner, task: &str, parent: &str) -> Result<()> {
    runner
        .state
        .lock()
        .map_err(|_| anyhow!("lock"))?
        .beads
        .insert(
            task.into(),
            FakeBead {
                id: task.into(),
                title: "Task".into(),
                status: "open".into(),
                issue_type: "task".into(),
                labels: vec![],
                parent: Some(parent.into()),
                metadata: BTreeMap::from([(
                    "unrelated".into(),
                    serde_json::json!({"nested": [1, true]}),
                )]),
                description: Some("Prose is not acceptance metadata".into()),
                notes: Some("original note".into()),
            },
        );
    Ok(())
}

fn mutate_assignment(success: &mut TodoSuccess, f: impl FnOnce(&mut Assignment)) {
    let mut rows = success.specs.clone().into_vec();
    if let TodoSpecOutcome::Decomposed { assignments, .. } = &mut rows[0].outcome {
        let mut values = assignments.clone().into_vec();
        f(&mut values[0]);
        *assignments = NonEmptyVec::new(values).unwrap();
    }
    success.specs = NonEmptyVec::new(rows).unwrap();
}

#[tokio::test]
async fn todo_acceptance_persistence_resolves_pending_refs_and_preserves_metadata() -> Result<()> {
    let mut fixture = Fixture::new().await?;
    fixture.finalize().await?;
    for (task, label) in [("lm-task.1", "alpha"), ("lm-task.2", "beta")] {
        let bead = fixture.runner.bead(task)?;
        assert_eq!(
            bead.metadata["unrelated"],
            serde_json::json!({"nested": [1, true]})
        );
        let candidates = Candidates::from_metadata(&bead.metadata[METADATA_KEY])?;
        assert_eq!(
            candidates,
            Candidates::Implementation {
                criteria: references(label)?
            }
        );
        let Candidates::Implementation { criteria } = candidates else {
            return Err(anyhow!("wrong purpose"));
        };
        let resolved = loom_gate::acceptance::load(fixture.dir.path())?.resolve(&criteria)?;
        assert!(resolved.obligations()[0].annotation().pending);
        assert_eq!(
            fixture.runner.bead(&format!("lm-{label}"))?.metadata["loom.todo_cursor"],
            fixture.head
        );
    }
    assert!(
        fixture
            .runner
            .bead("lm-work")?
            .labels
            .iter()
            .any(|label| label == "loom:active")
    );
    assert_eq!(
        fixture
            .state
            .notes_list(Some(&SpecLabel::new("alpha")?), Some("implementation"))?,
        [] as [loom_driver::state::NoteRow; 0]
    );
    Ok(())
}

#[tokio::test]
async fn todo_acceptance_persistence_requires_fresh_dispatch_resolution() -> Result<()> {
    let mut fixture = Fixture::new().await?;
    fixture.finalize().await?;
    let bead = fixture.runner.bead("lm-task.1")?;
    let Candidates::Implementation { criteria } =
        Candidates::from_metadata(&bead.metadata[METADATA_KEY])?
    else {
        return Err(anyhow!("wrong purpose"));
    };
    std::fs::write(
        fixture.dir.path().join("specs/alpha/tests.md"),
        format!("## Success Criteria\n\n- {REQUIREMENT} [test](current_verifier)\n"),
    )?;
    let resolved = loom_gate::acceptance::load(fixture.dir.path())?.resolve(&criteria)?;
    assert_eq!(
        resolved.obligations()[0].annotation().target.as_str(),
        "current_verifier"
    );
    std::fs::write(
        fixture.dir.path().join("specs/alpha/tests.md"),
        "## Success Criteria\n\n- Changed requirement [test](current_verifier)\n",
    )?;
    assert!(
        loom_gate::acceptance::load(fixture.dir.path())?
            .resolve(&criteria)
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn todo_acceptance_persistence_uses_captured_preflight_not_later_edits() -> Result<()> {
    let mut fixture = Fixture::new().await?;
    std::fs::write(
        fixture.dir.path().join("specs/alpha/tests.md"),
        "## Success Criteria\n\n- Later requirement [test](later_verifier)\n",
    )?;
    mutate_assignment(&mut fixture.success, |assignment| {
        assignment.criteria = References::new(vec![Reference::new(
            SpecLabel::new("alpha").unwrap(),
            CriterionId::for_spec_text(&SpecLabel::new("alpha").unwrap(), "Later requirement"),
        )])
        .unwrap();
    });
    assert!(matches!(
        fixture.finalize().await,
        Err(TodoError::AcceptanceResolution(_))
    ));
    fixture.assert_pending()
}

#[tokio::test]
async fn todo_acceptance_persistence_invalid_refs_do_not_activate_or_consume_notes() -> Result<()> {
    for label in ["unknown", "beta"] {
        let mut fixture = Fixture::new().await?;
        mutate_assignment(&mut fixture.success, |assignment| {
            assignment.criteria =
                References::parse([(label, "criterion-0000000000000000")]).unwrap();
        });
        assert!(matches!(
            fixture.finalize().await,
            Err(TodoError::AcceptanceResolution(_))
        ));
        fixture.assert_pending()?;
        assert!(
            !fixture
                .runner
                .bead("lm-task.1")?
                .metadata
                .contains_key(METADATA_KEY)
        );
    }
    Ok(())
}

#[tokio::test]
async fn todo_acceptance_persistence_rejects_out_of_roster_assignments() -> Result<()> {
    let mut fixture = Fixture::new().await?;
    insert_task(&fixture.runner, "lm-foreign", "lm-other")?;
    mutate_assignment(&mut fixture.success, |assignment| {
        assignment.task = BeadId::new("lm-foreign").unwrap();
    });
    assert!(matches!(
        fixture.finalize().await,
        Err(TodoError::TodoValidation { .. })
    ));
    fixture.assert_pending()
}

#[tokio::test]
async fn todo_acceptance_persistence_compensates_bindings_before_later_state() -> Result<()> {
    for failure in 1..=7 {
        let mut fixture = Fixture::new().await?;
        let previous = serde_json::to_string(&Candidates::Implementation {
            criteria: references("beta")?,
        })?;
        {
            let mut state = fixture.runner.state.lock().map_err(|_| anyhow!("lock"))?;
            state
                .beads
                .get_mut("lm-task.1")
                .unwrap()
                .metadata
                .insert(METADATA_KEY.into(), serde_json::json!(previous));
            state.fail_update = Some(state.update_attempt + failure);
        }
        assert!(matches!(fixture.finalize().await, Err(TodoError::Bd(_))));
        fixture.assert_pending()?;
        assert_eq!(
            fixture.runner.bead("lm-task.1")?.metadata[METADATA_KEY],
            previous
        );
        assert!(
            !fixture
                .runner
                .bead("lm-task.2")?
                .metadata
                .contains_key(METADATA_KEY)
        );
        assert_eq!(
            fixture.runner.bead("lm-task.1")?.metadata["unrelated"],
            serde_json::json!({"nested": [1, true]})
        );
    }
    Ok(())
}

#[tokio::test]
async fn todo_acceptance_persistence_compensates_a_write_that_reports_failure() -> Result<()> {
    let mut fixture = Fixture::new().await?;
    {
        let mut state = fixture.runner.state.lock().map_err(|_| anyhow!("lock"))?;
        state.fail_update = Some(state.update_attempt + 2);
        state.fail_update_after_write = true;
    }
    assert!(matches!(fixture.finalize().await, Err(TodoError::Bd(_))));
    fixture.assert_pending()?;
    for task in ["lm-task.1", "lm-task.2"] {
        assert!(
            !fixture
                .runner
                .bead(task)?
                .metadata
                .contains_key(METADATA_KEY)
        );
    }
    Ok(())
}

#[tokio::test]
async fn todo_acceptance_persistence_rejects_nonstring_previous_metadata() -> Result<()> {
    let mut fixture = Fixture::new().await?;
    let invalid = serde_json::json!({"purpose":"implementation", "criteria":[]});
    fixture
        .runner
        .state
        .lock()
        .map_err(|_| anyhow!("lock"))?
        .beads
        .get_mut("lm-task.1")
        .unwrap()
        .metadata
        .insert(METADATA_KEY.into(), invalid.clone());
    assert!(matches!(
        fixture.finalize().await,
        Err(TodoError::TodoValidation { .. })
    ));
    fixture.assert_pending()?;
    assert_eq!(
        fixture.runner.bead("lm-task.1")?.metadata[METADATA_KEY],
        invalid
    );
    Ok(())
}

#[tokio::test]
async fn todo_acceptance_persistence_cache_failure_compensates_bindings() -> Result<()> {
    let mut fixture = Fixture::new().await?;
    fixture.state.inject_todo_finalization_failure(1)?;
    assert!(matches!(fixture.finalize().await, Err(TodoError::State(_))));
    fixture.assert_pending()?;
    for task in ["lm-task.1", "lm-task.2"] {
        assert!(
            !fixture
                .runner
                .bead(task)?
                .metadata
                .contains_key(METADATA_KEY)
        );
    }
    Ok(())
}

#[tokio::test]
async fn todo_acceptance_persistence_labels_and_options_cannot_hide_unaccounted_drafts()
-> Result<()> {
    for (status, labels, description) in [
        ("open", vec!["loom:clarify"], BRIEF),
        ("blocked", vec![], BRIEF),
        ("blocked", vec!["loom:clarify"], BRIEF),
    ] {
        let mut fixture = Fixture::new().await?;
        insert_task(&fixture.runner, "lm-draft", "lm-work")?;
        {
            let mut state = fixture.runner.state.lock().map_err(|_| anyhow!("lock"))?;
            let bead = state.beads.get_mut("lm-draft").unwrap();
            bead.status = status.into();
            bead.labels = labels.into_iter().map(ToOwned::to_owned).collect();
            bead.description = Some(description.into());
            drop(state);
        }
        assert!(matches!(
            fixture.finalize().await,
            Err(TodoError::TodoValidation { .. })
        ));
        fixture.assert_pending()?;
    }
    Ok(())
}

#[tokio::test]
async fn todo_finalization_distinguishes_decisions_from_implementation_roster() -> Result<()> {
    let mut fixture = Fixture::new().await?;
    for task in ["lm-decision", "lm-repair"] {
        insert_task(&fixture.runner, task, "lm-work")?;
        let mut state = fixture.runner.state.lock().map_err(|_| anyhow!("lock"))?;
        let bead = state.beads.get_mut(task).unwrap();
        bead.status = "blocked".into();
        if task == "lm-decision" {
            bead.labels = vec!["loom:clarify".into()];
            bead.description = Some(BRIEF.into());
        } else {
            bead.labels = vec!["loom:blocked".into()];
            bead.notes = Some("clarify-without-options: repair brief".into());
        }
        drop(state);
    }
    fixture
        .controller
        .set_admitted_decisions([BeadId::new("lm-decision")?, BeadId::new("lm-repair")?]);
    fixture.finalize().await?;
    for task in ["lm-decision", "lm-repair"] {
        assert!(
            !fixture
                .runner
                .bead(task)?
                .metadata
                .contains_key(METADATA_KEY)
        );
    }
    Ok(())
}

#[tokio::test]
async fn todo_acceptance_persistence_rejects_admitted_decisions_as_assigned_tasks() -> Result<()> {
    let mut fixture = Fixture::new().await?;
    fixture
        .controller
        .set_admitted_decisions([BeadId::new("lm-task.1")?]);
    assert!(matches!(
        fixture.finalize().await,
        Err(TodoError::TodoValidation { .. })
    ));
    fixture.assert_pending()?;
    assert!(
        !fixture
            .runner
            .bead("lm-task.1")?
            .metadata
            .contains_key(METADATA_KEY)
    );
    Ok(())
}

#[tokio::test]
async fn todo_acceptance_persistence_keeps_resolved_decisions_out_of_implementation() -> Result<()>
{
    let mut fixture = Fixture::new().await?;
    insert_task(&fixture.runner, "lm-decision", "lm-work")?;
    fixture
        .runner
        .state
        .lock()
        .map_err(|_| anyhow!("lock"))?
        .beads
        .get_mut("lm-decision")
        .unwrap()
        .status = "closed".into();
    fixture
        .controller
        .set_admitted_decisions([BeadId::new("lm-decision")?]);
    fixture.finalize().await?;
    assert!(
        !fixture
            .runner
            .bead("lm-decision")?
            .metadata
            .contains_key(METADATA_KEY)
    );
    Ok(())
}

#[tokio::test]
async fn todo_acceptance_persistence_rejects_duplicate_or_missing_assignments() -> Result<()> {
    for duplicate in [false, true] {
        let mut fixture = Fixture::new().await?;
        let mut rows = fixture.success.specs.clone().into_vec();
        if let TodoSpecOutcome::Decomposed { beads, assignments } = &mut rows[0].outcome {
            if duplicate {
                let assignment = assignments[0].clone();
                *assignments = NonEmptyVec::new(vec![assignment.clone(), assignment])?;
            } else {
                insert_task(&fixture.runner, "lm-task.3", "lm-work")?;
                *beads =
                    NonEmptyVec::new(vec![BeadId::new("lm-task.1")?, BeadId::new("lm-task.3")?])?;
            }
        }
        fixture.success.specs = NonEmptyVec::new(rows)?;
        assert!(matches!(
            fixture.finalize().await,
            Err(TodoError::TodoValidation { .. })
        ));
        fixture.assert_pending()?;
    }
    Ok(())
}

#[tokio::test]
async fn todo_acceptance_persistence_preserves_checkpoint_notes_on_invalid_proposal() -> Result<()>
{
    let mut fixture = Fixture::new().await?;
    fixture
        .runner
        .state
        .lock()
        .map_err(|_| anyhow!("lock"))?
        .beads
        .get_mut("lm-work")
        .unwrap()
        .notes = Some("keep checkpoint".into());
    mutate_assignment(&mut fixture.success, |assignment| {
        assignment.criteria = References::parse([("alpha", "criterion-0000000000000000")]).unwrap();
    });
    assert!(fixture.finalize().await.is_err());
    assert!(
        fixture
            .runner
            .bead("lm-work")?
            .notes
            .unwrap()
            .starts_with("keep checkpoint\n")
    );
    fixture.assert_pending()
}

#[tokio::test]
async fn todo_acceptance_persistence_no_work_cannot_ignore_implementation_children() -> Result<()> {
    let mut fixture = Fixture::new().await?;
    let mut rows = fixture.success.specs.clone().into_vec();
    rows[0].outcome = TodoSpecOutcome::NoWork {
        reason: loom_protocol::todo::NonEmptyString::new("looks covered")?,
    };
    fixture.success.specs = NonEmptyVec::new(rows)?;
    assert!(matches!(
        fixture.finalize().await,
        Err(TodoError::TodoValidation { .. })
    ));
    fixture.assert_pending()
}

#[tokio::test]
async fn todo_acceptance_persistence_merges_shared_task_assignments() -> Result<()> {
    let mut fixture = Fixture::new().await?;
    fixture
        .runner
        .state
        .lock()
        .map_err(|_| anyhow!("lock"))?
        .beads
        .remove("lm-task.2");
    let mut rows = fixture.success.specs.clone().into_vec();
    if let TodoSpecOutcome::Decomposed { beads, assignments } = &mut rows[1].outcome {
        *beads = NonEmptyVec::new(vec![BeadId::new("lm-task.1")?])?;
        *assignments = NonEmptyVec::new(vec![Assignment {
            task: BeadId::new("lm-task.1")?,
            criteria: references("beta")?,
        }])?;
    }
    fixture.success.specs = NonEmptyVec::new(rows)?;
    fixture.finalize().await?;
    let bead = fixture.runner.bead("lm-task.1")?;
    let Candidates::Implementation { criteria } =
        Candidates::from_metadata(&bead.metadata[METADATA_KEY])?
    else {
        return Err(anyhow!("wrong purpose"));
    };
    assert_eq!(criteria.as_slice().len(), 2);
    Ok(())
}

#[tokio::test]
async fn stateful_bd_fixture_models_failure_after_metadata_write() -> Result<()> {
    let runner = StatefulRunner::new("base", None);
    insert_task(&runner, "lm-child", "lm-work")?;
    {
        let mut state = runner.state.lock().map_err(|_| anyhow!("lock"))?;
        state.fail_update = Some(1);
        state.fail_update_after_write = true;
    }
    let client = BdClient::with_runner(runner);
    let id = BeadId::new("lm-child")?;
    let candidates = Candidates::Implementation {
        criteria: references("alpha")?,
    };
    let result = client
        .update(
            &id,
            loom_driver::bd::UpdateOpts {
                set_metadata: vec![(METADATA_KEY.into(), serde_json::to_string(&candidates)?)],
                ..Default::default()
            },
        )
        .await;
    assert!(result.is_err());
    assert_eq!(
        Candidates::from_metadata(&client.show(&id).await?.metadata[METADATA_KEY])?,
        candidates
    );
    Ok(())
}

#[tokio::test]
async fn stateful_bd_fixture_conforms_to_parent_and_metadata_boundary() -> Result<()> {
    let runner = StatefulRunner::new("base", None);
    insert_task(&runner, "lm-child", "lm-work")?;
    insert_task(&runner, "lm-foreign", "lm-other")?;
    let client = BdClient::with_runner(runner);
    let children = client
        .list(loom_driver::bd::ListOpts {
            all: true,
            limit: Some(0),
            parent: Some(BeadId::new("lm-work")?),
            ..Default::default()
        })
        .await?;
    assert_eq!(children.len(), 1);
    let candidates = Candidates::Implementation {
        criteria: references("alpha")?,
    };
    client
        .update(
            &children[0].id,
            loom_driver::bd::UpdateOpts {
                set_metadata: vec![(METADATA_KEY.into(), serde_json::to_string(&candidates)?)],
                ..Default::default()
            },
        )
        .await?;
    let updated = client.show(&children[0].id).await?;
    assert_eq!(
        Candidates::from_metadata(&updated.metadata[METADATA_KEY])?,
        candidates
    );
    assert_eq!(
        updated.metadata["unrelated"],
        serde_json::json!({"nested": [1, true]})
    );
    Ok(())
}

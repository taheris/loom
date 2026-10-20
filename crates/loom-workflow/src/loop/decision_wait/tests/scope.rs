use super::*;

#[cfg(test)]
#[tokio::test]
async fn foreign_or_wrong_type_references_reject_whole_context_before_effects() {
    let f = Fixture::new().await;
    let foreign =
        f.bd.create(CreateOpts {
            title: "foreign decision".into(),
            issue_type: Some(IssueType::Decision),
            parent: Some(f.sibling.clone()),
            description: BRIEF.into(),
            ..CreateOpts::default()
        })
        .await
        .unwrap();
    for extra in [f.sibling.clone(), foreign.clone()] {
        let mut ids = f.decisions.clone();
        ids.push(extra);
        let outcome =
            reconcile_session(&f.bd, &f.journal(), &f.source, Fixture::session(&ids), None)
                .await
                .unwrap();
        assert!(matches!(outcome, AgentOutcome::Failure { .. }));
        assert!(f.journal().load(&f.source.id).unwrap().is_none());
        for id in &f.decisions {
            assert_eq!(f.bd.show(id).await.unwrap().status, Status::Open);
        }
    }
    assert_eq!(f.bd.show(&foreign).await.unwrap().status, Status::Open);
}

#[cfg(test)]
#[tokio::test]
async fn current_graph_releases_wait_but_orphaned_projection_blocks_publication() {
    let f = Fixture::new().await;
    assert!(matches!(f.admit().await, AgentOutcome::Waiting { .. }));
    for id in &f.decisions {
        f.bd.close(id, Some("human answer")).await.unwrap();
    }
    f.bd.close(&f.source.id, Some("human closed implementation"))
        .await
        .unwrap();
    f.bd.close(&f.sibling, Some("independent work complete"))
        .await
        .unwrap();
    f.bd.close(&f.root, Some("human closed root"))
        .await
        .unwrap();
    let mut git = loom_driver::git::GitClient::open(f.dir.path()).unwrap();
    let missing_hooks = f.dir.path().join("intentionally-missing-prek-hooks");
    git.set_prek_hooks_path_override(missing_hooks.clone());
    let molecule = f.root.as_str().parse().unwrap();
    let never = std::path::Path::new("/must-not-launch-verification-with-a-pending-hold");
    let handoff = super::super::super::execute_molecule_push_gate(
        &f.bd,
        &SpecLabel::new("loop").unwrap(),
        Some(&molecule),
        super::super::super::MoleculePushGateCommands::new(None, never, never),
        f.dir.path(),
        &git,
    )
    .await;
    assert!(matches!(
        handoff,
        Err(LoopError::Git(
            loom_driver::git::GitError::PrekHooksMissing { .. }
        ))
    ));
    assert_eq!(
        f.journal().load(&f.source.id).unwrap().unwrap().phase,
        Phase::InventoryAdmitted
    );
    // Only an actual orphaned projection, not historical waiting, fences publication.
    f.bd.update(
        &f.source.id,
        UpdateOpts {
            set_metadata: vec![(HOLD_KEY.into(), "{}".into())],
            ..UpdateOpts::default()
        },
    )
    .await
    .unwrap();
    let mut entry = f.journal().load(&f.source.id).unwrap().unwrap();
    entry.phase = Phase::Released;
    f.journal().save(&entry).unwrap();
    let orphaned_projection = super::super::super::execute_molecule_push_gate(
        &f.bd,
        &SpecLabel::new("loop").unwrap(),
        Some(&molecule),
        super::super::super::MoleculePushGateCommands::new(None, never, never),
        f.dir.path(),
        &git,
    )
    .await
    .unwrap();
    assert_eq!(
        orphaned_projection.evidence.molecule_state,
        loom_gate::MoleculeState::Unresolved
    );
    f.bd.update(
        &f.source.id,
        UpdateOpts {
            if_status: Some(Status::Closed),
            remove_labels: vec![HOLD_LABEL.into()],
            unset_metadata: vec![HOLD_KEY.into()],
            ..UpdateOpts::default()
        },
    )
    .await
    .unwrap();
    // Native wait attribution is separate from the retired legacy hold. Only
    // current-state reconciliation can acknowledge the human-closed source.
    f.journal()
        .refresh_native_waits(&f.bd, Some(&f.root), None)
        .await
        .unwrap();
    let control = super::super::super::execute_molecule_push_gate(
        &f.bd,
        &SpecLabel::new("loop").unwrap(),
        Some(&molecule),
        super::super::super::MoleculePushGateCommands::new(None, never, never),
        f.dir.path(),
        &git,
    )
    .await;
    match control {
        Err(LoopError::Git(loom_driver::git::GitError::PrekHooksMissing { path })) => {
            assert_eq!(path, missing_hooks);
        }
        Err(error) => panic!("unexpected downstream guard: {error:?}"),
        Ok(handoff) => panic!(
            "unexpected downstream state: {:?}",
            handoff.evidence.molecule_state
        ),
    }
}

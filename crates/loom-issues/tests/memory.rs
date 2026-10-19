#[cfg(test)]
mod common;

use loom_issues::{
    memory::{Backend, Fault, Operation, Timing},
    task::{Create, Guard, Query, Update},
    tracking::{Error, Persistence, Store, Synchronization},
};

#[tokio::test]
async fn issues_shared_contract_round_trips_operations() {
    common::round_trip(&Backend::default()).await;
}

#[tokio::test]
async fn lightweight_tracking_fixtures_need_no_native_database() {
    let retained = Backend::default();
    let worker = retained.clone();
    let id = worker
        .create(Create {
            title: "survives worker restart".into(),
            ..Create::default()
        })
        .await
        .unwrap();
    drop(worker);
    let restarted = retained.clone();
    assert_eq!(
        restarted.get(&id).await.unwrap().title,
        "survives worker restart"
    );
    assert_eq!(
        restarted.guarantees().persistence,
        Persistence::ProcessLifetime
    );
    assert_eq!(
        Backend::default().query(Query::default()).await.unwrap(),
        vec![]
    );
    common::round_trip(&restarted).await;
}

#[tokio::test]
async fn tracking_conformance_covers_lightweight_and_pinned_beads() {
    let store = Backend::default();
    common::stale_writers(&store, &store.clone()).await;
    common::graph_guards(&store).await;
    common::inherited_readiness(&store).await;
    common::interruptions(&store, |fault| store.inject(fault).unwrap()).await;
}

#[tokio::test]
async fn interrupted_creation_is_reconciled_after_handle_replacement() {
    let retained = Backend::default();
    let worker = retained.clone();
    worker
        .inject(Fault {
            operation: Operation::Create,
            timing: Timing::AfterWrite,
            skip: 0,
        })
        .unwrap();
    assert!(matches!(
        worker
            .create(Create {
                title: "uncertain creation".into(),
                ..Create::default()
            })
            .await,
        Err(Error::Uncertain { .. })
    ));
    drop(worker);
    let restarted = retained.clone();
    let records = restarted.query(Query::default()).await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].title, "uncertain creation");
}

#[tokio::test]
async fn synchronization_faults_do_not_discard_shared_state() {
    let store = Backend::default();
    let id = common::create(&store, "retained", None).await;
    for timing in [Timing::BeforeWrite, Timing::AfterWrite] {
        store
            .inject(Fault {
                operation: Operation::Synchronize,
                timing,
                skip: 0,
            })
            .unwrap();
        let result = store.synchronize(Synchronization::Publish).await;
        assert!(matches!(
            result,
            Err(Error::Transport { .. } | Error::Uncertain { .. })
        ));
        assert_eq!(store.get(&id).await.unwrap().title, "retained");
    }
    store.synchronize(Synchronization::Publish).await.unwrap();
}

#[tokio::test]
async fn memory_metadata_supports_json_without_native_coercion() {
    let store = Backend::default();
    let id = common::create(&store, "JSON", None).await;
    let metadata = std::collections::BTreeMap::from([(
        "structured".into(),
        serde_json::json!({"array":[null,"TRUE",9_007_199_254_740_993_u64]}),
    )]);
    store
        .update(
            &id,
            Update {
                set_metadata: metadata.clone(),
                ..Update::default()
            },
            Guard::None,
        )
        .await
        .unwrap();
    assert_eq!(store.get(&id).await.unwrap().metadata, metadata);
}

#[tokio::test]
async fn relationship_faults_preserve_before_and_after_write_boundaries() {
    let store = Backend::default();
    let source = common::create(&store, "source", None).await;
    let target = common::create(&store, "target", None).await;
    for timing in [Timing::BeforeWrite, Timing::AfterWrite] {
        store
            .inject(Fault {
                operation: Operation::Relate,
                timing,
                skip: 0,
            })
            .unwrap();
        assert_eq!(store.get(&source).await.unwrap().title, "source");
        let result = store
            .relate(&source, &target, loom_issues::task::Relation::Blocks)
            .await;
        match timing {
            Timing::BeforeWrite => {
                assert!(matches!(result, Err(Error::Transport { .. })));
                assert_eq!(
                    store.graph(&source).await.unwrap().active_blockers(),
                    vec![]
                );
            }
            Timing::AfterWrite => {
                assert!(matches!(result, Err(Error::Uncertain { .. })));
                assert_eq!(
                    store.graph(&source).await.unwrap().active_blockers(),
                    vec![target.clone()]
                );
            }
        }
    }
}

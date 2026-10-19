use loom_issues::{
    memory::{Fault, Operation, Timing},
    task::{
        self, Create, DependencyKind, Guard, Id, Kind, Label, Priority, Query, Ready, Relation,
        Status, Update,
    },
    tracking::{Error, Store, Synchronization},
};
use serde_json::json;
use std::collections::BTreeMap;

pub fn label(value: &str) -> Label {
    Label::new(value).unwrap()
}

pub async fn create(store: &impl Store, title: &str, parent: Option<Id>) -> Id {
    store
        .create(Create {
            title: title.into(),
            parent,
            ..Create::default()
        })
        .await
        .unwrap()
}

pub async fn coherent_observation(store: &impl Store) {
    let source = create(store, "observed source", None).await;
    let blocker = create(store, "observed blocker", None).await;
    store
        .relate(&source, &blocker, Relation::Blocks)
        .await
        .unwrap();
    store
        .update(
            &source,
            Update {
                status: Some(Status::Blocked),
                add_labels: vec![label("observed")],
                set_metadata: BTreeMap::from([("observation".into(), json!("before"))]),
                ..Update::default()
            },
            Guard::None,
        )
        .await
        .unwrap();
    let before = store.observe(&source).await.unwrap();
    assert_eq!(before.record.id, source);
    assert_eq!(before.record.status, Status::Blocked);
    assert_eq!(before.graph.status, before.record.status);
    assert!(before.record.labels.contains(&label("observed")));
    assert_eq!(before.record.metadata["observation"], json!("before"));
    assert_eq!(
        before.graph.dependencies,
        vec![task::Dependency {
            id: blocker.clone(),
            status: Status::Open,
            kind: DependencyKind::Blocks
        }]
    );
    store.close(&blocker, None).await.unwrap();
    store
        .update(
            &source,
            Update {
                status: Some(Status::Open),
                set_metadata: BTreeMap::from([("observation".into(), json!("after"))]),
                ..Update::default()
            },
            Guard::None,
        )
        .await
        .unwrap();
    let after = store.observe(&source).await.unwrap();
    assert_eq!(after.record.id, source);
    assert_eq!(after.record.status, Status::Open);
    assert_eq!(after.graph.status, after.record.status);
    assert_eq!(after.record.metadata["observation"], json!("after"));
    assert_eq!(after.graph.dependencies[0].status, Status::Closed);
    assert_eq!(before.graph.dependencies[0].status, Status::Open);
}

pub async fn round_trip(store: &impl Store) {
    let root = store
        .create(Create {
            title: "group".into(),
            kind: Some(Kind::Epic),
            ..Create::default()
        })
        .await
        .unwrap();
    let id = store
        .create(Create {
            title: "initial".into(),
            description: "body\nwith newline".into(),
            kind: Some(Kind::Bug),
            priority: Some(Priority::P1),
            parent: Some(root.clone()),
            labels: vec![label("conformance"), label("remove")],
            metadata: BTreeMap::from([
                ("keep".into(), json!({"nested":[1,true]})),
                ("drop".into(), json!("value")),
            ]),
            notes: Some("human notes".into()),
            ..Create::default()
        })
        .await
        .unwrap();
    let before = store.get(&id).await.unwrap();
    assert_eq!(before.id, id);
    assert_eq!(before.parent, Some(root.clone()));
    assert_eq!(before.description, "body\nwith newline");
    assert_eq!(before.issue_type, Kind::Bug);
    assert_eq!(before.priority, Priority::P1);
    assert_eq!(before.notes.as_deref(), Some("human notes"));
    store
        .update(
            &id,
            Update {
                title: Some("revised".into()),
                description: Some("updated body".into()),
                status: Some(Status::Blocked),
                priority: Some(Priority::P3),
                add_labels: vec![label("added")],
                remove_labels: vec![label("remove")],
                set_metadata: BTreeMap::from([("changed".into(), json!(true))]),
                unset_metadata: vec!["drop".into()],
                ..Update::default()
            },
            Guard::None,
        )
        .await
        .unwrap();
    let after = store.get(&id).await.unwrap();
    assert_eq!(after.title, "revised");
    assert_eq!(after.description, "updated body");
    assert_eq!(after.status, Status::Blocked);
    assert_eq!(after.priority, Priority::P3);
    assert_eq!(after.parent, before.parent);
    assert_eq!(after.issue_type, before.issue_type);
    assert_eq!(after.notes, before.notes);
    assert_eq!(after.metadata["keep"], before.metadata["keep"]);
    assert_eq!(after.metadata["changed"], json!(true));
    assert!(!after.metadata.contains_key("drop"));
    assert!(after.labels.contains(&label("added")));
    assert!(!after.labels.contains(&label("remove")));
    let found = store
        .query(Query {
            statuses: vec![Status::Blocked],
            label: Some(label("conformance")),
            label_any: vec![label("added")],
            parent: Some(root.clone()),
            kind: Some(Kind::Bug),
            limit: Some(0),
        })
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, id);
    assert_eq!(
        store
            .query(Query {
                label: Some(label("absent")),
                ..Query::default()
            })
            .await
            .unwrap(),
        vec![]
    );
    store
        .update(
            &id,
            Update {
                append_notes: Some("appended".into()),
                ..Update::default()
            },
            Guard::None,
        )
        .await
        .unwrap();
    assert_eq!(
        store.get(&id).await.unwrap().notes.as_deref(),
        Some("human notes\nappended")
    );
    let elsewhere = create(store, "elsewhere", None).await;
    store.parent(&id, &elsewhere).await.unwrap();
    assert_eq!(
        store.get(&id).await.unwrap().parent,
        Some(elsewhere.clone())
    );
    assert!(
        !store
            .graph(&id)
            .await
            .unwrap()
            .dependencies
            .iter()
            .any(|edge| edge.id == root)
    );
    store.parent(&id, &root).await.unwrap();
    let blocker = create(store, "blocker", Some(root.clone())).await;
    store.relate(&id, &blocker, Relation::Blocks).await.unwrap();
    store
        .relate(&id, &elsewhere, Relation::DiscoveredFrom)
        .await
        .unwrap();
    let graph = store.graph(&id).await.unwrap();
    assert_eq!(graph.status, Status::Blocked);
    assert_eq!(graph.active_blockers(), vec![blocker.clone()]);
    assert!(
        graph
            .dependencies
            .iter()
            .any(|edge| edge.id == root && edge.kind == DependencyKind::ParentChild)
    );
    assert!(
        graph
            .dependencies
            .iter()
            .any(|edge| edge.id == elsewhere && edge.kind == DependencyKind::DiscoveredFrom)
    );
    assert!(store.dependents(&blocker).await.unwrap().contains(&id));
    assert!(store.close(&id, Some("blocked")).await.is_err());
    store
        .update(
            &id,
            Update {
                status: Some(Status::Open),
                ..Update::default()
            },
            Guard::None,
        )
        .await
        .unwrap();
    assert!(
        !store
            .ready(Ready {
                parent: Some(root.clone()),
                ..Ready::default()
            })
            .await
            .unwrap()
            .iter()
            .any(|row| row.id == id)
    );
    store.close(&blocker, Some("done")).await.unwrap();
    let ready = store
        .ready(Ready {
            parent: Some(root.clone()),
            label: Some(label("conformance")),
            ..Ready::default()
        })
        .await
        .unwrap();
    assert_eq!(
        ready.iter().map(|row| &row.id).collect::<Vec<_>>(),
        vec![&id]
    );
    assert_eq!(
        store
            .ready(Ready {
                parent: Some(root.clone()),
                exclude_labels: vec![label("added")],
                ..Ready::default()
            })
            .await
            .unwrap(),
        vec![]
    );
    store
        .update(
            &id,
            Update {
                status: Some(Status::InProgress),
                ..Update::default()
            },
            Guard::None,
        )
        .await
        .unwrap();
    let group = task::GroupId::new(root.as_str()).unwrap();
    let progress = store.progress(&group).await.unwrap();
    assert_eq!(
        (progress.completed, progress.in_progress, progress.total),
        (1, 1, 2)
    );
    assert_eq!(progress.current_step_id, Some(id.clone()));
    store.close(&id, Some("finished")).await.unwrap();
    assert_eq!(store.get(&id).await.unwrap().status, Status::Closed);
    assert_eq!(store.progress(&group).await.unwrap().completed, 2);
    let left = store
        .create(Create {
            title: "left".into(),
            kind: Some(Kind::Epic),
            ..Create::default()
        })
        .await
        .unwrap();
    let right = store
        .create(Create {
            title: "right".into(),
            kind: Some(Kind::Epic),
            ..Create::default()
        })
        .await
        .unwrap();
    store.bond(&left, &right).await.unwrap();
    assert!(
        store
            .graph(&right)
            .await
            .unwrap()
            .active_blockers()
            .contains(&left)
    );
    store
        .synchronize(Synchronization::SharedState)
        .await
        .unwrap();
}

pub async fn stale_writers(store: &impl Store, peer: &impl Store) {
    let id = store
        .create(Create {
            title: "stale".into(),
            status: Some(Status::Blocked),
            notes: Some("protected notes".into()),
            metadata: BTreeMap::from([("unrelated".into(), json!("preserved"))]),
            ..Create::default()
        })
        .await
        .unwrap();
    let first = store.get(&id).await.unwrap();
    let second = peer.get(&id).await.unwrap();
    for (writer, status) in [("first", first.status), ("second", second.status)] {
        let patch = Update {
            set_metadata: BTreeMap::from([("writer".into(), json!(writer))]),
            ..Update::default()
        };
        peer.update(&id, patch, Guard::Status(status))
            .await
            .unwrap();
    }
    let after = store.get(&id).await.unwrap();
    assert_eq!(after.status, Status::Blocked);
    assert_eq!(after.notes, first.notes);
    assert_eq!(after.metadata["unrelated"], json!("preserved"));
    assert_eq!(after.metadata["writer"], json!("second"));
    assert!(matches!(
        store
            .update(
                &id,
                Update {
                    title: Some("must not write".into()),
                    ..Update::default()
                },
                Guard::Status(Status::Open)
            )
            .await,
        Err(Error::Conflict { .. })
    ));
    assert_eq!(store.get(&id).await.unwrap().title, "stale");
    assert!(matches!(
        store
            .update(
                &id,
                Update::default(),
                Guard::Revision(task::Revision::new("old").unwrap())
            )
            .await,
        Err(Error::Unsupported { .. })
    ));
    assert!(!store.guarantees().atomic_batch);
    assert!(!store.guarantees().revision_guard);
}

pub async fn graph_guards(store: &impl Store) {
    let root = create(store, "graph root", None).await;
    let child = create(store, "graph child", Some(root.clone())).await;
    let sibling = create(store, "sibling", Some(root.clone())).await;
    assert!(store.relate(&root, &child, Relation::Blocks).await.is_err());
    assert!(store.relate(&child, &root, Relation::Blocks).await.is_err());
    assert!(
        store
            .relate(&child, &root, Relation::DiscoveredFrom)
            .await
            .is_err()
    );
    assert!(store.parent(&root, &child).await.is_err());
    assert!(
        store
            .relate(&child, &child, Relation::Blocks)
            .await
            .is_err()
    );
    store
        .relate(&child, &sibling, Relation::Blocks)
        .await
        .unwrap();
    assert!(
        store
            .relate(&sibling, &child, Relation::Blocks)
            .await
            .is_err()
    );
    assert_eq!(store.graph(&root).await.unwrap().active_blockers(), vec![]);
    assert_eq!(store.get(&root).await.unwrap().parent, None);
    assert_eq!(
        store.graph(&sibling).await.unwrap().active_blockers(),
        vec![]
    );
}

pub async fn interruptions(store: &impl Store, inject: impl Fn(Fault)) {
    let id = create(store, "interruptible", None).await;
    for timing in [Timing::BeforeWrite, Timing::AfterWrite] {
        inject(Fault {
            operation: Operation::Update,
            timing,
            skip: 0,
        });
        let result = store
            .update(
                &id,
                Update {
                    title: Some("written".into()),
                    ..Update::default()
                },
                Guard::None,
            )
            .await;
        if timing == Timing::AfterWrite {
            assert!(matches!(result, Err(Error::Uncertain { .. })));
        } else {
            assert!(matches!(
                result,
                Err(Error::Transport { .. } | Error::Uncertain { .. })
            ));
        }
        assert_eq!(
            store.get(&id).await.unwrap().title,
            if timing == Timing::AfterWrite {
                "written"
            } else {
                "interruptible"
            }
        );
    }
    let other = create(store, "other", None).await;
    inject(Fault {
        operation: Operation::Update,
        timing: Timing::BeforeWrite,
        skip: 1,
    });
    store
        .update(
            &id,
            Update {
                title: Some("first committed".into()),
                ..Update::default()
            },
            Guard::None,
        )
        .await
        .unwrap();
    assert!(
        store
            .update(
                &other,
                Update {
                    title: Some("second committed".into()),
                    ..Update::default()
                },
                Guard::None
            )
            .await
            .is_err()
    );
    assert_eq!(store.get(&id).await.unwrap().title, "first committed");
    assert_eq!(store.get(&other).await.unwrap().title, "other");
}

pub async fn inherited_readiness(store: &impl Store) {
    let root = create(store, "readiness parent", None).await;
    let child = create(store, "readiness child", Some(root.clone())).await;
    let blocker = create(store, "readiness blocker", None).await;
    store
        .relate(&root, &blocker, Relation::Blocks)
        .await
        .unwrap();
    assert_eq!(
        store
            .ready(Ready {
                parent: Some(root.clone()),
                ..Ready::default()
            })
            .await
            .unwrap(),
        vec![]
    );
    store.close(&blocker, None).await.unwrap();
    let ready = store
        .ready(Ready {
            parent: Some(root),
            ..Ready::default()
        })
        .await
        .unwrap();
    assert_eq!(
        ready
            .into_iter()
            .map(|record| record.id)
            .collect::<Vec<_>>(),
        vec![child]
    );
}

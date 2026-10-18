use loom_issues::{
    beads::{BdClient, BdError, CommandRunner, RunOutput},
    task::{self, Guard},
    tracking::{Error, Store, Synchronization},
};
use std::{
    ffi::OsString,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Transport {
    result: Mutex<Option<Result<RunOutput, BdError>>>,
    calls: Arc<AtomicUsize>,
    written: Arc<AtomicBool>,
    after_write: bool,
}
impl CommandRunner for Transport {
    async fn run(&self, _: Vec<OsString>, _: Duration) -> Result<RunOutput, BdError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.written.store(self.after_write, Ordering::SeqCst);
        self.result
            .lock()
            .map_err(|error| BdError::Spawn(std::io::Error::other(error.to_string())))?
            .take()
            .ok_or_else(|| BdError::Unsupported {
                reason: "fixture response exhausted".into(),
            })?
    }
}
fn adapter(
    result: Result<RunOutput, BdError>,
    after_write: bool,
) -> (BdClient<Transport>, Arc<AtomicUsize>, Arc<AtomicBool>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let written = Arc::new(AtomicBool::new(false));
    (
        BdClient::with_runner(Transport {
            result: Mutex::new(Some(result)),
            calls: calls.clone(),
            written: written.clone(),
            after_write,
        }),
        calls,
        written,
    )
}
fn failure(status: i32) -> RunOutput {
    RunOutput {
        status,
        stdout: Vec::new(),
        stderr: b"native failure".to_vec(),
    }
}
fn update() -> task::Update {
    task::Update {
        title: Some("changed".into()),
        ..task::Update::default()
    }
}

#[tokio::test]
async fn issues_adapters_preserve_failure_and_guarantee_semantics() {
    let id = task::Id::new("external-a").unwrap();
    let (adapter, calls, _) = adapter(Ok(failure(13)), false);
    assert!(matches!(
        Store::update(&adapter, &id, update(), Guard::Status(task::Status::Open)).await,
        Err(Error::Conflict { .. })
    ));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "failed predicates are never retried"
    );
    let guarantees = adapter.guarantees();
    assert!(!guarantees.revision_guard);
    assert!(!guarantees.atomic_batch);
    assert!(guarantees.status_predicate);

    let (client, _, _) = self::adapter(Ok(failure(1)), false);
    assert!(matches!(
        Store::update(
            &client,
            &id,
            update(),
            Guard::Revision(task::Revision::new("old").unwrap())
        )
        .await,
        Err(Error::Unsupported { .. })
    ));
    let (client, _, _) = self::adapter(
        Ok(RunOutput {
            status: 0,
            stdout: b"[]".to_vec(),
            stderr: Vec::new(),
        }),
        false,
    );
    assert!(matches!(client.get(&id).await, Err(Error::Rejected { .. })));
    let (client, _, _) = self::adapter(
        Err(BdError::Spawn(std::io::Error::other("unavailable"))),
        false,
    );
    assert!(matches!(
        client.get(&id).await,
        Err(Error::Transport { .. })
    ));
    let (client, _, written) = self::adapter(
        Err(BdError::Timeout {
            args: "update external-a".into(),
        }),
        true,
    );
    assert!(matches!(
        Store::update(&client, &id, update(), Guard::None).await,
        Err(Error::Uncertain { .. })
    ));
    assert!(written.load(Ordering::SeqCst));
}

#[tokio::test]
async fn revision_guards_are_refused_before_any_native_write() {
    let (adapter, calls, _) = adapter(Ok(failure(1)), false);
    assert!(matches!(
        Store::update(
            &adapter,
            &task::Id::new("external-a").unwrap(),
            update(),
            Guard::Revision(task::Revision::new("stale").unwrap())
        )
        .await,
        Err(Error::Unsupported { .. })
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn interrupted_mutations_preserve_uncertain_effects() {
    for result in [
        Err(BdError::Spawn(std::io::Error::other(
            "output pipe failed after execution",
        ))),
        Err(BdError::Timeout {
            args: "update external-a".into(),
        }),
        Err(BdError::Cleanup(std::io::Error::other(
            "cleanup interrupted",
        ))),
        Ok(failure(1)),
    ] {
        let (adapter, calls, written) = adapter(result, true);
        assert!(matches!(
            Store::update(
                &adapter,
                &task::Id::new("external-a").unwrap(),
                update(),
                Guard::None
            )
            .await,
            Err(Error::Uncertain { .. })
        ));
        assert!(written.load(Ordering::SeqCst));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn invalid_create_receipt_does_not_assert_creation_rollback() {
    let (adapter, _, written) = adapter(
        Ok(RunOutput {
            status: 0,
            stdout: b"invalid receipt".to_vec(),
            stderr: Vec::new(),
        }),
        true,
    );
    assert!(matches!(
        Store::create(&adapter, task::Create::default()).await,
        Err(Error::Uncertain { .. })
    ));
    assert!(written.load(Ordering::SeqCst));
}

#[tokio::test]
async fn read_transport_failure_is_not_a_missing_task() {
    let (adapter, _, _) = adapter(
        Err(BdError::Spawn(std::io::Error::other("unavailable"))),
        false,
    );
    assert!(matches!(
        adapter.get(&task::Id::new("external-a").unwrap()).await,
        Err(Error::Transport { .. })
    ));
}

#[tokio::test]
async fn missing_task_is_a_rejection_not_a_transport_error() {
    let (adapter, _, _) = adapter(
        Ok(RunOutput {
            status: 0,
            stdout: b"[]".to_vec(),
            stderr: Vec::new(),
        }),
        false,
    );
    assert!(matches!(
        adapter.get(&task::Id::new("external-a").unwrap()).await,
        Err(Error::Rejected { .. })
    ));
}

#[tokio::test]
async fn unknown_relationships_are_not_silently_dropped() {
    let (adapter, _, _) = adapter(Ok(RunOutput { status: 0, stdout: br#"[{"status":"open","dependencies":[{"id":"external-b","status":"open","dependency_type":"new-relation"}]}]"#.to_vec(), stderr: Vec::new() }), false);
    assert!(matches!(
        adapter.graph(&task::Id::new("external-a").unwrap()).await,
        Err(Error::InvalidResponse { .. })
    ));
}

#[tokio::test]
async fn shared_state_sync_does_not_spawn_and_publication_is_not_assumed() {
    let (adapter, calls, _) = adapter(Ok(failure(1)), false);
    adapter
        .synchronize(Synchronization::SharedState)
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        adapter.synchronize(Synchronization::Publish).await,
        Err(Error::Unsupported { .. })
    ));
}

#[tokio::test]
async fn lossy_metadata_updates_are_refused_before_any_write() {
    for value in [
        serde_json::json!("TRUE"),
        serde_json::json!("t"),
        serde_json::json!("123"),
        serde_json::json!("0x1p2"),
        serde_json::json!("1_000"),
        serde_json::json!({"nested": true}),
        serde_json::json!(9_007_199_254_740_993_u64),
    ] {
        let (adapter, calls, _) = adapter(Ok(failure(1)), false);
        let update = task::Update {
            title: Some("must not apply".into()),
            set_metadata: std::collections::BTreeMap::from([("field".into(), value)]),
            ..task::Update::default()
        };
        assert!(matches!(
            Store::update(
                &adapter,
                &task::Id::new("external-a").unwrap(),
                update,
                Guard::None
            )
            .await,
            Err(Error::Unsupported { .. })
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn lossy_nested_creation_metadata_is_refused_before_any_write() {
    let (adapter, calls, _) = adapter(Ok(failure(1)), false);
    let request = task::Create {
        metadata: std::collections::BTreeMap::from([(
            "nested".into(),
            serde_json::json!([{"number": 9_007_199_254_740_993_u64}]),
        )]),
        ..task::Create::default()
    };
    assert!(matches!(
        Store::create(&adapter, request).await,
        Err(Error::Unsupported { .. })
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

struct PendingMutation(Arc<AtomicBool>);
impl CommandRunner for PendingMutation {
    async fn run(&self, _: Vec<OsString>, _: Duration) -> Result<RunOutput, BdError> {
        self.0.store(true, Ordering::SeqCst);
        std::future::pending().await
    }
}
#[tokio::test]
async fn dropping_a_mutation_future_never_rolls_back_transport_effects() {
    let written = Arc::new(AtomicBool::new(false));
    let adapter = BdClient::with_runner(PendingMutation(written.clone()));
    let id = task::Id::new("external-a").unwrap();
    let mut future = Box::pin(Store::update(&adapter, &id, update(), Guard::None));
    std::future::poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    drop(future);
    assert!(written.load(Ordering::SeqCst));
}

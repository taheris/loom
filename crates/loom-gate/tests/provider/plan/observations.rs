use std::sync::atomic::{AtomicUsize, Ordering};

use super::{check, context, definition, fixture, name, offered, write};
use loom_gate::Tier;
use loom_gate::provider::plan::Plan;
use loom_gate::provider::tracked::{Context, Outcome};
use loom_gate::provider::{Error, Registry};
use loom_gate::scope::Request;

fn setup<F>(callback: F) -> (tempfile::TempDir, Plan)
where
    F: for<'a> Fn(&mut Context<'a>) -> Result<Outcome, Error> + Send + Sync + 'static,
{
    let dir = fixture();
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Accounting [check](accounting)\n- First [test](first)\n- Second [test](second)\n- Third [test](third)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let first = definition(&snapshot, &inventory, "first", callback);
    let second = first.clone().named(name("second")).unwrap();
    let registry = Registry::new(
        &snapshot,
        vec![
            offered(&inventory, &snapshot),
            first,
            second,
            definition(&snapshot, &inventory, "third", check),
        ],
    )
    .unwrap();
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    (dir, plan)
}

static FRESH: AtomicUsize = AtomicUsize::new(0);

fn record_fresh(context: &mut Context<'_>) -> Result<Outcome, Error> {
    FRESH.fetch_add(1, Ordering::SeqCst);
    check(context)
}

#[tokio::test]
async fn captured_aliases_keep_order_owners_and_one_fresh_execution_per_request() {
    let (dir, plan) = setup(record_fresh);
    let scope = Request::Tree.resolve(dir.path()).await.unwrap();
    let selection = plan.select_tiers(&scope, &[Tier::Test]).unwrap();
    drop(selection.observations().unwrap());
    assert_eq!(FRESH.load(Ordering::SeqCst), 0);
    for expected in 1..=2 {
        let observations = selection
            .observations()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(FRESH.load(Ordering::SeqCst), expected);
        assert_eq!(observations.len(), 3);
        for (observation, target) in observations.iter().zip(["first", "second", "third"]) {
            assert_eq!(observation.observed().target(), &name(target));
            assert_eq!(observation.owners().len(), 1);
            assert_eq!(observation.owners()[0].target, target);
            assert_eq!(observation.owners()[0].tier, Tier::Test);
            assert_eq!(observation.observed().outcome(), &Outcome::Pass);
            assert_ne!(observation.observed().observations(), []);
        }
        assert_eq!(observations[0].shared_with(), None);
        assert_eq!(observations[1].shared_with(), Some(&name("first")));
        assert_eq!(observations[2].shared_with(), None);
        assert_eq!(observations[0].duration(), observations[1].duration());
    }
    let scope = Request::Target("second".into())
        .resolve(dir.path())
        .await
        .unwrap();
    let selection = plan.select_tiers(&scope, &[Tier::Test]).unwrap();
    let observations = selection
        .observations()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].observed().target(), &name("second"));
    assert_eq!(observations[0].shared_with(), None);
    assert_eq!(FRESH.load(Ordering::SeqCst), 3);
}

static STALE: AtomicUsize = AtomicUsize::new(0);

fn record_stale(context: &mut Context<'_>) -> Result<Outcome, Error> {
    STALE.fetch_add(1, Ordering::SeqCst);
    check(context)
}

#[tokio::test]
async fn delivered_facts_never_authorize_changed_context_or_a_later_callback() {
    let (dir, plan) = setup(record_stale);
    let scope = Request::Tree.resolve(dir.path()).await.unwrap();
    let selection = plan.select_tiers(&scope, &[Tier::Test]).unwrap();
    let original = std::fs::read(dir.path().join("README.md")).unwrap();
    let mut observations = selection.observations().unwrap();
    let first = observations.next().unwrap().unwrap();
    write(dir.path(), "README.md", "# Changed after capture\n");
    let second = observations.next().unwrap().unwrap();
    assert_eq!(second.shared_with(), Some(first.observed().target()));
    assert_eq!(
        second.observed().observations(),
        first.observed().observations()
    );
    assert!(loom_gate::InputResolver::from_plan(&plan).is_err());
    assert!(matches!(observations.next(), Some(Err(Error::Snapshot(_)))));
    std::fs::write(dir.path().join("README.md"), original).unwrap();
    assert!(observations.next().is_none());
    assert!(observations.next().is_none());
    assert_eq!(STALE.load(Ordering::SeqCst), 1);
    assert_eq!(
        selection
            .observations()
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .shared_with(),
        None
    );
    assert_eq!(STALE.load(Ordering::SeqCst), 2);
}

static CALLBACK_READY: std::sync::Barrier = std::sync::Barrier::new(2);
static CALLBACK_RETURN: std::sync::Barrier = std::sync::Barrier::new(2);

fn interrupted(context: &mut Context<'_>) -> Result<Outcome, Error> {
    CALLBACK_READY.wait();
    CALLBACK_RETURN.wait();
    check(context)
}

#[tokio::test]
async fn a_callback_invalidating_context_cannot_expose_any_alias_projection() {
    let (dir, plan) = setup(interrupted);
    let scope = Request::Tree.resolve(dir.path()).await.unwrap();
    let selection = plan.select_tiers(&scope, &[Tier::Test]).unwrap();
    let original = std::fs::read(dir.path().join("README.md")).unwrap();
    let mut observations = selection.observations().unwrap();
    std::thread::scope(|threads| {
        let writer = threads.spawn(|| {
            CALLBACK_READY.wait();
            write(dir.path(), "README.md", "# Changed during callback\n");
            CALLBACK_RETURN.wait();
        });
        assert!(matches!(observations.next(), Some(Err(Error::Snapshot(_)))));
        writer.join().unwrap();
    });
    std::fs::write(dir.path().join("README.md"), original).unwrap();
    assert!(observations.next().is_none());
}

#[tokio::test]
async fn empty_observation_selection_still_rejects_stale_context() {
    let (dir, plan) = setup(check);
    let scope = Request::Files(Vec::new())
        .resolve(dir.path())
        .await
        .unwrap();
    let selection = plan.select_tiers(&scope, &[Tier::Test]).unwrap();
    assert!(selection.observations().unwrap().next().is_none());
    write(dir.path(), "README.md", "# Changed before empty capture\n");
    assert!(matches!(selection.observations(), Err(Error::Snapshot(_))));
}

fn batch_fixture<F, G>(
    second: F,
    last: G,
    final_is_snapshot_only: bool,
) -> (tempfile::TempDir, Plan)
where
    F: for<'a> Fn(&mut Context<'a>) -> Result<Outcome, Error> + Send + Sync + 'static,
    G: for<'a> Fn(&mut Context<'a>) -> Result<Outcome, Error> + Send + Sync + 'static,
{
    let dir = fixture();
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Accounting [check](accounting)\n- First [check](first)\n- Second [check](second)\n- Alias [check](second-alias)\n- Last [check](third)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let first = definition(&snapshot, &inventory, "first", check)
        .with_snapshot_only_batching()
        .unwrap();
    let second = definition(&snapshot, &inventory, "second", second)
        .with_snapshot_only_batching()
        .unwrap();
    let alias = second.clone().named(name("second-alias")).unwrap();
    let last = definition(&snapshot, &inventory, "third", last);
    let last = if final_is_snapshot_only {
        last.with_snapshot_only_batching().unwrap()
    } else {
        last
    };
    let registry = Registry::new(
        &snapshot,
        vec![offered(&inventory, &snapshot), first, second, alias, last],
    )
    .unwrap();
    (dir, registry.admit(&snapshot, &inventory).unwrap())
}

static BATCH_SECOND: AtomicUsize = AtomicUsize::new(0);
static OUTSIDE_BATCH: AtomicUsize = AtomicUsize::new(0);

fn batch_second(context: &mut Context<'_>) -> Result<Outcome, Error> {
    BATCH_SECOND.fetch_add(1, Ordering::SeqCst);
    check(context)
}

fn outside_batch(context: &mut Context<'_>) -> Result<Outcome, Error> {
    OUTSIDE_BATCH.fetch_add(1, Ordering::SeqCst);
    check(context)
}

#[tokio::test]
async fn adjacent_snapshot_checks_are_captured_without_crossing_the_next_effect() {
    let (dir, plan) = batch_fixture(batch_second, outside_batch, false);
    let scope = Request::Tree.resolve(dir.path()).await.unwrap();
    let selection = plan.select_tiers(&scope, &[Tier::Check]).unwrap();
    let mut stream = selection.observations().unwrap();
    assert_eq!(
        stream.next().unwrap().unwrap().observed().target(),
        &name("accounting")
    );
    assert_eq!(BATCH_SECOND.load(Ordering::SeqCst), 0);
    assert_eq!(
        stream.next().unwrap().unwrap().observed().target(),
        &name("first")
    );
    assert_eq!(BATCH_SECOND.load(Ordering::SeqCst), 1);
    assert_eq!(OUTSIDE_BATCH.load(Ordering::SeqCst), 0);
    write(dir.path(), "README.md", "# Changed after batch capture\n");
    let second = stream.next().unwrap().unwrap();
    let alias = stream.next().unwrap().unwrap();
    assert_eq!(second.observed().target(), &name("second"));
    assert_eq!(alias.observed().target(), &name("second-alias"));
    assert_eq!(
        second.observed().observations(),
        alias.observed().observations()
    );
    assert_eq!(alias.shared_with(), Some(second.observed().target()));
    assert!(matches!(stream.next(), Some(Err(Error::Snapshot(_)))));
    assert!(stream.next().is_none());
    assert_eq!(OUTSIDE_BATCH.load(Ordering::SeqCst), 0);
    assert!(plan.require_current().is_err());
}

fn batch_error(_: &mut Context<'_>) -> Result<Outcome, Error> {
    Err(Error::Missing {
        target: name("failed-callback"),
    })
}

static AFTER_BATCH_ERROR: AtomicUsize = AtomicUsize::new(0);

fn after_batch_error(context: &mut Context<'_>) -> Result<Outcome, Error> {
    AFTER_BATCH_ERROR.fetch_add(1, Ordering::SeqCst);
    check(context)
}

#[tokio::test]
async fn a_snapshot_batch_delivers_the_first_error_in_order_and_never_runs_later_callbacks() {
    let (dir, plan) = batch_fixture(batch_error, after_batch_error, true);
    let scope = Request::Tree.resolve(dir.path()).await.unwrap();
    let selection = plan.select_tiers(&scope, &[Tier::Check]).unwrap();
    let mut stream = selection.observations().unwrap();
    assert_eq!(
        stream.next().unwrap().unwrap().observed().target(),
        &name("accounting")
    );
    assert_eq!(
        stream.next().unwrap().unwrap().observed().target(),
        &name("first")
    );
    assert!(
        matches!(stream.next(), Some(Err(Error::Missing { target })) if target == name("failed-callback"))
    );
    assert!(stream.next().is_none());
    assert_eq!(AFTER_BATCH_ERROR.load(Ordering::SeqCst), 0);
}

static BATCH_INTERRUPTED: std::sync::Barrier = std::sync::Barrier::new(2);
static BATCH_CHANGED: std::sync::Barrier = std::sync::Barrier::new(2);

fn batch_interrupted(context: &mut Context<'_>) -> Result<Outcome, Error> {
    BATCH_INTERRUPTED.wait();
    BATCH_CHANGED.wait();
    check(context)
}

#[tokio::test]
async fn mutation_in_a_later_snapshot_check_blocks_the_entire_batch() {
    let (dir, plan) = batch_fixture(batch_interrupted, check, false);
    let scope = Request::Tree.resolve(dir.path()).await.unwrap();
    let selection = plan.select_tiers(&scope, &[Tier::Check]).unwrap();
    let mut stream = selection.observations().unwrap();
    assert_eq!(
        stream.next().unwrap().unwrap().observed().target(),
        &name("accounting")
    );
    std::thread::scope(|threads| {
        let writer = threads.spawn(|| {
            BATCH_INTERRUPTED.wait();
            write(dir.path(), "README.md", "# Changed during batch\n");
            BATCH_CHANGED.wait();
        });
        assert!(matches!(stream.next(), Some(Err(Error::Snapshot(_)))));
        writer.join().unwrap();
    });
    assert!(stream.next().is_none());
}

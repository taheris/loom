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

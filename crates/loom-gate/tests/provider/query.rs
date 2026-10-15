use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

fn registry(dir: &TempDir) -> Registry {
    let (snapshot, inventory) = context(dir);
    Registry::new(
        &snapshot,
        vec![definition(&snapshot, &inventory, "heading", check)],
    )
    .unwrap()
}

fn assert_changed(registry: &Registry) {
    assert!(matches!(
        registry.query(&[name("heading")]),
        Err(Error::Snapshot(source))
            if matches!(*source, loom_gate::inventory::snapshot::Error::Changed)
    ));
}

#[test]
fn query_rejects_changed_source_before_returning_a_descriptor() {
    let dir = fixture();
    let original = registry(&dir);
    write(dir.path(), "src/lib.rs", "pub fn changed_checker() {}\n");
    assert_changed(&original);
}

#[test]
fn query_rejects_an_omitted_new_subject_until_fresh_registration() {
    let dir = fixture();
    let original = registry(&dir);
    write(dir.path(), "new.md", "# New subject\n");
    assert_changed(&original);
    let restarted = registry(&dir);
    assert!(restarted.query(&[name("heading")]).is_ok());
    assert_changed(&original);
}

#[test]
fn query_rejects_deleted_subjects() {
    let dir = fixture();
    let original = registry(&dir);
    std::fs::remove_file(dir.path().join("README.md")).unwrap();
    assert_changed(&original);
}

#[cfg(unix)]
#[test]
fn query_rejects_changed_source_executable_mode() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = fixture();
    let original = registry(&dir);
    let source = dir.path().join("src/lib.rs");
    let mut permissions = std::fs::metadata(&source).unwrap().permissions();
    permissions.set_mode(permissions.mode() ^ 0o111);
    std::fs::set_permissions(source, permissions).unwrap();
    assert_changed(&original);
}

static EXECUTIONS: AtomicUsize = AtomicUsize::new(0);

fn counted_check(context: &mut Context<'_>) -> Result<Outcome, Error> {
    EXECUTIONS.fetch_add(1, Ordering::SeqCst);
    check(context)
}

#[test]
fn query_does_not_execute_valid_siblings_or_return_a_partial_invalid_batch() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(
        &snapshot,
        vec![definition(&snapshot, &inventory, "heading", counted_check)],
    )
    .unwrap();
    let before = EXECUTIONS.load(Ordering::SeqCst);
    assert!(matches!(
        registry.query(&[name("heading"), name("absent")]),
        Err(Error::Missing { target }) if target == name("absent")
    ));
    let json = registry.query(&[name("heading")]).unwrap();
    assert_eq!(EXECUTIONS.load(Ordering::SeqCst), before);
    let units = registry
        .resolve_query(&snapshot, &inventory, &json, &contracts(&["heading"]))
        .unwrap();
    assert_eq!(EXECUTIONS.load(Ordering::SeqCst), before);
    assert!(matches!(units[0].run().unwrap().outcome(), Outcome::Pass));
    assert_eq!(EXECUTIONS.load(Ordering::SeqCst), before + 1);
}

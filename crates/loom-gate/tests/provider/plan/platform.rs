use super::{context, fixture, selectable_plan};
use loom_gate::inventory::Inventory;
use loom_gate::provider::{Definition, Error, Registry};
use loom_gate::scope::{Platform, Request};

#[tokio::test]
async fn native_admission_cannot_be_upgraded_by_an_all_platform_selector() {
    let dir = fixture();
    let plan = selectable_plan(&dir);
    let scope = Request::Tree
        .resolve(dir.path())
        .await
        .unwrap()
        .with_platform(Platform::All);
    assert!(matches!(plan.select(&scope), Err(Error::PlatformScope)));
}

#[test]
fn all_platform_definitions_cannot_be_readmitted_as_native_definitions() {
    let dir = fixture();
    let (snapshot, native) = context(&dir);
    let all = Inventory::discover_for(&snapshot, Platform::All).unwrap();
    assert_eq!(all.platform(), Platform::All);
    let definitions = Definition::project_context(&snapshot, &all).unwrap();
    assert!(!definitions.is_empty());
    let registry = Registry::new(&snapshot, definitions).unwrap();
    assert!(matches!(
        registry.admit(&snapshot, &native),
        Err(Error::PlatformScope)
    ));
}

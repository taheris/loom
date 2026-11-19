#![cfg(test)]

use super::{Inventory, Resolution, ResourcePath, Snapshot};
use std::sync::Arc;

#[test]
fn admission_inputs_share_parsing_only_within_the_same_resolution_pass() {
    let directory = tempfile::tempdir().unwrap();
    loom_driver::git::init_test_repo(directory.path()).unwrap();
    let file = directory.path().join("README.md");
    std::fs::write(
        &file,
        "# Contract\n\n## Success Criteria\n- First. [check](first)\n- Second. [test](second)\n",
    )
    .unwrap();
    let snapshot = Snapshot::discover(directory.path()).unwrap();
    let inventory = Inventory::discover(&snapshot).unwrap();
    let contract = ResourcePath::try_from(std::path::PathBuf::from("README.md")).unwrap();
    let mut resolution = Resolution::new(&snapshot, &inventory);
    let first = resolution.declarations(&contract).unwrap();
    let second = resolution.declarations(&contract).unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(first.annotations.len(), 2);
    assert_eq!(first.annotations[0].target, "first");
    assert_eq!(first.annotations[1].target, "second");
    let mut independent = Resolution::new(&snapshot, &inventory);
    assert!(!Arc::ptr_eq(
        &first,
        &independent.declarations(&contract).unwrap()
    ));
    std::fs::write(
        &file,
        "# Changed\n\n## Success Criteria\n- Fixed. [check](fixed)\n",
    )
    .unwrap();
    assert!(snapshot.require_current().is_err());
    let changed = Snapshot::discover(directory.path()).unwrap();
    let inventory = Inventory::discover(&changed).unwrap();
    let mut resolution = Resolution::new(&changed, &inventory);
    let current = resolution.declarations(&contract).unwrap();
    assert!(!Arc::ptr_eq(&first, &current));
    assert_eq!(current.annotations.len(), 1);
    assert_eq!(current.annotations[0].target, "fixed");
}

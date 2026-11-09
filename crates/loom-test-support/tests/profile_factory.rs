#![cfg(feature = "acceptance")]

use loom_test_support::profile_manifest::ProfileFactoryCommand;
use std::process::Command;

#[test]
fn failed_factory_preparation_never_starts_a_child_or_allows_a_retry() {
    let root = tempfile::tempdir().unwrap();
    let mut command = Command::new("sh");
    command.current_dir(root.path()).args(["-c", "exit 0"]);
    let mut factory = command.profile_factory(root.path().join("missing-manifest.json"));
    assert!(factory.status().is_err());
    assert!(factory.output().is_err());
    assert!(factory.spawn().is_err());
    assert!(factory.status().is_err());
}

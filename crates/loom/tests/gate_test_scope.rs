#![allow(clippy::unwrap_used)]

use std::fs;
use std::process::Command;

use tempfile::TempDir;

fn loom_bin() -> &'static str {
    env!("CARGO_BIN_EXE_loom")
}

fn write_fixture() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::create_dir_all(dir.path().join("specs")).unwrap();
    fs::create_dir_all(dir.path().join(".loom")).unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"scope-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("src/lib.rs"),
        "#[test]\nfn scoped_test_kept() {}\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("specs/gate.md"),
        "# Gate\n\n## Success Criteria\n\n- scoped test runs for touched source [test](scoped_test_kept)\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("runner.sh"),
        "#!/bin/sh\nprintf '%s\\n' \"$*\" > runner.log\nprintf '{\"pass\": true, \"evidence\": \"argv=%s\"}\\n' \"$*\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("loom.toml"),
        "[runner.test]\ncommand = \"sh runner.sh {paths}\"\n",
    )
    .unwrap();
    dir
}

#[test]
fn test_tier_filters_targets_by_files_scope_intersection() {
    let dir = write_fixture();
    let output = Command::new(loom_bin())
        .current_dir(dir.path())
        .args(["gate", "test", "--files", "src/lib.rs", "--spec", "gate"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "loom gate test must exit cleanly, stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("no annotations matched scope filter"),
        "CLI must not drop the pre-filtered test batch with EmptyScope: {stderr}",
    );
    let log = fs::read_to_string(dir.path().join("runner.log")).unwrap();
    assert!(
        log.contains("scoped_test_kept"),
        "runner must receive the in-scope test target, log={log}",
    );
}

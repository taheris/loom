//! Exercise the actual hermetic analysis entrypoint, including failing source and dispatch controls.

#![cfg(test)]
#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::{Command, Output};

fn fixture(criteria: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    loom_driver::git::init_test_repo(root).unwrap();
    for (path, bytes) in [
        (
            "Cargo.toml",
            "[workspace]\nmembers=['crates/demo']\nresolver='3'\n",
        ),
        (
            "crates/demo/Cargo.toml",
            "[package]\nname='demo'\nversion='0.1.0'\nedition='2024'\n",
        ),
        ("crates/demo/src/lib.rs", "pub fn work() {}\n"),
        ("specs/demo.md", criteria),
    ] {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, bytes).unwrap();
    }
    let result = Command::new("git")
        .args(["add", "--all"])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(result.success());
    dir
}

fn run(root: impl AsRef<Path>) -> Output {
    Command::new(env!("CARGO_BIN_EXE_loom-walk"))
        .arg("--from-specs")
        .env("LOOM_FILES", "crates/absent.rs")
        .current_dir(root)
        .output()
        .unwrap()
}

#[test]
fn source_producer_runs_registered_analysis_once_and_detects_actual_source_failure() {
    let dir = fixture(
        "## Success Criteria\n- No panics [check](cargo run -p loom-walk\n  -- no_panics_in_production)\n- Same check [check](no_panics_in_production)\n- Native sibling [check](nix build .#native-verifier)\n",
    );
    let output = run(&dir);
    assert!(output.status.success(), "{output:?}");
    let records: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["target"], "no_panics_in_production");
    assert_eq!(records[0]["pass"], true);
    std::fs::write(
        dir.path().join("crates/demo/src/lib.rs"),
        "pub fn work() { panic!(\"actual production panic\"); }\n",
    )
    .unwrap();
    let output = run(&dir);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let record: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(record["pass"], false);
    assert!(record["evidence"].as_str().unwrap().contains("panic"));
}

#[test]
fn source_producer_rejects_raw_commands_without_dispatching_any_command() {
    let dir = fixture(
        "## Success Criteria\n- Real analysis [check](no_panics_in_production)\n- Not a registered analysis [check](touch unauthorized)\n",
    );
    let output = run(&dir);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unsupported source-analysis binding"),
        "{output:?}"
    );
    assert_eq!(output.stdout.as_slice(), b"");
    assert!(!dir.path().join("unauthorized").exists());
}

#[test]
fn source_producer_rejects_unknown_walks_instead_of_reporting_empty_success() {
    let dir = fixture(
        "## Success Criteria\n- Missing implementation [check](cargo run -p loom-walk -- no_such_walk)\n",
    );
    let output = run(&dir);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no_such_walk"),
        "{output:?}"
    );
    assert_eq!(output.stdout.as_slice(), b"");
}

//! Process-level contracts for `loom spec` annotation target discovery.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::{Command, Output};

fn run_spec(workspace: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_loom"))
        .arg("--workspace")
        .arg(workspace)
        .arg("spec")
        .args(args)
        .env_remove("LOOM_INSIDE")
        .output()
        .expect("spawn loom spec")
}

#[test]
fn spec_targets_lists_annotation_targets_with_tier_and_plain_modes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let specs = dir.path().join("specs");
    std::fs::create_dir_all(&specs).expect("create specs directory");
    std::fs::write(
        specs.join("alpha.md"),
        "## Success Criteria\n\n\
         - check [check](cargo run -p loom-walk -- alpha_contract)\n\
         - test [test](crate::module::target_with_spaces)\n\
         - system [system?](nix run .#alpha-smoke)\n\
         - judge [judge](../tests/judges/alpha.sh#judge_alpha)\n",
    )
    .expect("write spec");

    let all = run_spec(dir.path(), &["alpha", "--targets"]);
    assert!(
        all.status.success(),
        "all targets failed: {}",
        String::from_utf8_lossy(&all.stderr),
    );
    assert_eq!(
        String::from_utf8(all.stdout).expect("utf-8 target output"),
        "[check] cargo run -p loom-walk -- alpha_contract\n\
         [test] crate::module::target_with_spaces\n\
         [system] nix run .#alpha-smoke\n\
         [judge] ../tests/judges/alpha.sh#judge_alpha\n",
    );

    let test_only = run_spec(dir.path(), &["alpha", "--targets", "--tier", "test"]);
    assert!(
        test_only.status.success(),
        "tier-filtered targets failed: {}",
        String::from_utf8_lossy(&test_only.stderr),
    );
    assert_eq!(
        String::from_utf8(test_only.stdout).expect("utf-8 filtered output"),
        "[test] crate::module::target_with_spaces\n",
    );

    let plain_judge = run_spec(
        dir.path(),
        &["alpha", "--targets", "--tier", "judge", "--plain"],
    );
    assert!(
        plain_judge.status.success(),
        "plain targets failed: {}",
        String::from_utf8_lossy(&plain_judge.stderr),
    );
    assert_eq!(
        String::from_utf8(plain_judge.stdout).expect("utf-8 plain output"),
        "../tests/judges/alpha.sh#judge_alpha\n",
    );
}

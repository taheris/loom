//! CLI guard at process entry: when `LOOM_INSIDE=1` is set in the host
//! environment, container-spawning and workspace-mutating subcommands
//! refuse to execute and read-only/deterministic inspection subcommands
//! run normally.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

fn loom_with_inside_env(args: &[&str]) -> std::process::Output {
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    Command::new(loom_bin)
        .args(args)
        .env("LOOM_INSIDE", "1")
        .env_remove("LOOM_PROFILES_MANIFEST")
        .output()
        .expect("spawn loom")
}

#[test]
fn mutating_and_llm_spawning_subcommands_refuse_with_loom_inside_set() {
    for sub in [
        &["init"][..],
        &["use", "harness"],
        &["plan", "tmp"],
        &["loop"],
        &["gate", "audit", "--tree"],
        &["gate", "mint", "--tree"],
        &["gate", "review", "--tree"],
        &["gate", "judge", "--tree"],
        &["gate", "rubric", "--tree"],
        &["inbox", "chat"],
        &["todo"],
    ] {
        let out = loom_with_inside_env(sub);
        assert!(
            !out.status.success(),
            "expected refusal for `loom {}` under LOOM_INSIDE=1, got success",
            sub.join(" "),
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("loom cannot run inside a loom-managed container"),
            "expected guard error in stderr for `loom {}`, got:\n{stderr}",
            sub.join(" "),
        );
    }
}

#[test]
fn plan_accepts_optional_anchor_labels_and_interspersed_options() {
    for args in [
        &["plan", "--profile", "base", "alpha", "beta"][..],
        &["plan", "alpha", "--profile", "base", "beta"],
        &["plan", "alpha", "beta", "--profile", "base"],
    ] {
        let out = loom_with_inside_env(args);
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("loom cannot run inside a loom-managed container"),
            "plan args should parse and then hit the nested-loom guard, got:\n{stderr}",
        );
    }
}

#[test]
fn readonly_and_deterministic_gate_subcommands_run_under_loom_inside_set() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::create_dir_all(workspace.join(".loom")).unwrap();
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(workspace.join("specs/dummy.md"), "# dummy\n").unwrap();
    let db = loom_driver::state::CacheDb::open(workspace.join(".loom/cache.db")).unwrap();
    db.upsert_spec(
        &loom_driver::identifier::SpecLabel::new("dummy"),
        "specs/dummy.md",
    )
    .unwrap();
    drop(db);

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    for sub in [
        &["status"][..],
        &["logs"],
        &["spec"],
        &["inbox"],
        &["inbox", "list"],
        &["inbox", "view", "1"],
        &["gate"],
        &["gate", "status", "--tree"],
        &["gate", "verify", "--tree"],
        &["gate", "check", "--tree"],
        &["gate", "test", "--tree"],
        &["gate", "system", "--tree"],
        &["gate", "verify-marker"],
    ] {
        let out = Command::new(loom_bin)
            .arg("--workspace")
            .arg(workspace)
            .args(sub)
            .env("LOOM_INSIDE", "1")
            .output()
            .expect("spawn loom");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("loom cannot run inside"),
            "inspection `loom {}` should bypass nested-loom guard, got:\n{stderr}",
            sub.join(" "),
        );
    }
}

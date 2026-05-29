//! CLI guard at process entry: when `LOOM_INSIDE=1` is set in the host
//! environment, container-spawning and workspace-mutating subcommands
//! refuse to execute and read-only subcommands run normally.
//!
//! Spec: `harness.md` § Nested-Loom Guard, success criteria
//! `test_nested_loom_guard_refuses` / `test_nested_loom_guard_allows_readonly`.

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
fn mutating_subcommands_refuse_with_loom_inside_set() {
    // Each invocation appends `--help` so clap would normally exit 0 with
    // help text; the guard must intercept before clap reaches that path.
    // (Plain `init`/`run`/etc. would also work but might fail for other
    // reasons in CI; the guard runs *after* parse, so the subcommand name
    // is what we care about.)
    for sub in [
        &["init"][..],
        &["use", "harness"],
        &["plan", "-n", "tmp"],
        &["loop", "--once"],
        // `gate audit` triggers an LLM rubric path that spawns containers,
        // so it falls under the nested-loom guard. The deterministic
        // `gate` paths (bare status, `verify` / `check` / `test` /
        // `system`) are read-only and tested in the bypass case below.
        &["gate", "audit"],
        &["msg"],
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
fn readonly_subcommands_run_under_loom_inside_set() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::create_dir_all(workspace.join(".loom")).unwrap();
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(workspace.join("specs/dummy.md"), "# dummy\n").unwrap();
    let db = loom_driver::state::StateDb::open(workspace.join(".loom/state.db")).unwrap();
    db.set_current_spec(&loom_driver::identifier::SpecLabel::new("dummy"))
        .unwrap();
    drop(db);

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    for sub in [&["status"][..], &["logs"], &["spec"], &["gate"]] {
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
            "read-only `loom {}` should bypass nested-loom guard, got:\n{stderr}",
            sub.join(" "),
        );
    }
}

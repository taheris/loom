//! Integration tests for [`build_criterion_status`] that need a real git
//! repo. Pure logic tests for parse/result mapping live alongside the
//! parser in `loom-gate`; the cases here all exercise `commits_since`,
//! which only has anything to assert against a real `HEAD` history.
//!
//! These tests spawn the system `git` binary to seed and inspect a real
//! workspace (spec NFR #8) — `loom_driver::git::GitClient` deliberately
//! exposes no repo-setup surface.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;

use loom_driver::git::GitClient;
use loom_driver::identifier::SpecLabel;
use loom_gate::annotation::{Tier, parse_content};
use loom_gate::cache::{CacheRow, StatusCache, Verdict};
use loom_templates::criterion_status::CriterionResult;
use loom_workflow::todo::build_criterion_status;
use tempfile::TempDir;

fn init_git_repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    let must = |args: &[&str]| {
        let s = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .status()
            .unwrap();
        assert!(s.success(), "git {args:?} failed");
    };
    must(&["init", "-q", "-b", "main"]);
    must(&["config", "user.email", "test@example.com"]);
    must(&["config", "user.name", "Test"]);
    must(&["config", "commit.gpgsign", "false"]);
    std::fs::write(path.join("README.md"), "initial\n").unwrap();
    must(&["add", "README.md"]);
    must(&["commit", "-q", "-m", "initial"]);
    dir
}

fn head_sha(repo: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().into()
}

fn add_empty_commit(repo: &Path, msg: &str) {
    let s = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["commit", "-q", "--allow-empty", "-m", msg])
        .status()
        .unwrap();
    assert!(s.success());
}

fn write_spec(workspace: &Path, label: &str, body: &str) -> PathBuf {
    let specs_dir = workspace.join("specs");
    std::fs::create_dir_all(&specs_dir).unwrap();
    let rel = PathBuf::from("specs").join(format!("{label}.md"));
    std::fs::write(workspace.join(&rel), body).unwrap();
    rel
}

#[tokio::test]
async fn empty_cache_yields_no_result_for_every_criterion() {
    let dir = init_git_repo();
    let workspace = dir.path();
    let label = SpecLabel::new("alpha");
    let body = "\
## Success Criteria

- First criterion [check](cargo run -p w -- a)
- Second criterion [test](crate::t::b)
";
    let spec_rel = write_spec(workspace, "alpha", body);
    let git = GitClient::open(workspace).unwrap();
    let cache_path = workspace.join(".wrapix/loom/gate-cache.sqlite");

    let rows = build_criterion_status(workspace, &cache_path, &label, &spec_rel, &git).await;
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|r| matches!(r.last_result, CriterionResult::NoResult))
    );
    assert!(rows.iter().all(|r| r.last_timestamp_ms.is_none()));
    assert!(rows.iter().all(|r| r.last_commit.is_none()));
    assert!(rows.iter().all(|r| r.commits_since.is_none()));
    assert_eq!(rows[0].annotation, "[check](cargo run -p w -- a)");
    assert_eq!(rows[1].annotation, "[test](crate::t::b)");
}

#[tokio::test]
async fn cache_hit_with_stale_commit_renders_non_zero_commits_since() {
    let dir = init_git_repo();
    let workspace = dir.path();
    let label = SpecLabel::new("alpha");
    let body = "\
## Success Criteria

- A criterion [check](cargo run -p w -- a)
";
    let spec_rel = write_spec(workspace, "alpha", body);
    let stale = head_sha(workspace);
    add_empty_commit(workspace, "second");
    add_empty_commit(workspace, "third");

    let parsed = parse_content(&spec_rel, body);
    let crit_line = parsed.criteria[0].line;

    let cache_path = workspace.join(".wrapix/loom/gate-cache.sqlite");
    let cache = StatusCache::open(&cache_path).unwrap();
    cache
        .upsert(&CacheRow {
            spec_label: "alpha".into(),
            criterion_anchor: crit_line.to_string(),
            tier: Tier::Check,
            annotation_target: "cargo run -p w -- a".into(),
            last_run_ts_ms: 1_700_000_000_000,
            last_run_commit: stale.clone(),
            verdict: Verdict::Pass,
            evidence: "ok".into(),
        })
        .unwrap();
    drop(cache);

    let git = GitClient::open(workspace).unwrap();
    let rows = build_criterion_status(workspace, &cache_path, &label, &spec_rel, &git).await;
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(r.last_result, CriterionResult::Pass);
    assert_eq!(r.last_commit.as_deref(), Some(stale.as_str()));
    assert_eq!(r.last_timestamp_ms, Some(1_700_000_000_000));
    assert_eq!(r.commits_since, Some(2));
    assert_eq!(r.annotation, "[check](cargo run -p w -- a)");
}

#[tokio::test]
async fn missing_spec_file_returns_empty_vec() {
    let dir = init_git_repo();
    let workspace = dir.path();
    let label = SpecLabel::new("ghost");
    let spec_rel = PathBuf::from("specs/ghost.md");
    let git = GitClient::open(workspace).unwrap();
    let cache_path = workspace.join(".wrapix/loom/gate-cache.sqlite");
    let rows = build_criterion_status(workspace, &cache_path, &label, &spec_rel, &git).await;
    assert!(rows.is_empty());
}

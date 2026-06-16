//! Integration tests for [`build_criterion_status`] that need a real git repo.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;

use loom_driver::git::GitClient;
use loom_driver::identifier::SpecLabel;
use loom_gate::annotation::{Tier, parse_content};
use loom_gate::cache::{CacheRow, StatusCache, Verdict};
use loom_templates::criterion_status::{CriterionResult, EvidenceState};
use loom_workflow::todo::{build_criterion_status, criterion_id_for, criterion_text_for_line};
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

fn criterion_id(label: &SpecLabel, body: &str, line_index: usize) -> String {
    let parsed = parse_content(Path::new("specs/alpha.md"), body);
    let line = parsed.criteria[line_index].line;
    let next = parsed.criteria.get(line_index + 1).map(|c| c.line);
    criterion_id_for(label, &criterion_text_for_line(body, line, next))
        .as_str()
        .to_string()
}

fn cache_row(label: &str, id: String, target: &str, commit: String) -> CacheRow {
    CacheRow {
        spec_label: label.into(),
        criterion_anchor: id,
        tier: Tier::Check,
        annotation_target: target.into(),
        last_run_ts_ms: 1_700_000_000_000,
        last_run_commit: commit,
        verdict: Verdict::Pass,
        evidence: "ok".into(),
    }
}

#[tokio::test]
async fn empty_cache_yields_missing_evidence_for_every_criterion() {
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
    let cache_path = workspace.join(".loom/cache.db");

    let rows = build_criterion_status(workspace, &cache_path, &label, &spec_rel, &git).await;
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|r| matches!(r.evidence, EvidenceState::Missing))
    );
    assert!(
        cache_path.exists(),
        "fresh .loom/cache.db should be created"
    );
    assert_eq!(
        rows[0].annotation.to_string(),
        "[check](cargo run -p w -- a)"
    );
    assert_eq!(rows[1].annotation.to_string(), "[test](crate::t::b)");
    assert_eq!(rows[0].criterion_text, "First criterion");
    assert_eq!(rows[1].criterion_text, "Second criterion");
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

    let cache_path = workspace.join(".loom/cache.db");
    let cache = StatusCache::open(&cache_path).unwrap();
    cache
        .upsert(&cache_row(
            "alpha",
            criterion_id(&label, body, 0),
            "cargo run -p w -- a",
            stale.clone(),
        ))
        .unwrap();
    drop(cache);

    let git = GitClient::open(workspace).unwrap();
    let rows = build_criterion_status(workspace, &cache_path, &label, &spec_rel, &git).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].evidence,
        EvidenceState::Current {
            result: CriterionResult::Pass,
            last_timestamp_ms: 1_700_000_000_000,
            last_commit: stale,
            commits_since: 2,
        }
    );
    assert_eq!(
        rows[0].annotation.to_string(),
        "[check](cargo run -p w -- a)"
    );
}

#[tokio::test]
async fn todo_populates_criterion_status_from_cache_db() {
    let dir = init_git_repo();
    let workspace = dir.path();
    let label = SpecLabel::new("alpha");
    let body = "\
## Success Criteria

- Cached criterion [check](cargo run -p w -- a)
- Uncached criterion [test](crate::t::b)
";
    let spec_rel = write_spec(workspace, "alpha", body);
    let head = head_sha(workspace);
    let cache_path = workspace.join(".loom/cache.db");
    let cache = StatusCache::open(&cache_path).unwrap();
    cache
        .upsert(&cache_row(
            "alpha",
            criterion_id(&label, body, 0),
            "cargo run -p w -- a",
            head.clone(),
        ))
        .unwrap();
    drop(cache);

    let git = GitClient::open(workspace).unwrap();
    let rows = build_criterion_status(workspace, &cache_path, &label, &spec_rel, &git).await;
    assert_eq!(rows.len(), 2);
    assert!(matches!(rows[0].evidence, EvidenceState::Current { .. }));
    assert!(matches!(rows[1].evidence, EvidenceState::Missing));
}

#[tokio::test]
async fn stale_annotation_detected_when_cached_binding_differs() {
    let dir = init_git_repo();
    let workspace = dir.path();
    let label = SpecLabel::new("alpha");
    let body = "\
## Success Criteria

- A criterion [check](cargo run -p w -- new)
";
    let spec_rel = write_spec(workspace, "alpha", body);
    let head = head_sha(workspace);
    let cache_path = workspace.join(".loom/cache.db");
    let cache = StatusCache::open(&cache_path).unwrap();
    cache
        .upsert(&cache_row(
            "alpha",
            criterion_id(&label, body, 0),
            "cargo run -p w -- old",
            head,
        ))
        .unwrap();
    drop(cache);

    let git = GitClient::open(workspace).unwrap();
    let rows = build_criterion_status(workspace, &cache_path, &label, &spec_rel, &git).await;
    assert_eq!(rows.len(), 1);
    let EvidenceState::StaleAnnotation {
        cached_annotation, ..
    } = &rows[0].evidence
    else {
        panic!("expected stale annotation evidence");
    };
    assert_eq!(
        cached_annotation.to_string(),
        "[check](cargo run -p w -- old)"
    );
}

#[tokio::test]
async fn legacy_gate_cache_file_is_not_a_live_input() {
    let dir = init_git_repo();
    let workspace = dir.path();
    let label = SpecLabel::new("alpha");
    let body = "\
## Success Criteria

- A criterion [check](cargo run -p w -- a)
";
    let spec_rel = write_spec(workspace, "alpha", body);
    let head = head_sha(workspace);
    let legacy_path = workspace.join(".loom/gate-cache.sqlite");
    let legacy = StatusCache::open(&legacy_path).unwrap();
    legacy
        .upsert(&cache_row(
            "alpha",
            criterion_id(&label, body, 0),
            "cargo run -p w -- a",
            head,
        ))
        .unwrap();
    drop(legacy);

    let git = GitClient::open(workspace).unwrap();
    let rows = build_criterion_status(
        workspace,
        &workspace.join(".loom/cache.db"),
        &label,
        &spec_rel,
        &git,
    )
    .await;
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0].evidence, EvidenceState::Missing));
}

#[tokio::test]
async fn criterion_status_commits_since_computed_from_git_rev_list() {
    let dir = init_git_repo();
    let workspace = dir.path();
    let label = SpecLabel::new("alpha");
    let body = "\
## Success Criteria

- A criterion [check](cargo run -p w -- a)
";
    let spec_rel = write_spec(workspace, "alpha", body);
    let stale = head_sha(workspace);
    add_empty_commit(workspace, "one");
    add_empty_commit(workspace, "two");
    add_empty_commit(workspace, "three");

    let cache_path = workspace.join(".loom/cache.db");
    let cache = StatusCache::open(&cache_path).unwrap();
    cache
        .upsert(&cache_row(
            "alpha",
            criterion_id(&label, body, 0),
            "cargo run -p w -- a",
            stale,
        ))
        .unwrap();
    drop(cache);

    let git = GitClient::open(workspace).unwrap();
    let rows = build_criterion_status(workspace, &cache_path, &label, &spec_rel, &git).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].evidence.commits_since_label(), "3");
}

#[tokio::test]
async fn criterion_status_commits_since_is_missing_when_no_cache_row() {
    let dir = init_git_repo();
    let workspace = dir.path();
    let label = SpecLabel::new("alpha");
    let body = "\
## Success Criteria

- Uncached [check](cargo run -p w -- a)
";
    let spec_rel = write_spec(workspace, "alpha", body);
    let git = GitClient::open(workspace).unwrap();
    let cache_path = workspace.join(".loom/cache.db");
    let rows = build_criterion_status(workspace, &cache_path, &label, &spec_rel, &git).await;
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0].evidence, EvidenceState::Missing));
}

#[tokio::test]
async fn missing_spec_file_returns_empty_vec() {
    let dir = init_git_repo();
    let workspace = dir.path();
    let label = SpecLabel::new("ghost");
    let spec_rel = PathBuf::from("specs/ghost.md");
    let git = GitClient::open(workspace).unwrap();
    let cache_path = workspace.join(".loom/cache.db");
    let rows = build_criterion_status(workspace, &cache_path, &label, &spec_rel, &git).await;
    assert!(rows.is_empty());
}

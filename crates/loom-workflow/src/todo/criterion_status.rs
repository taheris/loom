//! Build `criterion_status` rows for `todo_new` / `todo_update` prompts.
//!
//! Reads the loom-gate sqlite status cache for the spec being decomposed,
//! parses the spec to enumerate every criterion in scope, and joins the two
//! into a [`Vec<CriterionStatus>`] threaded into the template context. For
//! cache rows with a recorded `last_run_commit`, computes `commits_since`
//! against the current `HEAD` via `git rev-list --count`. Per
//! `specs/templates.md` § Criterion-Status Surface, criteria with no cache
//! row arrive as [`CriterionResult::NoResult`] — verifiers are NOT invoked
//! inline as a fallback.

use std::collections::BTreeMap;
use std::path::Path;

use loom_driver::git::GitClient;
use loom_driver::identifier::SpecLabel;
use loom_gate::annotation::{Annotation, parse_content};
use loom_gate::cache::{CacheRow, StatusCache, Verdict};
use loom_templates::criterion_status::{CriterionResult, CriterionStatus};
use tracing::warn;

/// Build the `criterion_status` vec for one spec's todo render.
///
/// `spec_path` is the workspace-relative path used both for filesystem read
/// and as the source stamp on parser records (the parser's
/// `spec_label_from_path` helper derives the label from the file stem).
/// `cache_path` is the loom-gate sqlite cache; a missing file yields the
/// empty-cache path where every parsed criterion arrives as `NoResult`.
pub async fn build_criterion_status(
    workspace: &Path,
    cache_path: &Path,
    spec_label: &SpecLabel,
    spec_path: &Path,
    git: &GitClient,
) -> Vec<CriterionStatus> {
    let spec_abs = workspace.join(spec_path);
    let Ok(content) = std::fs::read_to_string(&spec_abs) else {
        return Vec::new();
    };
    let parsed = parse_content(spec_path, &content);

    let mut annotations_by_line: BTreeMap<u32, Vec<&Annotation>> = BTreeMap::new();
    for ann in &parsed.annotations {
        annotations_by_line.entry(ann.criterion_line).or_default().push(ann);
    }

    let cache_rows = if cache_path.exists() {
        match StatusCache::open(cache_path) {
            Ok(cache) => cache
                .read_for_spec(spec_label.as_str())
                .unwrap_or_else(|e| {
                    warn!(
                        spec_label = %spec_label,
                        error = %e,
                        "loom todo: failed to read status cache; rendering with empty criterion_status",
                    );
                    Vec::new()
                }),
            Err(e) => {
                warn!(
                    spec_label = %spec_label,
                    error = %e,
                    "loom todo: failed to open status cache; rendering with empty criterion_status",
                );
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let cache_by_anchor: BTreeMap<String, &CacheRow> = cache_rows
        .iter()
        .map(|row| (row.criterion_anchor.clone(), row))
        .collect();

    let mut out: Vec<CriterionStatus> = Vec::new();
    for crit in &parsed.criteria {
        let anchor = crit.line.to_string();
        let cache_row = cache_by_anchor.get(&anchor).copied();
        let parsed_anns = annotations_by_line.get(&crit.line);
        match (cache_row, parsed_anns) {
            (Some(row), _) => {
                let commits_since = git
                    .commits_since(&row.last_run_commit)
                    .await
                    .ok();
                out.push(CriterionStatus {
                    criterion_anchor: anchor,
                    annotation: format!("[{}]({})", row.tier.as_wire(), row.annotation_target),
                    last_result: verdict_to_result(row.verdict),
                    last_timestamp_ms: Some(row.last_run_ts_ms),
                    last_commit: Some(row.last_run_commit.clone()),
                    commits_since,
                });
            }
            (None, Some(anns)) => {
                for ann in anns {
                    out.push(CriterionStatus {
                        criterion_anchor: anchor.clone(),
                        annotation: format!("[{}]({})", ann.tier.as_wire(), ann.target),
                        last_result: CriterionResult::NoResult,
                        last_timestamp_ms: None,
                        last_commit: None,
                        commits_since: None,
                    });
                }
            }
            (None, None) => {
                out.push(CriterionStatus {
                    criterion_anchor: anchor,
                    annotation: String::new(),
                    last_result: CriterionResult::NoResult,
                    last_timestamp_ms: None,
                    last_commit: None,
                    commits_since: None,
                });
            }
        }
    }
    out
}

fn verdict_to_result(verdict: Verdict) -> CriterionResult {
    match verdict {
        Verdict::Pass => CriterionResult::Pass,
        Verdict::Fail => CriterionResult::Fail,
        Verdict::Skipped => CriterionResult::Skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_gate::annotation::Tier;
    use std::path::PathBuf;
    use std::process::Command;
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
        assert!(rows.iter().all(|r| matches!(r.last_result, CriterionResult::NoResult)));
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
}

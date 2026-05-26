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
        annotations_by_line
            .entry(ann.criterion_line)
            .or_default()
            .push(ann);
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
                let commits_since = git.commits_since(&row.last_run_commit).await.ok();
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

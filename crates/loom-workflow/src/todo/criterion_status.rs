//! Build `criterion_status` rows for todo prompts.
//!
//! The todo prompt consumes the current spec text, not the cache schema, as
//! the source of criteria in scope. `.loom/cache.db` contributes verifier
//! evidence only; absent rows become [`EvidenceState::Missing`].

use std::collections::BTreeMap;
use std::path::Path;

use loom_driver::git::GitClient;
use loom_driver::identifier::SpecLabel;
use loom_gate::annotation::{Annotation, Tier, parse_content};
use loom_gate::cache::{CacheRow, StatusCache, Verdict};
use loom_protocol::todo::GitSha;
use loom_templates::criterion_status::{
    AnnotationTarget, AnnotationTier, CriterionAnnotation, CriterionId, CriterionResult,
    CriterionStatus, EvidenceState,
};
use tracing::warn;

/// Build the `criterion_status` vec for one spec's todo render.
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

    let cache_rows = match StatusCache::open(cache_path) {
        Ok(cache) => cache.read_for_spec(spec_label.as_str()).unwrap_or_else(|e| {
            warn!(
                spec_label = %spec_label,
                error = %e,
                "loom todo: failed to read criterion evidence cache; rendering missing evidence",
            );
            Vec::new()
        }),
        Err(e) => {
            warn!(
                spec_label = %spec_label,
                error = %e,
                "loom todo: failed to open criterion evidence cache; rendering missing evidence",
            );
            Vec::new()
        }
    };
    let cache_by_id: BTreeMap<String, &CacheRow> = cache_rows
        .iter()
        .map(|row| (row.criterion_anchor.clone(), row))
        .collect();

    let next_lines: BTreeMap<u32, u32> = parsed
        .criteria
        .windows(2)
        .map(|pair| (pair[0].line, pair[1].line))
        .collect();

    let mut out: Vec<CriterionStatus> = Vec::new();
    for crit in &parsed.criteria {
        let Some(anns) = annotations_by_line.get(&crit.line) else {
            continue;
        };
        if anns.len() != 1 {
            continue;
        }
        let ann = anns[0];
        let criterion_text =
            criterion_text_for_line(&content, crit.line, next_lines.get(&crit.line).copied());
        let criterion_id = criterion_id_for(spec_label, &criterion_text);
        let annotation = annotation_from_parsed(ann);
        let evidence = match cache_by_id.get(criterion_id.as_str()).copied() {
            Some(row) => evidence_from_row(row, &annotation, git).await,
            None => EvidenceState::Missing,
        };
        out.push(CriterionStatus {
            spec_label: spec_label.clone(),
            criterion_id,
            criterion_text,
            annotation,
            evidence,
        });
    }
    out
}

pub fn criterion_id_for(spec_label: &SpecLabel, criterion_text: &str) -> CriterionId {
    CriterionId::for_spec_text(spec_label, criterion_text)
}

pub fn criterion_text_for_line(content: &str, line: u32, next_line: Option<u32>) -> String {
    loom_gate::annotation::criterion_text_for_line(content, line, next_line)
}

async fn evidence_from_row(
    row: &CacheRow,
    current_annotation: &CriterionAnnotation,
    git: &GitClient,
) -> EvidenceState {
    let commits_since = commits_since(git, &row.last_run_commit).await;
    let cached_annotation = annotation_from_cache_row(row);
    let Some(last_commit) = parse_cache_commit(&row.last_run_commit, row) else {
        return EvidenceState::Missing;
    };
    if &cached_annotation == current_annotation {
        EvidenceState::Current {
            result: verdict_to_result(row.verdict),
            last_timestamp_ms: row.last_run_ts_ms,
            last_commit,
            commits_since,
        }
    } else {
        EvidenceState::StaleAnnotation {
            cached_annotation,
            last_timestamp_ms: row.last_run_ts_ms,
            last_commit,
            commits_since,
        }
    }
}

fn parse_cache_commit(raw: &str, row: &CacheRow) -> Option<GitSha> {
    match GitSha::new(raw) {
        Ok(sha) => Some(sha),
        Err(err) => {
            warn!(
                spec_label = %row.spec_label,
                criterion_id = %row.criterion_anchor,
                error = %err,
                "loom todo: criterion evidence cache row has invalid commit; rendering missing evidence",
            );
            None
        }
    }
}

async fn commits_since(git: &GitClient, commit: &str) -> u32 {
    match git.commits_since(commit).await {
        Ok(n) => n,
        Err(err) => {
            warn!(commit, error = %err, "loom todo: failed to compute criterion evidence recency");
            0
        }
    }
}

fn annotation_from_parsed(ann: &Annotation) -> CriterionAnnotation {
    CriterionAnnotation {
        tier: tier_from_gate(ann.tier),
        target: AnnotationTarget::new(ann.target.clone()),
        pending: ann.pending,
    }
}

fn annotation_from_cache_row(row: &CacheRow) -> CriterionAnnotation {
    CriterionAnnotation {
        tier: tier_from_gate(row.tier),
        target: AnnotationTarget::new(row.annotation_target.clone()),
        pending: false,
    }
}

fn tier_from_gate(tier: Tier) -> AnnotationTier {
    match tier {
        Tier::Check => AnnotationTier::Check,
        Tier::Test => AnnotationTier::Test,
        Tier::System => AnnotationTier::System,
        Tier::Judge => AnnotationTier::Judge,
    }
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

    #[test]
    fn criterion_text_strips_bullet_and_annotation() {
        let content = "## Success Criteria\n\n- A criterion spans\n  continuation text\n  [test](crate::test_name)\n- Next criterion [check](cargo test)\n";
        assert_eq!(
            criterion_text_for_line(content, 3, Some(6)),
            "A criterion spans continuation text",
        );
        assert_eq!(criterion_text_for_line(content, 6, None), "Next criterion",);
    }

    #[test]
    fn criterion_id_ignores_annotation_text_changes() {
        let label = SpecLabel::new("templates");
        let a = criterion_id_for(&label, "A criterion");
        let b = criterion_id_for(&label, "A   criterion");
        assert_eq!(a, b);
    }

    #[test]
    fn criterion_id_matches_gate_cache_key_algorithm() {
        let label = SpecLabel::new("templates");
        let id = criterion_id_for(&label, "A criterion");
        assert_eq!(
            id.as_str(),
            loom_gate::annotation::criterion_id_for(&label, "A criterion"),
        );
    }
}

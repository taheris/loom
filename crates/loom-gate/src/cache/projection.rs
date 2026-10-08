//! Streaming disk projection without retaining or cloning every cached row.

use super::{
    CacheError, FailingCriterion, Report, StaleAnnotation, StaleRun, StatusCache, TierBuilder,
    TierSummary, Verdict, current_annotation_snapshots, parse_annotation_json, summarise_health,
    summarise_specs, tier_ord,
};
use crate::annotation::{ParsedSpecs, Tier};
use crate::integrity::IntegrityFinding;

pub(super) fn render(
    cache: &StatusCache,
    parsed: &ParsedSpecs,
    integrity: &[IntegrityFinding],
    now_ms: i64,
    stale_threshold_days: i64,
) -> Result<Report, CacheError> {
    let current = current_annotation_snapshots(parsed);
    let mut health = summarise_health(&[], parsed, integrity, now_ms, stale_threshold_days);
    let mut tiers: [Option<TierBuilder>; 4] = [None, None, None, None];
    let conn = cache.lock_conn()?;
    let mut statement = conn.prepare(
        "SELECT spec_label, criterion_id, annotation_json,
                last_timestamp_ms, last_commit, result, evidence
         FROM criterion_status ORDER BY spec_label, criterion_id",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let label = text(row, 0)?;
        let anchor = text(row, 1)?;
        let annotation = parse_annotation_json(text(row, 2)?).map_err(|source| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                Box::new(source),
            )
        })?;
        let timestamp: Option<i64> = row.get(3)?;
        optional_text(row, 4)?;
        let verdict = text(row, 5)?;
        let evidence = optional_text(row, 6)?.unwrap_or_default();
        let tier = Tier::from_wire(&annotation.tier).ok_or_else(|| CacheError::BadTier {
            row_key: format!("{label}/{anchor}"),
            tier: annotation.tier.into_owned(),
        })?;
        let verdict = Verdict::from_wire(verdict).ok_or_else(|| CacheError::BadVerdict {
            row_key: format!("{label}/{anchor}"),
            verdict: verdict.to_owned(),
        })?;
        let timestamp = timestamp.unwrap_or_default();
        let builder = tiers[tier_ord(tier)].get_or_insert_with(|| TierBuilder {
            tier,
            last_run_ts_ms: None,
            pass_count: 0,
            fail_count: 0,
            skipped_count: 0,
            failing: Vec::new(),
        });
        match verdict {
            Verdict::Pass => builder.pass_count += 1,
            Verdict::Skipped => builder.skipped_count += 1,
            Verdict::Fail => {
                builder.fail_count += 1;
                builder.failing.push(FailingCriterion {
                    spec_label: label.to_owned(),
                    criterion_anchor: anchor.to_owned(),
                    annotation_target: annotation.target.to_string(),
                    evidence: evidence.to_owned(),
                });
            }
        }
        builder.last_run_ts_ms = Some(
            builder
                .last_run_ts_ms
                .map_or(timestamp, |old| old.max(timestamp)),
        );
        if stale_threshold_days > 0
            && now_ms.saturating_sub(timestamp)
                > stale_threshold_days
                    .saturating_mul(86_400)
                    .saturating_mul(1000)
        {
            health.stale_runs.push(StaleRun {
                spec_label: label.to_owned(),
                criterion_anchor: anchor.to_owned(),
                last_run_ts_ms: timestamp,
            });
        }
        if !current.is_empty()
            && let Some((current_tier, current_target)) =
                current.get(&(label.to_owned(), anchor.to_owned()))
            && (*current_tier != tier || current_target != &annotation.target)
        {
            health.stale_annotations.push(StaleAnnotation {
                spec_label: label.to_owned(),
                criterion_id: anchor.to_owned(),
                cached_tier: tier,
                cached_target: annotation.target.into_owned(),
                current_tier: *current_tier,
                current_target: current_target.clone(),
            });
        }
    }
    drop(rows);
    drop(statement);
    drop(conn);
    Ok(Report {
        generated_at_ms: now_ms,
        stale_threshold_days,
        specs: summarise_specs(parsed),
        tiers: tiers
            .into_iter()
            .flatten()
            .map(|builder| TierSummary {
                tier: builder.tier,
                last_run_ts_ms: builder.last_run_ts_ms,
                pass_count: builder.pass_count,
                fail_count: builder.fail_count,
                skipped_count: builder.skipped_count,
                failing: builder.failing,
            })
            .collect(),
        annotation_health: health,
    })
}

fn text<'a>(row: &'a rusqlite::Row<'_>, column: usize) -> rusqlite::Result<&'a str> {
    let value = row.get_ref(column)?;
    value.as_str().map_err(|source| {
        rusqlite::Error::FromSqlConversionFailure(column, value.data_type(), Box::new(source))
    })
}

fn optional_text<'a>(
    row: &'a rusqlite::Row<'_>,
    column: usize,
) -> rusqlite::Result<Option<&'a str>> {
    if row.get_ref(column)? == rusqlite::types::ValueRef::Null {
        Ok(None)
    } else {
        text(row, column).map(Some)
    }
}

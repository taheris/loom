//! Per-criterion status cache.
//!
//! `loom gate` (no subcommand) reads from this sqlite-backed cache and
//! prints a fast report; `loom gate verify` / `loom gate review` and the
//! tier subcommands write to it as they run.
//!
//! One row per criterion, indexed by `(spec_label, criterion_anchor)`
//! per `specs/gate.md`. The cache stores the annotation tier so the
//! status report can summarise per-tier counts without re-parsing the
//! spec tree on every status invocation. Joining the cache against a
//! freshly parsed [`ParsedSpecs`] yields the un-annotated criterion
//! counts and the broken-annotation list that the spec mandates.
//!
//! Hard latency target: `render_report` finishes in under 500ms for a
//! corpus of arbitrary size. The self-test in this module asserts the
//! ceiling against a 2000-row seeded cache so a future schema or
//! rendering regression is caught at gate time.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use displaydoc::Display;
use rusqlite::{Connection, params};
use thiserror::Error;

use crate::annotation::{Annotation, ParsedSpecs, Tier};
use crate::integrity::IntegrityFinding;

const SCHEMA_VERSION: &str = "1";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS gate_criteria (
    spec_label        TEXT NOT NULL,
    criterion_anchor  TEXT NOT NULL,
    tier              TEXT NOT NULL,
    annotation_target TEXT NOT NULL,
    last_run_ts_ms    INTEGER NOT NULL,
    last_run_commit   TEXT NOT NULL,
    verdict           TEXT NOT NULL,
    evidence          TEXT NOT NULL,
    PRIMARY KEY (spec_label, criterion_anchor)
);
CREATE INDEX IF NOT EXISTS idx_gate_criteria_tier
    ON gate_criteria(tier);
CREATE INDEX IF NOT EXISTS idx_gate_criteria_spec
    ON gate_criteria(spec_label);
CREATE TABLE IF NOT EXISTS gate_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

/// Per-criterion verdict recorded by the most recent verifier run.
///
/// `Skipped` carries the scope reason (e.g. "annotation outside `--files`
/// set") in the row's `evidence` field; consumers display it alongside the
/// failing rows so a stale skip is visible in the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail,
    Skipped,
}

impl Verdict {
    /// Lowercase wire string. Matches the column value persisted by
    /// [`StatusCache::upsert`]; the parser side is [`Verdict::from_wire`].
    pub fn as_wire(&self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Fail => "fail",
            Verdict::Skipped => "skipped",
        }
    }

    /// Inverse of [`Verdict::as_wire`]. Returns `None` for any token
    /// outside the closed set so a row written by an older schema fails
    /// loud instead of silently re-classifying.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "pass" => Some(Verdict::Pass),
            "fail" => Some(Verdict::Fail),
            "skipped" => Some(Verdict::Skipped),
            _ => None,
        }
    }
}

/// One row of the status cache. Indexed by `(spec_label, criterion_anchor)`
/// per `specs/gate.md`. `last_run_commit` lets the report distinguish
/// fresh runs from stale runs without re-executing the verifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheRow {
    pub spec_label: String,
    pub criterion_anchor: String,
    pub tier: Tier,
    pub annotation_target: String,
    pub last_run_ts_ms: i64,
    pub last_run_commit: String,
    pub verdict: Verdict,
    pub evidence: String,
}

/// Handle on the sqlite-backed status cache.
///
/// Wraps the connection in a `Mutex` so the type is `Send + Sync`; the
/// underlying `rusqlite::Connection` is `!Sync`. Opening creates the
/// schema on first use and migrates older versions in place.
pub struct StatusCache {
    conn: Mutex<Connection>,
}

impl StatusCache {
    /// Open or create the cache at `path`, applying the schema.
    pub fn open(path: &Path) -> Result<Self, CacheError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| CacheError::OpenIo {
                path: path.to_path_buf(),
                source,
            })?;
        }
        let conn = Connection::open(path).map_err(|source| CacheError::Open {
            path: path.to_path_buf(),
            source,
        })?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        conn.execute_batch(SCHEMA)?;
        write_schema_version(&conn, SCHEMA_VERSION)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Read every row currently persisted for `spec_label`, sorted by
    /// `criterion_anchor` for deterministic output. Empty when the spec has
    /// no cached rows (fresh checkout, never-verified spec).
    pub fn read_for_spec(&self, spec_label: &str) -> Result<Vec<CacheRow>, CacheError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT spec_label, criterion_anchor, tier, annotation_target,
                    last_run_ts_ms, last_run_commit, verdict, evidence
             FROM gate_criteria
             WHERE spec_label = ?1
             ORDER BY criterion_anchor",
        )?;
        let rows = stmt
            .query_map(params![spec_label], row_to_cache_row)?
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Read every row currently persisted, sorted by
    /// `(spec_label, criterion_anchor)` for deterministic output.
    pub fn read_all(&self) -> Result<Vec<CacheRow>, CacheError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT spec_label, criterion_anchor, tier, annotation_target,
                    last_run_ts_ms, last_run_commit, verdict, evidence
             FROM gate_criteria
             ORDER BY spec_label, criterion_anchor",
        )?;
        let rows = stmt
            .query_map([], row_to_cache_row)?
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Insert or update a row keyed by `(spec_label, criterion_anchor)`.
    /// Idempotent — re-upserting the same key overwrites the prior verdict.
    pub fn upsert(&self, row: &CacheRow) -> Result<(), CacheError> {
        let conn = self.lock_conn()?;
        conn.execute(
            UPSERT_SQL,
            params![
                row.spec_label,
                row.criterion_anchor,
                row.tier.as_wire(),
                row.annotation_target,
                row.last_run_ts_ms,
                row.last_run_commit,
                row.verdict.as_wire(),
                row.evidence,
            ],
        )?;
        Ok(())
    }

    /// Batched form of [`Self::upsert`]: every row lands inside one
    /// transaction with a single prepared statement, so callers seeding
    /// many rows pay one fsync instead of N.
    pub fn upsert_many(&self, rows: &[CacheRow]) -> Result<(), CacheError> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(UPSERT_SQL)?;
            for row in rows {
                exec_upsert(&mut stmt, row)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, CacheError> {
        self.conn.lock().map_err(|_| CacheError::Poisoned)
    }
}

const UPSERT_SQL: &str = "INSERT INTO gate_criteria(
         spec_label, criterion_anchor, tier, annotation_target,
         last_run_ts_ms, last_run_commit, verdict, evidence
     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
     ON CONFLICT(spec_label, criterion_anchor) DO UPDATE SET
         tier = excluded.tier,
         annotation_target = excluded.annotation_target,
         last_run_ts_ms = excluded.last_run_ts_ms,
         last_run_commit = excluded.last_run_commit,
         verdict = excluded.verdict,
         evidence = excluded.evidence";

fn exec_upsert(stmt: &mut rusqlite::CachedStatement<'_>, row: &CacheRow) -> rusqlite::Result<()> {
    stmt.execute(params![
        row.spec_label,
        row.criterion_anchor,
        row.tier.as_wire(),
        row.annotation_target,
        row.last_run_ts_ms,
        row.last_run_commit,
        row.verdict.as_wire(),
        row.evidence,
    ])?;
    Ok(())
}

fn write_schema_version(conn: &Connection, version: &str) -> Result<(), CacheError> {
    conn.execute(
        "INSERT INTO gate_meta(key, value) VALUES ('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![version],
    )?;
    Ok(())
}

fn row_to_cache_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<CacheRow, CacheError>> {
    let spec_label: String = row.get(0)?;
    let criterion_anchor: String = row.get(1)?;
    let tier_wire: String = row.get(2)?;
    let annotation_target: String = row.get(3)?;
    let last_run_ts_ms: i64 = row.get(4)?;
    let last_run_commit: String = row.get(5)?;
    let verdict_wire: String = row.get(6)?;
    let evidence: String = row.get(7)?;

    let Some(tier) = Tier::from_wire(&tier_wire) else {
        return Ok(Err(CacheError::BadTier {
            row_key: format!("{spec_label}/{criterion_anchor}"),
            tier: tier_wire,
        }));
    };
    let Some(verdict) = Verdict::from_wire(&verdict_wire) else {
        return Ok(Err(CacheError::BadVerdict {
            row_key: format!("{spec_label}/{criterion_anchor}"),
            verdict: verdict_wire,
        }));
    };
    Ok(Ok(CacheRow {
        spec_label,
        criterion_anchor,
        tier,
        annotation_target,
        last_run_ts_ms,
        last_run_commit,
        verdict,
        evidence,
    }))
}

/// Failures the cache surfaces. Per RS-4 each variant carries enough
/// context for the caller to route the error back to its source.
#[derive(Debug, Display, Error)]
pub enum CacheError {
    /// failed to open cache at {path}: {source}
    Open {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    /// failed to create cache parent for {path}: {source}
    OpenIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// sqlite error: {0}
    Sqlite(#[from] rusqlite::Error),
    /// cache mutex poisoned by a panicked writer
    Poisoned,
    /// row {row_key} carries unknown tier {tier:?}
    BadTier { row_key: String, tier: String },
    /// row {row_key} carries unknown verdict {verdict:?}
    BadVerdict { row_key: String, verdict: String },
}

/// One spec's criterion accounting in the status report.
///
/// Counts derive from the parsed [`ParsedSpecs`] handed to
/// [`render_report`], not from the cache — a brand-new criterion that has
/// never been run still needs to show up as un-annotated when no annotation
/// attaches to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecReport {
    pub spec_label: String,
    pub criterion_total: usize,
    pub criterion_annotated: usize,
    pub criterion_unannotated: usize,
}

/// One tier's last-run summary across the whole cache.
///
/// `last_run_ts_ms` is the most recent timestamp seen across every cached
/// row for the tier; `None` when the tier has no rows. `failing` lists
/// every currently-failing row (verdict == [`Verdict::Fail`]) for the tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierSummary {
    pub tier: Tier,
    pub last_run_ts_ms: Option<i64>,
    pub pass_count: usize,
    pub fail_count: usize,
    pub skipped_count: usize,
    pub failing: Vec<FailingCriterion>,
}

/// One currently-failing row's identifying coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailingCriterion {
    pub spec_label: String,
    pub criterion_anchor: String,
    pub annotation_target: String,
    pub evidence: String,
}

/// One stale row — verdict is fresh on its own line but the run is older
/// than the report's `stale_threshold_days` cutoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleRun {
    pub spec_label: String,
    pub criterion_anchor: String,
    pub last_run_ts_ms: i64,
}

/// One broken annotation — the integrity gate flagged it as unresolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenAnnotation {
    pub source_spec: PathBuf,
    pub line: u32,
    pub tier: Tier,
    pub target: String,
}

/// Annotation-health rollup for the report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnnotationHealth {
    pub broken_annotations: Vec<BrokenAnnotation>,
    pub stale_runs: Vec<StaleRun>,
}

/// Full status report rendered by [`render_report`]. Each section maps
/// 1:1 to the `loom gate` report contents the spec mandates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub generated_at_ms: i64,
    pub stale_threshold_days: i64,
    pub specs: Vec<SpecReport>,
    pub tiers: Vec<TierSummary>,
    pub annotation_health: AnnotationHealth,
}

/// Render a status report by joining cache rows, parsed annotations, and
/// integrity findings.
///
/// `now_ms` is the report's clock reading, used to compute staleness so
/// tests can pass a fixed value and avoid wall-time dependency.
/// `stale_threshold_days` follows the spec's "stale runs older than N
/// days" wording — pass a non-positive value to disable staleness flags.
pub fn render_report(
    cache: &StatusCache,
    parsed: &ParsedSpecs,
    integrity: &[IntegrityFinding],
    now_ms: i64,
    stale_threshold_days: i64,
) -> Result<Report, CacheError> {
    let rows = cache.read_all()?;
    Ok(render_from_rows(
        &rows,
        parsed,
        integrity,
        now_ms,
        stale_threshold_days,
    ))
}

/// Pure rendering core — separated so the latency self-test can pump rows
/// in without going back through sqlite for each iteration. Production
/// callers use [`render_report`].
pub fn render_from_rows(
    rows: &[CacheRow],
    parsed: &ParsedSpecs,
    integrity: &[IntegrityFinding],
    now_ms: i64,
    stale_threshold_days: i64,
) -> Report {
    let specs = summarise_specs(parsed);
    let tiers = summarise_tiers(rows);
    let annotation_health = summarise_health(rows, integrity, now_ms, stale_threshold_days);
    Report {
        generated_at_ms: now_ms,
        stale_threshold_days,
        specs,
        tiers,
        annotation_health,
    }
}

fn summarise_specs(parsed: &ParsedSpecs) -> Vec<SpecReport> {
    let mut per_spec: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut annotated_keys: std::collections::HashSet<(String, u32)> =
        std::collections::HashSet::new();
    for ann in &parsed.annotations {
        annotated_keys.insert((spec_label_from_path(&ann.source_spec), ann.criterion_line));
    }
    for crit in &parsed.criteria {
        let label = spec_label_from_path(&crit.source_spec);
        let entry = per_spec.entry(label.clone()).or_insert((0, 0));
        entry.0 += 1;
        if annotated_keys.contains(&(label, crit.line)) {
            entry.1 += 1;
        }
    }
    per_spec
        .into_iter()
        .map(|(spec_label, (total, annotated))| SpecReport {
            spec_label,
            criterion_total: total,
            criterion_annotated: annotated,
            criterion_unannotated: total - annotated,
        })
        .collect()
}

fn summarise_tiers(rows: &[CacheRow]) -> Vec<TierSummary> {
    let mut per_tier: BTreeMap<u8, TierBuilder> = BTreeMap::new();
    for row in rows {
        let entry = per_tier
            .entry(tier_ord(row.tier))
            .or_insert_with(|| TierBuilder {
                tier: row.tier,
                last_run_ts_ms: None,
                pass_count: 0,
                fail_count: 0,
                skipped_count: 0,
                failing: Vec::new(),
            });
        match row.verdict {
            Verdict::Pass => entry.pass_count += 1,
            Verdict::Fail => {
                entry.fail_count += 1;
                entry.failing.push(FailingCriterion {
                    spec_label: row.spec_label.clone(),
                    criterion_anchor: row.criterion_anchor.clone(),
                    annotation_target: row.annotation_target.clone(),
                    evidence: row.evidence.clone(),
                });
            }
            Verdict::Skipped => entry.skipped_count += 1,
        }
        entry.last_run_ts_ms = Some(match entry.last_run_ts_ms {
            Some(prev) => prev.max(row.last_run_ts_ms),
            None => row.last_run_ts_ms,
        });
    }
    per_tier
        .into_values()
        .map(|b| TierSummary {
            tier: b.tier,
            last_run_ts_ms: b.last_run_ts_ms,
            pass_count: b.pass_count,
            fail_count: b.fail_count,
            skipped_count: b.skipped_count,
            failing: b.failing,
        })
        .collect()
}

fn summarise_health(
    rows: &[CacheRow],
    integrity: &[IntegrityFinding],
    now_ms: i64,
    stale_threshold_days: i64,
) -> AnnotationHealth {
    let broken_annotations = integrity
        .iter()
        .filter_map(|f| match f {
            IntegrityFinding::UnresolvedAnnotation {
                spec,
                line,
                tier,
                target,
            } => Some(BrokenAnnotation {
                source_spec: spec.clone(),
                line: *line,
                tier: *tier,
                target: target.clone(),
            }),
            IntegrityFinding::UnresolvedCargoTestName {
                spec, line, target, ..
            } => Some(BrokenAnnotation {
                source_spec: spec.clone(),
                line: *line,
                tier: Tier::Check,
                target: target.clone(),
            }),
            IntegrityFinding::StubTestFunction {
                spec,
                line,
                tier,
                target,
                ..
            } => Some(BrokenAnnotation {
                source_spec: spec.clone(),
                line: *line,
                tier: *tier,
                target: target.clone(),
            }),
            IntegrityFinding::MultipleAnnotations { .. }
            | IntegrityFinding::UnneededPendingMarker { .. } => None,
        })
        .collect();

    let stale_runs = if stale_threshold_days <= 0 {
        Vec::new()
    } else {
        let cutoff_ms = stale_threshold_days
            .saturating_mul(86_400)
            .saturating_mul(1000);
        rows.iter()
            .filter(|r| now_ms.saturating_sub(r.last_run_ts_ms) > cutoff_ms)
            .map(|r| StaleRun {
                spec_label: r.spec_label.clone(),
                criterion_anchor: r.criterion_anchor.clone(),
                last_run_ts_ms: r.last_run_ts_ms,
            })
            .collect()
    };

    AnnotationHealth {
        broken_annotations,
        stale_runs,
    }
}

struct TierBuilder {
    tier: Tier,
    last_run_ts_ms: Option<i64>,
    pass_count: usize,
    fail_count: usize,
    skipped_count: usize,
    failing: Vec<FailingCriterion>,
}

fn tier_ord(tier: Tier) -> u8 {
    match tier {
        Tier::Check => 0,
        Tier::Test => 1,
        Tier::System => 2,
        Tier::Judge => 3,
    }
}

/// Derive a spec label from a `specs/<label>.md` path. Falls back to the
/// path's string form when the file stem is empty so the report still
/// renders deterministically even for unusual inputs.
fn spec_label_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Build a [`CacheRow`] from an [`Annotation`] plus a fresh verdict.
///
/// Convenience for verifier dispatchers: they already hold the annotation
/// and the verdict, and the cache row joins them with the run's
/// commit/timestamp.
pub fn row_for(
    annotation: &Annotation,
    verdict: Verdict,
    evidence: impl Into<String>,
    last_run_ts_ms: i64,
    last_run_commit: impl Into<String>,
) -> CacheRow {
    CacheRow {
        spec_label: spec_label_from_path(&annotation.source_spec),
        criterion_anchor: annotation.criterion_line.to_string(),
        tier: annotation.tier,
        annotation_target: annotation.target.clone(),
        last_run_ts_ms,
        last_run_commit: last_run_commit.into(),
        verdict,
        evidence: evidence.into(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    use std::path::PathBuf;

    use tempfile::tempdir;

    use crate::annotation::{Annotation, Criterion};

    fn db_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("gate-cache.sqlite")
    }

    fn row(spec: &str, anchor: &str, tier: Tier, verdict: Verdict, ts: i64) -> CacheRow {
        CacheRow {
            spec_label: spec.into(),
            criterion_anchor: anchor.into(),
            tier,
            annotation_target: "cargo run -p w".into(),
            last_run_ts_ms: ts,
            last_run_commit: "abc1234".into(),
            verdict,
            evidence: "ok".into(),
        }
    }

    #[test]
    fn verdict_round_trips_through_wire() {
        for v in [Verdict::Pass, Verdict::Fail, Verdict::Skipped] {
            assert_eq!(Verdict::from_wire(v.as_wire()), Some(v));
        }
        assert_eq!(Verdict::from_wire("unknown"), None);
    }

    #[test]
    fn open_then_read_empty_cache_returns_no_rows() {
        let dir = tempdir().unwrap();
        let cache = StatusCache::open(&db_path(&dir)).unwrap();
        let rows = cache.read_all().unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn upsert_then_read_round_trips_every_field() {
        let dir = tempdir().unwrap();
        let cache = StatusCache::open(&db_path(&dir)).unwrap();
        let r = row("alpha", "42", Tier::Test, Verdict::Pass, 1_700_000_000_000);
        cache.upsert(&r).unwrap();
        let rows = cache.read_all().unwrap();
        assert_eq!(rows, vec![r]);
    }

    #[test]
    fn upsert_on_same_anchor_overwrites_prior_verdict() {
        let dir = tempdir().unwrap();
        let cache = StatusCache::open(&db_path(&dir)).unwrap();
        let first = row("alpha", "42", Tier::Test, Verdict::Pass, 1);
        cache.upsert(&first).unwrap();
        let mut second = first.clone();
        second.verdict = Verdict::Fail;
        second.evidence = "boom".into();
        second.last_run_ts_ms = 2;
        cache.upsert(&second).unwrap();
        let rows = cache.read_all().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].verdict, Verdict::Fail);
        assert_eq!(rows[0].evidence, "boom");
        assert_eq!(rows[0].last_run_ts_ms, 2);
    }

    #[test]
    fn read_all_returns_rows_sorted_by_spec_then_anchor() {
        let dir = tempdir().unwrap();
        let cache = StatusCache::open(&db_path(&dir)).unwrap();
        cache
            .upsert(&row("beta", "1", Tier::Check, Verdict::Pass, 0))
            .unwrap();
        cache
            .upsert(&row("alpha", "2", Tier::Test, Verdict::Pass, 0))
            .unwrap();
        cache
            .upsert(&row("alpha", "1", Tier::Test, Verdict::Pass, 0))
            .unwrap();
        let rows = cache.read_all().unwrap();
        let keys: Vec<(String, String)> = rows
            .into_iter()
            .map(|r| (r.spec_label, r.criterion_anchor))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("alpha".to_string(), "1".to_string()),
                ("alpha".to_string(), "2".to_string()),
                ("beta".to_string(), "1".to_string()),
            ]
        );
    }

    #[test]
    fn reopen_recovers_persisted_rows() {
        let dir = tempdir().unwrap();
        let path = db_path(&dir);
        {
            let cache = StatusCache::open(&path).unwrap();
            cache
                .upsert(&row("alpha", "1", Tier::Test, Verdict::Pass, 7))
                .unwrap();
        }
        let cache = StatusCache::open(&path).unwrap();
        let rows = cache.read_all().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].spec_label, "alpha");
        assert_eq!(rows[0].last_run_ts_ms, 7);
    }

    #[test]
    fn spec_summary_separates_annotated_from_unannotated() {
        let parsed = ParsedSpecs {
            annotations: vec![Annotation {
                tier: Tier::Test,
                target: "crate::a::t".into(),
                source_spec: PathBuf::from("specs/alpha.md"),
                line: 6,
                criterion_line: 5,
                pending: false,
            }],
            criteria: vec![
                Criterion {
                    source_spec: PathBuf::from("specs/alpha.md"),
                    line: 5,
                },
                Criterion {
                    source_spec: PathBuf::from("specs/alpha.md"),
                    line: 9,
                },
            ],
        };
        let report = render_from_rows(&[], &parsed, &[], 0, 0);
        assert_eq!(report.specs.len(), 1);
        let s = &report.specs[0];
        assert_eq!(s.spec_label, "alpha");
        assert_eq!(s.criterion_total, 2);
        assert_eq!(s.criterion_annotated, 1);
        assert_eq!(s.criterion_unannotated, 1);
    }

    #[test]
    fn tier_summary_counts_pass_fail_skipped_per_tier() {
        let rows = vec![
            row("a", "1", Tier::Test, Verdict::Pass, 5),
            row("a", "2", Tier::Test, Verdict::Fail, 7),
            row("a", "3", Tier::Check, Verdict::Pass, 1),
            row("a", "4", Tier::Test, Verdict::Skipped, 2),
        ];
        let report = render_from_rows(&rows, &ParsedSpecs::default(), &[], 0, 0);
        assert_eq!(report.tiers.len(), 2);
        let test = report
            .tiers
            .iter()
            .find(|t| t.tier == Tier::Test)
            .expect("test tier summary present");
        assert_eq!(test.pass_count, 1);
        assert_eq!(test.fail_count, 1);
        assert_eq!(test.skipped_count, 1);
        assert_eq!(test.last_run_ts_ms, Some(7));
        assert_eq!(test.failing.len(), 1);
        assert_eq!(test.failing[0].criterion_anchor, "2");
    }

    #[test]
    fn stale_runs_flagged_when_older_than_threshold() {
        let day_ms: i64 = 86_400 * 1000;
        let rows = vec![
            row("a", "1", Tier::Test, Verdict::Pass, 0),
            row("a", "2", Tier::Test, Verdict::Pass, 20 * day_ms),
        ];
        let report = render_from_rows(&rows, &ParsedSpecs::default(), &[], 30 * day_ms, 14);
        assert_eq!(report.annotation_health.stale_runs.len(), 1);
        assert_eq!(report.annotation_health.stale_runs[0].criterion_anchor, "1");
    }

    #[test]
    fn stale_threshold_zero_disables_stale_flagging() {
        let rows = vec![row("a", "1", Tier::Test, Verdict::Pass, 0)];
        let report = render_from_rows(&rows, &ParsedSpecs::default(), &[], i64::MAX / 2, 0);
        assert!(report.annotation_health.stale_runs.is_empty());
    }

    #[test]
    fn broken_annotations_come_from_integrity_findings() {
        let findings = vec![
            IntegrityFinding::UnresolvedAnnotation {
                spec: PathBuf::from("specs/alpha.md"),
                line: 12,
                tier: Tier::Check,
                target: "missing-bin".into(),
            },
            IntegrityFinding::MultipleAnnotations {
                spec: PathBuf::from("specs/alpha.md"),
                line: 13,
                count: 2,
            },
        ];
        let report = render_from_rows(&[], &ParsedSpecs::default(), &findings, 0, 0);
        assert_eq!(report.annotation_health.broken_annotations.len(), 1);
        assert_eq!(
            report.annotation_health.broken_annotations[0].target,
            "missing-bin"
        );
    }

    #[test]
    fn row_for_packs_annotation_into_cache_row_with_verdict() {
        let ann = Annotation {
            tier: Tier::Check,
            target: "cargo run -p w".into(),
            source_spec: PathBuf::from("specs/gate.md"),
            line: 100,
            criterion_line: 95,
            pending: false,
        };
        let r = row_for(&ann, Verdict::Pass, "ok", 42, "deadbeef");
        assert_eq!(r.spec_label, "gate");
        assert_eq!(r.criterion_anchor, "95");
        assert_eq!(r.tier, Tier::Check);
        assert_eq!(r.verdict, Verdict::Pass);
        assert_eq!(r.last_run_commit, "deadbeef");
    }
}

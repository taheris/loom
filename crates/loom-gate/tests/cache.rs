#![allow(clippy::unwrap_used)]
//! Integration coverage for the status cache.
//!
//! Round-trip through real on-disk sqlite (never `:memory:`, per
//! `specs/tests.md` test patterns) and assert the hard <500ms
//! latency ceiling on the `render_report` path against a 2000-row seed.

use std::path::PathBuf;
use std::time::Instant;

use loom_driver::identifier::SpecLabel;
use loom_driver::state::CacheDb;
use loom_gate::annotation::{Annotation, Criterion, ParsedSpecs, Tier, criterion_id_for};
use loom_gate::cache::{CacheRow, StatusCache, Verdict, render_from_rows, render_report, row_for};
use loom_gate::integrity::IntegrityFinding;
use tempfile::tempdir;

fn cache_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("cache.db")
}

fn cache_row(spec: &str, anchor: &str, tier: Tier, verdict: Verdict) -> CacheRow {
    CacheRow {
        spec_label: spec.into(),
        criterion_anchor: anchor.into(),
        tier,
        annotation_target: format!("cargo run -p {spec}"),
        last_run_ts_ms: 1_700_000_000_000,
        last_run_commit: "abc1234".into(),
        verdict,
        evidence: "evidence body".into(),
    }
}

#[test]
fn open_creates_db_file_when_missing() {
    let dir = tempdir().unwrap();
    let path = cache_path(&dir);
    assert!(!path.exists());
    let _cache = StatusCache::open(&path).unwrap();
    assert!(path.exists(), "open() materialised the on-disk file");
}

#[test]
fn status_cache_open_preserves_unified_cache_db_schema_version() {
    let dir = tempdir().unwrap();
    let path = cache_path(&dir);
    let _cache_db = CacheDb::open(&path).unwrap();

    let _status_cache = StatusCache::open(&path).unwrap();
    let reopened = CacheDb::open(&path).unwrap();

    assert!(reopened.work_epics().unwrap().is_empty());
}

#[test]
fn round_trip_through_sqlite_preserves_every_field() {
    let dir = tempdir().unwrap();
    let cache = StatusCache::open(&cache_path(&dir)).unwrap();
    let row = CacheRow {
        spec_label: "gate".into(),
        criterion_anchor: "120".into(),
        tier: Tier::Test,
        annotation_target: "crate::cache::tests::round_trip".into(),
        last_run_ts_ms: 42,
        last_run_commit: "abcdef0".into(),
        verdict: Verdict::Pass,
        evidence: "captured stdout".into(),
    };
    cache.upsert(&row).unwrap();
    let read = cache.read_all().unwrap();
    assert_eq!(read, vec![row]);
}

#[test]
fn render_report_reads_from_disk_and_summarises_per_tier() {
    let dir = tempdir().unwrap();
    let cache = StatusCache::open(&cache_path(&dir)).unwrap();
    cache
        .upsert(&cache_row("gate", "10", Tier::Test, Verdict::Pass))
        .unwrap();
    cache
        .upsert(&cache_row("gate", "20", Tier::Test, Verdict::Fail))
        .unwrap();
    cache
        .upsert(&cache_row("gate", "30", Tier::Check, Verdict::Pass))
        .unwrap();

    let parsed = ParsedSpecs {
        annotations: vec![Annotation {
            tier: Tier::Test,
            target: "crate::a::t".into(),
            source_spec: PathBuf::from("specs/gate.md"),
            line: 11,
            criterion_line: 10,
            pending: false,
        }],
        criteria: vec![
            Criterion {
                source_spec: PathBuf::from("specs/gate.md"),
                line: 10,
                text: "first".into(),
            },
            Criterion {
                source_spec: PathBuf::from("specs/gate.md"),
                line: 50,
                text: "second".into(),
            },
        ],
    };
    let report = render_report(&cache, &parsed, &[], 1_700_000_000_000, 0).unwrap();

    assert_eq!(report.specs.len(), 1);
    assert_eq!(report.specs[0].criterion_total, 2);
    assert_eq!(report.specs[0].criterion_annotated, 1);
    assert_eq!(report.specs[0].criterion_unannotated, 1);

    let test = report
        .tiers
        .iter()
        .find(|t| t.tier == Tier::Test)
        .expect("test tier");
    assert_eq!(test.pass_count, 1);
    assert_eq!(test.fail_count, 1);
    assert_eq!(test.failing.len(), 1);
    assert_eq!(test.failing[0].criterion_anchor, "20");
}

#[test]
fn row_for_helper_writes_round_trip_row() {
    let dir = tempdir().unwrap();
    let cache = StatusCache::open(&cache_path(&dir)).unwrap();
    let ann = Annotation {
        tier: Tier::Judge,
        target: "rubrics/api.md".into(),
        source_spec: PathBuf::from("specs/gate.md"),
        line: 200,
        criterion_line: 195,
        pending: false,
    };
    let row = row_for(&ann, Verdict::Pass, "judge ok", 99, "feedbac");
    cache.upsert(&row).unwrap();
    let rows = cache.read_all().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].spec_label, "gate");
    assert_eq!(rows[0].tier, Tier::Judge);
    assert_eq!(rows[0].criterion_anchor, "195");
}

#[test]
fn render_under_500ms_on_2000_row_corpus() {
    // Hard target from specs/gate.md status-cache section: report
    // renders in <500ms on a corpus of arbitrary size. The 2000-row seed
    // is two orders of magnitude past the realistic per-spec criterion
    // count, so this regresses if the *cache impl* drops to linear-scan
    // territory or starts re-querying inside the render loop.
    let dir = tempdir().unwrap();
    let cache = StatusCache::open(&cache_path(&dir)).unwrap();
    let rows: Vec<CacheRow> = (0..2000)
        .map(|i| CacheRow {
            spec_label: format!("spec-{}", i % 16),
            criterion_anchor: format!("{i}"),
            tier: match i % 4 {
                0 => Tier::Check,
                1 => Tier::Test,
                2 => Tier::System,
                _ => Tier::Judge,
            },
            annotation_target: format!("target-{i}"),
            last_run_ts_ms: i as i64,
            last_run_commit: "deadbeef".into(),
            verdict: match i % 3 {
                0 => Verdict::Pass,
                1 => Verdict::Fail,
                _ => Verdict::Skipped,
            },
            evidence: "ev".into(),
        })
        .collect();
    cache.upsert_many(&rows).unwrap();

    let parsed = ParsedSpecs::default();
    let started = Instant::now();
    let report = render_report(&cache, &parsed, &[], i64::MAX / 2, 0).unwrap();
    let elapsed = started.elapsed();

    assert!(
        elapsed.as_millis() < 500,
        "render_report took {elapsed:?} (>500ms hard ceiling)"
    );
    assert_eq!(report.tiers.len(), 4);
    let total: usize = report
        .tiers
        .iter()
        .map(|t| t.pass_count + t.fail_count + t.skipped_count)
        .sum();
    assert_eq!(total, 2000);
}

#[test]
fn render_from_rows_under_500ms_on_2000_row_corpus() {
    // Same target, exercised on the pure-render path so a regression in
    // the rendering core (group-by-tier, group-by-spec) surfaces even
    // when sqlite IO is held constant.
    let rows: Vec<CacheRow> = (0..2000)
        .map(|i| CacheRow {
            spec_label: format!("spec-{}", i % 16),
            criterion_anchor: format!("{i}"),
            tier: match i % 4 {
                0 => Tier::Check,
                1 => Tier::Test,
                2 => Tier::System,
                _ => Tier::Judge,
            },
            annotation_target: format!("t-{i}"),
            last_run_ts_ms: i as i64,
            last_run_commit: "c".into(),
            verdict: match i % 3 {
                0 => Verdict::Pass,
                1 => Verdict::Fail,
                _ => Verdict::Skipped,
            },
            evidence: "e".into(),
        })
        .collect();

    let parsed = ParsedSpecs::default();
    let started = Instant::now();
    let _report = render_from_rows(&rows, &parsed, &[], 0, 0);
    let elapsed = started.elapsed();
    assert!(
        elapsed.as_millis() < 500,
        "render_from_rows took {elapsed:?} (>500ms hard ceiling)"
    );
}

#[test]
fn broken_annotations_in_report_come_from_integrity_findings() {
    let dir = tempdir().unwrap();
    let cache = StatusCache::open(&cache_path(&dir)).unwrap();
    let findings = vec![IntegrityFinding::UnresolvedAnnotation {
        spec: PathBuf::from("specs/alpha.md"),
        line: 7,
        tier: Tier::Check,
        target: "missing-bin".into(),
    }];
    let report = render_report(&cache, &ParsedSpecs::default(), &findings, 0, 0).unwrap();
    assert_eq!(report.annotation_health.broken_annotations.len(), 1);
    assert_eq!(
        report.annotation_health.broken_annotations[0].target,
        "missing-bin"
    );
}

#[test]
fn stale_annotations_join_typed_criterion_ids_against_current_snapshot() {
    let dir = tempdir().unwrap();
    let cache = StatusCache::open(&cache_path(&dir)).unwrap();
    let spec_label = SpecLabel::new("gate");
    let criterion_text = "Verifier evidence is cached";
    let criterion_id = criterion_id_for(&spec_label, criterion_text);
    cache
        .upsert(&CacheRow {
            spec_label: "gate".into(),
            criterion_anchor: criterion_id.clone(),
            tier: Tier::Check,
            annotation_target: "cargo run -p old-walk".into(),
            last_run_ts_ms: 1,
            last_run_commit: "abc1234".into(),
            verdict: Verdict::Pass,
            evidence: "old pass".into(),
        })
        .unwrap();
    let parsed = ParsedSpecs {
        annotations: vec![Annotation {
            tier: Tier::Test,
            target: "crate::new_test".into(),
            source_spec: PathBuf::from("specs/gate.md"),
            line: 12,
            criterion_line: 10,
            pending: false,
        }],
        criteria: vec![Criterion {
            source_spec: PathBuf::from("specs/gate.md"),
            line: 10,
            text: criterion_text.into(),
        }],
    };

    let report = render_report(&cache, &parsed, &[], 0, 0).unwrap();

    assert_eq!(report.annotation_health.stale_annotations.len(), 1);
    let stale = &report.annotation_health.stale_annotations[0];
    assert_eq!(stale.spec_label, "gate");
    assert_eq!(stale.criterion_id, criterion_id);
    assert_eq!(stale.cached_tier, Tier::Check);
    assert_eq!(stale.current_tier, Tier::Test);
}

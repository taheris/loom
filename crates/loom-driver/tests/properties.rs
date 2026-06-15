//! Property-based tests for `loom-driver` invariants.
//!
//! Per `specs/tests.md` (Architecture / Property-Based Testing), this
//! crate owns invariants for the types it defines. The cache DB is the
//! sole proptest target: arbitrary spec-file content never corrupts the
//! schema, a corrupted DB always recovers via `recreate`, and round-trips
//! through known shapes are stable.
//!
//! `PROPTEST_CASES=32` under `nix flake check`; local exhaustive runs
//! override via env var (`PROPTEST_CASES=2048+`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use loom_driver::identifier::{MoleculeId, SpecLabel};
use loom_driver::state::{ActiveMolecule, CacheDb};
use loom_test_support::CI_PROPTEST_CASES;
use proptest::prelude::*;

/// Acceptable label characters: ASCII letters, digits, dash, underscore.
/// Restricting the alphabet keeps spec files writable to the tempdir on
/// every platform — arbitrary unicode would explode the case count without
/// adding signal.
fn label_strategy() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_-]{0,15}".prop_filter("non-empty", |s| !s.is_empty())
}

/// Spec-file body — arbitrary printable bytes, with `## Companions` headings
/// occasionally injected. Tests that the rebuild path tolerates whatever the
/// filesystem hands it.
fn spec_body_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        // Bare body — no companions section.
        ".{0,200}",
        // Companions section with arbitrary bullet content.
        ("(- `[a-z0-9/_.-]{1,30}`\n){0,5}").prop_map(|bullets| format!(
            "# Spec\n\n## Companions\n\n{bullets}\n## Other\n\nbody\n"
        )),
        // Mixed garbage that may include the heading at unexpected
        // positions — stresses the parser without crashing rebuild.
        ".{0,400}",
    ]
}

fn list_tables(db_path: &std::path::Path) -> Vec<String> {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

fn schema_intact(db_path: &std::path::Path) -> bool {
    let names = list_tables(db_path);
    [
        "companions",
        "criterion_status",
        "meta",
        "spec_epics",
        "specs",
        "work_epics",
    ]
    .iter()
    .all(|expected| names.iter().any(|n| n == expected))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CI_PROPTEST_CASES))]

    /// Arbitrary spec-file content never panics rebuild and never corrupts
    /// the schema: after `rebuild`, every required table still exists and
    /// the connection stays usable for a follow-up query.
    #[test]
    fn rebuild_never_corrupts_schema(
        labels in proptest::collection::vec(label_strategy(), 0..4),
        bodies in proptest::collection::vec(spec_body_strategy(), 0..4),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path();
        let specs_dir = workspace.join("specs");
        std::fs::create_dir_all(&specs_dir).unwrap();

        // Pair labels with bodies; duplicate labels collapse via filename.
        let pairs: Vec<(String, String)> = labels
            .into_iter()
            .zip(bodies.into_iter().chain(std::iter::repeat_with(String::new)))
            .collect();
        for (label, body) in &pairs {
            let path = specs_dir.join(format!("{label}.md"));
            std::fs::write(&path, body).unwrap();
        }

        let db_path = workspace.join(".loom/cache.db");
        let db = CacheDb::open(&db_path).unwrap();

        // Rebuild with no molecules — exercises the spec-walking codepath
        // most likely to hit the parser on arbitrary bodies.
        let report = db.rebuild(workspace, &[]).unwrap();
        prop_assert!(report.specs <= pairs.len());
        prop_assert!(schema_intact(&db_path));

        // Connection is still queryable after rebuild.
        for (label, _) in &pairs {
            let _ = db.spec(&SpecLabel::new(label.clone()));
        }
    }

    /// Corrupted DB always recovers via `recreate`. Garbage in the file
    /// must never wedge the state subsystem: `recreate` deletes and
    /// re-opens, then `rebuild` populates from a fresh workspace.
    #[test]
    fn recreate_recovers_from_arbitrary_bytes(
        garbage in proptest::collection::vec(any::<u8>(), 0..2048),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("cache.db");
        std::fs::write(&db_path, &garbage).unwrap();

        let db = CacheDb::recreate(&db_path).unwrap();
        prop_assert!(schema_intact(&db_path));

        // A trivial rebuild + query still succeeds — the file is usable.
        let report = db.rebuild(dir.path(), &[]).unwrap();
        prop_assert_eq!(report.specs, 0);
    }

    /// Round-trip identity for known shapes: every active molecule paired
    /// with a spec file survives `rebuild` and re-emerges via
    /// `active_molecule`.
    #[test]
    fn rebuild_round_trips_known_shapes(
        labels in proptest::collection::vec(label_strategy(), 1..4),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path();
        let specs_dir = workspace.join("specs");
        std::fs::create_dir_all(&specs_dir).unwrap();

        // Deduplicate labels — the filesystem collapses dupes anyway, but
        // the molecules vector below would carry phantom entries otherwise.
        let mut unique: Vec<String> = labels;
        unique.sort();
        unique.dedup();

        for label in &unique {
            let path = specs_dir.join(format!("{label}.md"));
            std::fs::write(&path, "# spec\n").unwrap();
        }

        let molecules: Vec<ActiveMolecule> = unique
            .iter()
            .enumerate()
            .map(|(i, label)| ActiveMolecule {
                id: MoleculeId::new(format!("lm-{i}")),
                spec_label: SpecLabel::new(label.clone()),
                base_commit: Some(format!("commit-{i}")),
            })
            .collect();

        let db_path = workspace.join(".loom/cache.db");
        let db = CacheDb::open(&db_path).unwrap();
        let report = db.rebuild(workspace, &molecules).unwrap();
        prop_assert_eq!(report.specs, unique.len());
        prop_assert_eq!(report.work_epics, molecules.len());

        for mol in &molecules {
            let row = db.molecule_for_spec(&mol.spec_label).unwrap()
                .expect("molecule should round-trip");
            prop_assert_eq!(row.id.as_str(), mol.id.as_str());
            prop_assert_eq!(&row.base_commit, &mol.base_commit);
            prop_assert_eq!(row.iteration_count, 0);
        }
    }
}

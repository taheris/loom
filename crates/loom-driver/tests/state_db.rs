//! Integration tests for `loom_driver::state::StateDb`.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use loom_driver::bd::BdError;
use loom_driver::identifier::{MoleculeId, SpecLabel};
use loom_driver::state::{ActiveMolecule, BdUpdateFn, StateDb, StateError};

fn write_spec(workspace: &Path, label: &str, body: &str) -> Result<()> {
    let specs = workspace.join("specs");
    std::fs::create_dir_all(&specs)?;
    std::fs::write(specs.join(format!("{label}.md")), body)?;
    Ok(())
}

fn list_table(db_path: &Path, sql: &str) -> Result<Vec<Vec<String>>> {
    let conn = rusqlite::Connection::open(db_path)?;
    let mut stmt = conn.prepare(sql)?;
    let cols = stmt.column_count();
    let rows: Vec<Vec<String>> = stmt
        .query_map([], |row| {
            (0..cols)
                .map(|i| {
                    let v: rusqlite::types::Value = row.get(i)?;
                    Ok(match v {
                        rusqlite::types::Value::Null => String::from("NULL"),
                        rusqlite::types::Value::Integer(i) => i.to_string(),
                        rusqlite::types::Value::Real(r) => r.to_string(),
                        rusqlite::types::Value::Text(t) => t,
                        rusqlite::types::Value::Blob(_) => String::from("<blob>"),
                    })
                })
                .collect::<rusqlite::Result<Vec<String>>>()
        })?
        .collect::<rusqlite::Result<Vec<Vec<String>>>>()?;
    Ok(rows)
}

#[test]
fn state_db_init_creates_tables() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("state.db");
    let _db = StateDb::open(&db_path)?;
    assert!(db_path.exists(), "state.db should be created");

    let tables = list_table(
        &db_path,
        "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
    )?;
    let names: Vec<&str> = tables.iter().map(|r| r[0].as_str()).collect();
    for expected in ["companions", "meta", "molecules", "specs"] {
        assert!(
            names.contains(&expected),
            "expected table {expected}: {names:?}"
        );
    }
    assert!(
        !names.contains(&"current_molecule"),
        "current_molecule must not exist on a fresh v7 schema: {names:?}",
    );

    let meta = list_table(
        &db_path,
        "SELECT key, value FROM meta WHERE key='schema_version'",
    )?;
    assert_eq!(
        meta,
        vec![vec!["schema_version".to_string(), "7".to_string()]]
    );
    Ok(())
}

#[test]
fn state_db_rebuild_populates_specs_and_molecules() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    write_spec(workspace, "alpha", "# alpha\n\nbody\n")?;
    write_spec(workspace, "beta", "# beta\n\nbody\n")?;

    let db = StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    let molecules = vec![ActiveMolecule {
        id: MoleculeId::new("wx-alpha"),
        spec_label: SpecLabel::new("alpha"),
        base_commit: Some("abc123".to_string()),
    }];

    let report = db.rebuild(workspace, &molecules)?;
    assert_eq!(report.specs, 2);
    assert_eq!(report.molecules, 1);

    let alpha = db.spec(&SpecLabel::new("alpha"))?;
    assert_eq!(alpha.label.as_str(), "alpha");

    let mol = db
        .molecule_for_spec(&SpecLabel::new("alpha"))?
        .context("molecule should be present after rebuild")?;
    assert_eq!(mol.id.as_str(), "wx-alpha");
    assert_eq!(mol.base_commit.as_deref(), Some("abc123"));
    assert_eq!(mol.iteration_count, 0);

    assert!(db.molecule_for_spec(&SpecLabel::new("beta"))?.is_none());
    Ok(())
}

#[test]
fn state_db_rebuild_companions() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    write_spec(
        workspace,
        "with-companions",
        "# spec\n\n## Companions\n\n- `lib/a/`\n- `lib/b/`\n\n## Other\n\n- `lib/skip/`\n",
    )?;
    write_spec(
        workspace,
        "no-companions",
        "# bare spec\n\nno section here\n",
    )?;

    let db = StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    let report = db.rebuild(workspace, &[])?;
    assert_eq!(report.specs, 2);
    assert_eq!(report.companions, 2);

    let rows = list_table(
        &workspace.join(".wrapix/loom/state.db"),
        "SELECT spec_label, companion_path FROM companions ORDER BY spec_label, companion_path",
    )?;
    assert_eq!(
        rows,
        vec![
            vec!["with-companions".to_string(), "lib/a/".to_string()],
            vec!["with-companions".to_string(), "lib/b/".to_string()],
        ]
    );
    Ok(())
}

#[test]
fn state_db_rebuild_resets_counters() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    write_spec(workspace, "alpha", "# alpha\n")?;
    let db = StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    let molecules = vec![ActiveMolecule {
        id: MoleculeId::new("wx-alpha"),
        spec_label: SpecLabel::new("alpha"),
        base_commit: None,
    }];
    db.rebuild(workspace, &molecules)?;

    let mol_id = MoleculeId::new("wx-alpha");
    assert_eq!(db.increment_iteration(&mol_id)?, 1);
    assert_eq!(db.increment_iteration(&mol_id)?, 2);

    db.rebuild(workspace, &molecules)?;
    let mol = db
        .molecule_for_spec(&SpecLabel::new("alpha"))?
        .context("molecule still present after rebuild")?;
    assert_eq!(mol.iteration_count, 0);
    Ok(())
}

/// `StateDb::open` on a fresh DB must NOT carry the obsolete
/// `current_molecule` table. The at-most-one-open-epic-per-spec
/// invariant collapses resolution into a single `bd find` query, so
/// the pointer table is dead code; this test pins that the schema
/// reflects the new model.
#[test]
fn current_molecule_table_does_not_exist() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("state.db");
    let _db = StateDb::open(&db_path)?;
    let rows = list_table(
        &db_path,
        "SELECT name FROM sqlite_master WHERE type='table' AND name='current_molecule'",
    )?;
    assert!(
        rows.is_empty(),
        "current_molecule table must not exist on a v7 schema: {rows:?}",
    );
    Ok(())
}

/// Opening an existing v6 DB that still carries the `current_molecule`
/// table must transparently drop the table and advance the schema
/// version to `7` without losing data in the other tables.
#[test]
fn state_db_open_migrates_v6_drops_current_molecule() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("state.db");

    // Hand-build a v6 DB whose `current_molecule` table carries one
    // pointer, plus rows in the unrelated tables that must survive the
    // v6→v7 migration.
    {
        let conn = rusqlite::Connection::open(&db_path)?;
        conn.execute_batch(
            "CREATE TABLE specs (label TEXT PRIMARY KEY);
            CREATE TABLE molecules (
                id              TEXT PRIMARY KEY,
                spec_label      TEXT NOT NULL REFERENCES specs(label),
                base_commit     TEXT,
                iteration_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE companions (
                spec_label     TEXT NOT NULL REFERENCES specs(label),
                companion_path TEXT NOT NULL,
                PRIMARY KEY (spec_label, companion_path)
            );
            CREATE TABLE notes (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                spec_label TEXT NOT NULL REFERENCES specs(label) ON DELETE CASCADE,
                kind       TEXT NOT NULL,
                text       TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX idx_notes_spec_kind ON notes(spec_label, kind);
            CREATE TABLE current_molecule (
                spec_label TEXT PRIMARY KEY REFERENCES specs(label) ON DELETE CASCADE,
                epic_id    TEXT NOT NULL
            );
            CREATE TABLE meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            INSERT INTO meta(key, value) VALUES ('schema_version', '6');
            INSERT INTO meta(key, value) VALUES ('current_spec', 'alpha');
            INSERT INTO specs(label) VALUES ('alpha');
            INSERT INTO molecules(id, spec_label, base_commit, iteration_count)
              VALUES ('wx-alpha', 'alpha', 'deadbeef', 3);
            INSERT INTO companions(spec_label, companion_path)
              VALUES ('alpha', 'lib/a/');
            INSERT INTO current_molecule(spec_label, epic_id)
              VALUES ('alpha', 'wx-mol.1');",
        )?;
    }

    let db = StateDb::open(&db_path)?;

    let version = list_table(
        &db_path,
        "SELECT value FROM meta WHERE key='schema_version'",
    )?;
    assert_eq!(
        version,
        vec![vec!["7".to_string()]],
        "schema_version must advance to 7",
    );

    let surviving = list_table(
        &db_path,
        "SELECT name FROM sqlite_master WHERE type='table' AND name='current_molecule'",
    )?;
    assert!(
        surviving.is_empty(),
        "current_molecule table must be dropped after v6→v7 migration: {surviving:?}",
    );

    // Unrelated rows survive the migration.
    let alpha = db.spec(&SpecLabel::new("alpha"))?;
    assert_eq!(alpha.label.as_str(), "alpha");
    let mol = db
        .molecule_for_spec(&SpecLabel::new("alpha"))?
        .context("molecules row must survive")?;
    assert_eq!(mol.id.as_str(), "wx-alpha");
    assert_eq!(mol.base_commit.as_deref(), Some("deadbeef"));
    assert_eq!(mol.iteration_count, 3);
    assert_eq!(db.companions(&SpecLabel::new("alpha"))?, vec!["lib/a/"]);
    assert_eq!(
        db.current_spec()?.map(|s| s.to_string()),
        Some("alpha".to_string()),
    );
    Ok(())
}

#[test]
fn state_current_spec_round_trips() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = StateDb::open(dir.path().join("state.db"))?;
    assert!(db.current_spec()?.is_none());

    let label = SpecLabel::new("harness");
    db.set_current_spec(&label)?;
    assert_eq!(db.current_spec()?, Some(label.clone()));

    let other = SpecLabel::new("gate");
    db.set_current_spec(&other)?;
    assert_eq!(db.current_spec()?, Some(other));
    Ok(())
}

#[test]
fn state_increment_iteration_returns_updated_count() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    write_spec(workspace, "alpha", "# alpha\n")?;
    let db = StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    let molecules = vec![ActiveMolecule {
        id: MoleculeId::new("wx-alpha"),
        spec_label: SpecLabel::new("alpha"),
        base_commit: None,
    }];
    db.rebuild(workspace, &molecules)?;

    let mol_id = MoleculeId::new("wx-alpha");
    assert_eq!(db.increment_iteration(&mol_id)?, 1);
    assert_eq!(db.increment_iteration(&mol_id)?, 2);
    assert_eq!(db.increment_iteration(&mol_id)?, 3);
    Ok(())
}

#[test]
fn state_set_and_reset_iteration_round_trip() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    write_spec(workspace, "alpha", "# alpha\n")?;
    let db = StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    let molecules = vec![ActiveMolecule {
        id: MoleculeId::new("wx-alpha"),
        spec_label: SpecLabel::new("alpha"),
        base_commit: None,
    }];
    db.rebuild(workspace, &molecules)?;

    let mol_id = MoleculeId::new("wx-alpha");
    db.set_iteration(&mol_id, 3)?;
    assert_eq!(
        db.molecule_for_spec(&SpecLabel::new("alpha"))?
            .context("molecule present")?
            .iteration_count,
        3
    );

    db.reset_iteration(&mol_id)?;
    assert_eq!(
        db.molecule_for_spec(&SpecLabel::new("alpha"))?
            .context("molecule present")?
            .iteration_count,
        0
    );

    let unknown = MoleculeId::new("wx-missing");
    assert!(db.set_iteration(&unknown, 1).is_err());
    assert!(db.reset_iteration(&unknown).is_err());
    Ok(())
}

#[test]
fn state_db_open_migrates_v1_to_v2() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("state.db");

    // Hand-build a v1 DB matching the pre-migration shape (specs has spec_path
    // NOT NULL, schema_version='1'). The new open() must drop spec_path and
    // bump schema_version to '2' without losing any rows.
    {
        let conn = rusqlite::Connection::open(&db_path)?;
        conn.execute_batch(
            "CREATE TABLE specs (
                label                TEXT PRIMARY KEY,
                spec_path            TEXT NOT NULL,
                implementation_notes TEXT
            );
            CREATE TABLE molecules (
                id              TEXT PRIMARY KEY,
                spec_label      TEXT NOT NULL REFERENCES specs(label),
                base_commit     TEXT,
                iteration_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE companions (
                spec_label     TEXT NOT NULL REFERENCES specs(label),
                companion_path TEXT NOT NULL,
                PRIMARY KEY (spec_label, companion_path)
            );
            CREATE TABLE meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            INSERT INTO meta(key, value) VALUES ('schema_version', '1');
            INSERT INTO specs(label, spec_path, implementation_notes)
              VALUES ('alpha', 'specs/alpha.md', NULL);
            INSERT INTO companions(spec_label, companion_path)
              VALUES ('alpha', 'lib/a/');",
        )?;
    }

    let db = StateDb::open(&db_path)?;

    let meta = list_table(
        &db_path,
        "SELECT value FROM meta WHERE key='schema_version'",
    )?;
    assert_eq!(meta, vec![vec!["7".to_string()]]);

    let cols = list_table(&db_path, "PRAGMA table_info(specs)")?;
    let names: Vec<&str> = cols.iter().map(|r| r[1].as_str()).collect();
    assert!(
        !names.contains(&"spec_path"),
        "spec_path column should be dropped: {names:?}",
    );
    assert!(names.contains(&"label"));
    assert!(
        !names.contains(&"implementation_notes"),
        "implementation_notes column should be dropped: {names:?}",
    );

    let alpha = db.spec(&SpecLabel::new("alpha"))?;
    assert_eq!(alpha.label.as_str(), "alpha");
    assert_eq!(db.companions(&SpecLabel::new("alpha"))?, vec!["lib/a/"]);
    Ok(())
}

#[test]
fn state_db_open_is_idempotent_after_migration() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("state.db");

    // Fresh open lands at the current schema; a second open must be a
    // no-op rather than re-running any ALTER (which would fail because
    // the columns are already in their final shape).
    {
        let _ = StateDb::open(&db_path)?;
    }
    let _db = StateDb::open(&db_path)?;
    let meta = list_table(
        &db_path,
        "SELECT value FROM meta WHERE key='schema_version'",
    )?;
    assert_eq!(meta, vec![vec!["7".to_string()]]);
    Ok(())
}

#[test]
fn routine_commands_never_delete_spec_row() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    write_spec(workspace, "alpha", "# alpha\n")?;
    let db = StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    let molecules = vec![ActiveMolecule {
        id: MoleculeId::new("wx-alpha"),
        spec_label: SpecLabel::new("alpha"),
        base_commit: None,
    }];
    db.rebuild(workspace, &molecules)?;
    let label = SpecLabel::new("alpha");
    let mol_id = MoleculeId::new("wx-alpha");

    db.set_current_spec(&label)?;
    db.set_iteration(&mol_id, 2)?;
    db.increment_iteration(&mol_id)?;
    db.reset_iteration(&mol_id)?;
    db.replace_companions(&label, &["lib/foo/".into(), "lib/bar/".into()])?;
    db.notes_set(
        &label,
        "implementation",
        &["touch lib/foo".to_string()],
        100,
    )?;
    db.notes_clear(&label, Some("implementation"))?;

    let row_count = list_table(
        &workspace.join(".wrapix/loom/state.db"),
        "SELECT COUNT(*) FROM specs WHERE label='alpha'",
    )?;
    assert_eq!(
        row_count,
        vec![vec!["1".to_string()]],
        "routine commands must never DELETE a specs row — only `loom init --rebuild` may",
    );
    assert_eq!(db.spec(&label)?.label.as_str(), "alpha");
    Ok(())
}

#[test]
fn state_corruption_recovery() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("state.db");
    std::fs::write(&db_path, b"this is not a sqlite database\x00\x01\x02")?;

    if StateDb::open(&db_path).is_ok() {
        return Err(anyhow!("opening a corrupt db should fail"));
    }

    let db = StateDb::recreate(&db_path)?;
    let workspace = dir.path();
    write_spec(workspace, "alpha", "# alpha\n")?;
    let report = db.rebuild(workspace, &[])?;
    assert_eq!(report.specs, 1);
    Ok(())
}

// -- notes table CRUD -------------------------------------------------------

#[test]
fn notes_add_then_list_chronological() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = StateDb::open(dir.path().join("state.db"))?;
    let label = SpecLabel::new("alpha");
    let id1 = db.notes_add(&label, "implementation", "first", 100)?;
    let id2 = db.notes_add(&label, "implementation", "second", 200)?;
    let rows = db.notes_list(Some(&label), Some("implementation"))?;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, id1);
    assert_eq!(rows[0].text, "first");
    assert_eq!(rows[1].id, id2);
    assert_eq!(rows[1].text, "second");
    assert!(rows[0].id < rows[1].id, "list must be chronological by id");
    Ok(())
}

#[test]
fn notes_set_replaces_atomically() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = StateDb::open(dir.path().join("state.db"))?;
    let label = SpecLabel::new("alpha");
    db.notes_add(&label, "implementation", "old A", 100)?;
    db.notes_add(&label, "implementation", "old B", 200)?;
    db.notes_set(
        &label,
        "implementation",
        &[
            "new A".to_string(),
            "new B".to_string(),
            "new C".to_string(),
        ],
        300,
    )?;
    let rows = db.notes_list(Some(&label), Some("implementation"))?;
    let texts: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, vec!["new A", "new B", "new C"]);
    Ok(())
}

#[test]
fn notes_clear_kind_only_or_all_kinds() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = StateDb::open(dir.path().join("state.db"))?;
    let label = SpecLabel::new("alpha");
    db.notes_add(&label, "implementation", "impl note", 100)?;
    db.notes_add(&label, "design", "design note", 100)?;

    db.notes_clear(&label, Some("implementation"))?;
    let rows = db.notes_list(Some(&label), None)?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, "design");

    db.notes_clear(&label, None)?;
    let rows = db.notes_list(Some(&label), None)?;
    assert!(rows.is_empty());
    Ok(())
}

#[test]
fn notes_rm_removes_one_row_by_id() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = StateDb::open(dir.path().join("state.db"))?;
    let label = SpecLabel::new("alpha");
    let id1 = db.notes_add(&label, "implementation", "first", 100)?;
    let id2 = db.notes_add(&label, "implementation", "second", 200)?;
    db.notes_rm(id1)?;
    let rows = db.notes_list(Some(&label), Some("implementation"))?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, id2);
    Ok(())
}

#[test]
fn notes_kind_defaults_implementation() -> Result<()> {
    // The CLI binary's `Note` subcommand defaults `--kind` to
    // `implementation`. The DB layer takes `kind` explicitly; the
    // contract this pins is that the CLI's `default_value =
    // "implementation"` matches what `list` reads when called with
    // `--kind implementation` (the same default).
    let dir = tempfile::tempdir()?;
    let db = StateDb::open(dir.path().join("state.db"))?;
    let label = SpecLabel::new("alpha");
    db.notes_add(&label, "implementation", "a", 100)?;
    db.notes_add(&label, "implementation", "b", 200)?;
    let rows = db.notes_list(Some(&label), Some("implementation"))?;
    assert_eq!(rows.len(), 2);
    Ok(())
}

#[test]
fn rebuild_drops_all_notes() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    write_spec(workspace, "alpha", "# alpha\n")?;
    let db = StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    let label = SpecLabel::new("alpha");
    db.notes_add(&label, "implementation", "impl 1", 100)?;
    db.notes_add(&label, "implementation", "impl 2", 200)?;
    db.notes_add(&label, "design", "design 1", 300)?;
    db.notes_add(&label, "review", "review 1", 400)?;
    assert_eq!(db.notes_list(Some(&label), None)?.len(), 4);

    db.rebuild(workspace, &[])?;
    let rows = db.notes_list(Some(&label), None)?;
    assert!(
        rows.is_empty(),
        "rebuild must drop and recreate the notes table — no notes survive regardless of kind",
    );

    let table_rows = list_table(
        &workspace.join(".wrapix/loom/state.db"),
        "SELECT name FROM sqlite_master WHERE type='table' AND name='notes'",
    )?;
    assert_eq!(
        table_rows,
        vec![vec!["notes".to_string()]],
        "notes table must be recreated after rebuild so subsequent inserts succeed",
    );
    Ok(())
}

#[test]
fn notes_cascade_on_spec_delete() -> Result<()> {
    // Verifies the `ON DELETE CASCADE` clause on `notes.spec_label` actually
    // fires when foreign keys are enabled — the spec acknowledges this is a
    // dormant guarantee (no routine command DELETEs from `specs`) but the
    // clause must work if a future code path ever takes that route.
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("state.db");
    let db = StateDb::open(&db_path)?;
    let label = SpecLabel::new("alpha");
    db.notes_add(&label, "implementation", "n1", 100)?;
    db.notes_add(&label, "design", "n2", 200)?;
    assert_eq!(db.notes_list(Some(&label), None)?.len(), 2);

    // Drop the StateDb handle so its connection lock cannot collide with
    // the side-channel connection below. The cascade depends on
    // PRAGMA foreign_keys = ON, which StateDb::open already enables for
    // its own connection — the side-channel must enable it explicitly.
    drop(db);
    let conn = rusqlite::Connection::open(&db_path)?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    let removed = conn.execute(
        "DELETE FROM specs WHERE label = ?1",
        rusqlite::params![label.as_str()],
    )?;
    assert_eq!(removed, 1, "exactly one specs row must be deleted");
    drop(conn);

    let db = StateDb::open(&db_path)?;
    let surviving = db.notes_list(Some(&label), None)?;
    assert!(
        surviving.is_empty(),
        "ON DELETE CASCADE must wipe child notes in the same statement; got {} rows",
        surviving.len(),
    );
    Ok(())
}

#[test]
fn open_wipes_legacy_todo_cursor_meta_keys() -> Result<()> {
    // v4→v5 migration: any `todo_cursor:<label>` row surviving from an
    // older binary is deleted on open. The cursor concept is replaced by
    // the molecule's `loom.base_commit` bead metadata; legacy rows are
    // dead state that must not bleed into v5 callers.
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("state.db");

    {
        let conn = rusqlite::Connection::open(&db_path)?;
        conn.execute_batch(
            "CREATE TABLE specs (label TEXT PRIMARY KEY);
            CREATE TABLE molecules (
                id              TEXT PRIMARY KEY,
                spec_label      TEXT NOT NULL REFERENCES specs(label),
                base_commit     TEXT,
                iteration_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE companions (
                spec_label     TEXT NOT NULL REFERENCES specs(label),
                companion_path TEXT NOT NULL,
                PRIMARY KEY (spec_label, companion_path)
            );
            CREATE TABLE notes (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                spec_label TEXT NOT NULL REFERENCES specs(label) ON DELETE CASCADE,
                kind       TEXT NOT NULL,
                text       TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX idx_notes_spec_kind ON notes(spec_label, kind);
            CREATE TABLE meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            INSERT INTO meta(key, value) VALUES ('schema_version', '4');
            INSERT INTO meta(key, value) VALUES ('current_spec', 'alpha');
            INSERT INTO meta(key, value) VALUES ('todo_cursor:alpha', 'deadbeef');
            INSERT INTO meta(key, value) VALUES ('todo_cursor:beta', 'cafebabe');",
        )?;
    }

    let _db = StateDb::open(&db_path)?;

    let version = list_table(
        &db_path,
        "SELECT value FROM meta WHERE key='schema_version'",
    )?;
    assert_eq!(version, vec![vec!["7".to_string()]]);

    let legacy = list_table(
        &db_path,
        "SELECT key FROM meta WHERE key LIKE 'todo_cursor:%' ORDER BY key",
    )?;
    assert!(
        legacy.is_empty(),
        "v4→v5 migration must wipe legacy todo_cursor:<label> rows: {legacy:?}",
    );

    let current = list_table(&db_path, "SELECT value FROM meta WHERE key='current_spec'")?;
    assert_eq!(
        current,
        vec![vec!["alpha".to_string()]],
        "migration must leave unrelated meta rows intact",
    );
    Ok(())
}

#[test]
fn consume_notes_and_advance_base_commit_is_atomic() -> Result<()> {
    // Productive-completion gate: the implementation-notes delete, the
    // `molecules.base_commit` cache refresh, and the bd-update closure
    // happen as one transaction. A closure failure aborts the whole gate
    // so the local cache and the durable Beads state stay aligned.
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    write_spec(workspace, "alpha", "# alpha\n")?;
    let db = StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    let label = SpecLabel::new("alpha");
    let mol_id = MoleculeId::new("wx-alpha");
    db.rebuild(
        workspace,
        &[ActiveMolecule {
            id: mol_id.clone(),
            spec_label: label.clone(),
            base_commit: Some("old-sha".to_string()),
        }],
    )?;
    db.notes_add(&label, "implementation", "impl 1", 100)?;
    db.notes_add(&label, "implementation", "impl 2", 200)?;
    db.notes_add(&label, "design", "design 1", 300)?;

    let bd_fail: BdUpdateFn = Box::new(|_, _| Err(BdError::CreateMissingId));
    let err = db
        .consume_notes_and_refresh_base_commit(&label, &mol_id, "new-sha", bd_fail)
        .expect_err("closure failure must propagate");
    assert!(
        matches!(err, StateError::BdUpdate(BdError::CreateMissingId)),
        "expected StateError::BdUpdate, got {err:?}",
    );

    assert_eq!(
        db.notes_list(Some(&label), Some("implementation"))?.len(),
        2,
        "rollback must keep the implementation notes intact",
    );
    let mol = db
        .molecule_for_spec(&label)?
        .context("molecule must survive rollback")?;
    assert_eq!(
        mol.base_commit,
        Some("old-sha".to_string()),
        "rollback must keep the molecules.base_commit cache at its pre-write value",
    );

    let bd_ok: BdUpdateFn = Box::new(|_, _| Ok(()));
    db.consume_notes_and_refresh_base_commit(&label, &mol_id, "new-sha", bd_ok)?;

    assert!(
        db.notes_list(Some(&label), Some("implementation"))?
            .is_empty(),
        "productive completion must delete every implementation-kind note",
    );
    assert_eq!(
        db.notes_list(Some(&label), Some("design"))?.len(),
        1,
        "non-implementation kinds must survive the gate",
    );
    let mol = db
        .molecule_for_spec(&label)?
        .context("molecule lookup after commit")?;
    assert_eq!(
        mol.base_commit,
        Some("new-sha".to_string()),
        "molecules.base_commit cache must advance atomically with the notes delete",
    );
    Ok(())
}

#[test]
fn consume_notes_and_refresh_base_commit_invokes_closure_with_args() -> Result<()> {
    // The closure receives the molecule id and the new base_commit
    // verbatim; the gate is the single-pointed surface that wires the
    // SQLite writes to the durable bead-metadata write.
    use std::sync::Mutex;
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    write_spec(workspace, "alpha", "# alpha\n")?;
    let db = StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    let label = SpecLabel::new("alpha");
    let mol_id = MoleculeId::new("wx-alpha");
    db.rebuild(
        workspace,
        &[ActiveMolecule {
            id: mol_id.clone(),
            spec_label: label.clone(),
            base_commit: None,
        }],
    )?;

    let captured: std::sync::Arc<Mutex<Vec<(String, String)>>> =
        std::sync::Arc::new(Mutex::new(Vec::new()));
    let probe = std::sync::Arc::clone(&captured);
    let bd_capture: BdUpdateFn = Box::new(move |mol_id, new_base_commit| {
        probe
            .lock()
            .unwrap()
            .push((mol_id.as_str().to_owned(), new_base_commit.to_owned()));
        Ok(())
    });
    db.consume_notes_and_refresh_base_commit(&label, &mol_id, "fresh-head", bd_capture)?;

    let calls = captured.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec![("wx-alpha".to_string(), "fresh-head".to_string())],
        "closure must receive the molecule id and new base_commit unchanged",
    );
    Ok(())
}

/// At the state-DB layer, `rebuild` mirrors the single-query resolver's
/// invariant: a spec may carry at most one active molecule. When the input
/// list pairs two molecules with the same spec_label, rebuild MUST refuse
/// with [`StateError::DuplicateSpecMolecules`] naming every conflicting id,
/// not silently insert both. Spec: `harness.md` *Auxiliary commands*
/// `loom init --rebuild` aborts on this case rather than papering over the
/// invariant break.
#[test]
fn todo_resolution_is_single_query_with_invariant_violation_refusal() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    write_spec(workspace, "alpha", "# alpha\n")?;

    let db = StateDb::open(workspace.join(".wrapix/loom/state.db"))?;
    let molecules = vec![
        ActiveMolecule {
            id: MoleculeId::new("wx-aaa"),
            spec_label: SpecLabel::new("alpha"),
            base_commit: None,
        },
        ActiveMolecule {
            id: MoleculeId::new("wx-bbb"),
            spec_label: SpecLabel::new("alpha"),
            base_commit: None,
        },
    ];
    let err = db.rebuild(workspace, &molecules).unwrap_err();
    match err {
        StateError::DuplicateSpecMolecules { label, ids } => {
            assert_eq!(label, "alpha");
            assert!(
                ids.contains("wx-aaa") && ids.contains("wx-bbb"),
                "expected every conflicting id in the error, got {ids:?}",
            );
        }
        other => return Err(anyhow!("expected DuplicateSpecMolecules, got {other:?}")),
    }
    Ok(())
}

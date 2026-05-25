use std::path::Path;

use loom_driver::identifier::SpecLabel;
use loom_driver::markdown::{HeadingLevel, section_events};
use loom_driver::state::{StateDb, parse_companions};

use super::error::PlanError;

/// Outcome of a companion-section reconciliation, returned by
/// [`reconcile_companions`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompanionReconciliation {
    pub paths: Vec<String>,
    /// `true` when the spec markdown contained a `## Companions` heading.
    /// Distinguishes "intentionally zero companions declared" from "section
    /// missing entirely" so callers can warn the user when an interview
    /// silently produced no declarations.
    pub section_present: bool,
}

/// After the interactive interview exits, read the (possibly newly-created)
/// spec markdown, parse its `## Companions` section, and replace the
/// state-DB rows for `label`. Specs without a `## Companions` heading land
/// zero rows, matching `parse_companions`'s tolerance.
pub fn reconcile_companions(
    db: &StateDb,
    label: &SpecLabel,
    spec_path: &Path,
) -> Result<CompanionReconciliation, PlanError> {
    let body = match std::fs::read_to_string(spec_path) {
        Ok(b) => b,
        Err(source) => {
            return Err(PlanError::ReadSpec {
                path: spec_path.to_path_buf(),
                source,
            });
        }
    };
    let paths = parse_companions(&body);
    let section_present = section_events(&body, HeadingLevel::H2, |t| t == "Companions").is_some();
    db.replace_companions(label, &paths)?;
    Ok(CompanionReconciliation {
        paths,
        section_present,
    })
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests use panicking helpers")]
mod tests {
    use super::*;
    use anyhow::Result;
    use loom_driver::state::StateDb;

    #[test]
    fn empty_spec_lands_zero_rows() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = StateDb::open(dir.path().join("state.db"))?;
        let spec = dir.path().join("specs/foo.md");
        std::fs::create_dir_all(spec.parent().unwrap())?;
        std::fs::write(
            &spec,
            "# Foo\n\n## Architecture\n\nNo companions section.\n",
        )?;
        let label = SpecLabel::new("foo");

        let outcome = reconcile_companions(&db, &label, &spec)?;
        assert!(outcome.paths.is_empty());
        assert!(!outcome.section_present);
        assert!(db.companions(&label)?.is_empty());
        Ok(())
    }

    #[test]
    fn empty_section_with_heading_reports_section_present() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = StateDb::open(dir.path().join("state.db"))?;
        let spec = dir.path().join("specs/foo.md");
        std::fs::create_dir_all(spec.parent().unwrap())?;
        std::fs::write(&spec, "# Foo\n\n## Companions\n\n(intentionally none)\n")?;
        let label = SpecLabel::new("foo");

        let outcome = reconcile_companions(&db, &label, &spec)?;
        assert!(outcome.paths.is_empty());
        assert!(outcome.section_present);
        Ok(())
    }

    #[test]
    fn populated_spec_lands_each_path() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = StateDb::open(dir.path().join("state.db"))?;
        let spec = dir.path().join("specs/foo.md");
        std::fs::create_dir_all(spec.parent().unwrap())?;
        std::fs::write(
            &spec,
            "# Foo\n\n## Companions\n\n- `lib/sandbox/`\n- `crates/loom-templates/templates/`\n",
        )?;
        let label = SpecLabel::new("foo");

        let outcome = reconcile_companions(&db, &label, &spec)?;
        assert_eq!(
            outcome.paths,
            vec!["lib/sandbox/", "crates/loom-templates/templates/"]
        );
        assert!(outcome.section_present);
        let stored = db.companions(&label)?;
        assert_eq!(
            stored,
            vec!["crates/loom-templates/templates/", "lib/sandbox/"]
        );
        Ok(())
    }

    #[test]
    fn rerun_replaces_previous_rows() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = StateDb::open(dir.path().join("state.db"))?;
        let spec = dir.path().join("specs/foo.md");
        std::fs::create_dir_all(spec.parent().unwrap())?;
        let label = SpecLabel::new("foo");

        std::fs::write(&spec, "## Companions\n\n- `old/path/`\n")?;
        reconcile_companions(&db, &label, &spec)?;
        assert_eq!(db.companions(&label)?, vec!["old/path/"]);

        std::fs::write(&spec, "## Companions\n\n- `new/path/`\n")?;
        reconcile_companions(&db, &label, &spec)?;
        // Old row gone, new row landed.
        assert_eq!(db.companions(&label)?, vec!["new/path/"]);
        Ok(())
    }

    #[test]
    fn missing_spec_file_errors() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = StateDb::open(dir.path().join("state.db"))?;
        let label = SpecLabel::new("foo");
        let spec = dir.path().join("specs/foo.md");

        match reconcile_companions(&db, &label, &spec) {
            Err(PlanError::ReadSpec { path, .. }) => {
                assert_eq!(path, spec);
                Ok(())
            }
            other => Err(anyhow::anyhow!("expected ReadSpec, got {other:?}")),
        }
    }
}

//! `loom spec` — query spec annotations and their tooling dependencies.
//!
//! The default mode prints `[verify]`/`[judge]` annotations parsed from the
//! active spec's `## Success Criteria` section. The `--deps` mode walks each
//! annotated test file and emits the set of nixpkgs names referenced by the
//! tools they invoke.
//!
//! Read-only — no lock acquired (per the lock matrix in
//! `specs/harness.md`).

mod annotations;
mod deps;
mod error;

use std::path::Path;

pub use annotations::{Annotation, AnnotationKind, parse_spec_annotations};
pub use deps::{collect_deps, scan_file_body};
pub use error::SpecError;

use loom_driver::identifier::SpecLabel;

/// Convenience: locate the spec file for `label` under `<workspace>/specs/`
/// and parse its annotations.
pub fn list_for_label(workspace: &Path, label: &SpecLabel) -> Result<Vec<Annotation>, SpecError> {
    let spec_path = workspace
        .join("specs")
        .join(format!("{}.md", label.as_str()));
    parse_spec_annotations(&spec_path)
}

/// Convenience: parse `<workspace>/specs/<label>.md` and return the unique
/// nixpkgs names referenced by its `verify`/`judge` test files.
pub fn deps_for_label(
    workspace: &Path,
    label: &SpecLabel,
) -> Result<std::collections::BTreeSet<String>, SpecError> {
    let annotations = list_for_label(workspace, label)?;
    collect_deps(workspace, &annotations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use std::fs;

    #[test]
    fn list_for_label_reads_spec_under_workspace() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let specs = dir.path().join("specs");
        fs::create_dir_all(&specs)?;
        fs::write(
            specs.join("alpha.md"),
            "## Success Criteria\n\n- [ ] a\n  [verify](tests/alpha.sh#test_a)\n",
        )?;
        let rows = list_for_label(dir.path(), &SpecLabel::new("alpha"))?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, AnnotationKind::Verify);
        Ok(())
    }

    #[test]
    fn deps_for_label_aggregates_across_test_files() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let specs = dir.path().join("specs");
        let tests = dir.path().join("tests");
        fs::create_dir_all(&specs)?;
        fs::create_dir_all(&tests)?;
        fs::write(tests.join("a.sh"), "curl x\n")?;
        fs::write(tests.join("b.sh"), "jq .\n")?;
        fs::write(
            specs.join("alpha.md"),
            "## Success Criteria\n\n- [ ] a\n  [verify](tests/a.sh#test_a)\n- [ ] b\n  [judge](tests/b.sh#test_b)\n",
        )?;
        let pkgs = deps_for_label(dir.path(), &SpecLabel::new("alpha"))?;
        assert!(pkgs.contains("curl"));
        assert!(pkgs.contains("jq"));
        Ok(())
    }
}

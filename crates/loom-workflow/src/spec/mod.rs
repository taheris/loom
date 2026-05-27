//! `loom spec` — query spec annotations and their tooling dependencies.
//!
//! Annotation parsing is delegated to [`loom_gate::annotation`] — the
//! authoritative `[check]/[test]/[system]/[judge]` parser owned by the gate
//! crate (per `docs/spec-conventions.md` Trust tiers). This module reuses
//! that parser to render the `loom spec` table and to walk file-shaped
//! `[test]`/`[judge]` targets for nixpkgs dependency discovery.
//!
//! Read-only — no lock acquired (per the lock matrix in
//! `specs/harness.md`).

mod deps;
mod error;

use std::path::Path;

pub use deps::{collect_deps, scan_file_body, target_file_path};
pub use error::SpecError;

use loom_driver::identifier::SpecLabel;
use loom_gate::annotation::{Annotation, parse_content};

/// Convenience: locate the spec file for `label` under `<workspace>/specs/`
/// and parse its annotations via [`loom_gate::annotation::parse_content`].
pub fn list_for_label(workspace: &Path, label: &SpecLabel) -> Result<Vec<Annotation>, SpecError> {
    let spec_path = workspace
        .join("specs")
        .join(format!("{}.md", label.as_str()));
    let body = std::fs::read_to_string(&spec_path).map_err(|source| SpecError::Io {
        path: spec_path.clone(),
        source,
    })?;
    Ok(parse_content(&spec_path, &body).annotations)
}

/// Convenience: parse `<workspace>/specs/<label>.md` and return the unique
/// nixpkgs names referenced by its `[check]`/`[test]`/`[system]`/`[judge]`
/// annotations.
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
    use loom_gate::annotation::Tier;
    use std::fs;

    #[test]
    fn list_for_label_reads_all_four_tiers() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let specs = dir.path().join("specs");
        fs::create_dir_all(&specs)?;
        fs::write(
            specs.join("alpha.md"),
            "## Success Criteria\n\n\
             - a [check](cargo run -p w -- a)\n\
             - b [test](crate::t::it)\n\
             - c [system](nix run .#x)\n\
             - d [judge](rubrics/api.md)\n",
        )?;
        let rows = list_for_label(dir.path(), &SpecLabel::new("alpha"))?;
        let tiers: Vec<Tier> = rows.iter().map(|a| a.tier).collect();
        assert_eq!(
            tiers,
            vec![Tier::Check, Tier::Test, Tier::System, Tier::Judge],
        );
        Ok(())
    }

    #[test]
    fn deps_for_label_walks_file_targets_and_command_strings() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let specs = dir.path().join("specs");
        let tests = dir.path().join("tests");
        fs::create_dir_all(&specs)?;
        fs::create_dir_all(&tests)?;
        fs::write(tests.join("a.sh"), "curl x\n")?;
        fs::write(tests.join("b.sh"), "jq .\n")?;
        fs::write(
            specs.join("alpha.md"),
            "## Success Criteria\n\n\
             - a [test](tests/a.sh#test_a)\n\
             - b [judge](tests/b.sh#test_b)\n\
             - c [check](rg pattern files)\n",
        )?;
        let pkgs = deps_for_label(dir.path(), &SpecLabel::new("alpha"))?;
        assert!(pkgs.contains("curl"), "file-shaped [test] target scanned");
        assert!(pkgs.contains("jq"), "file-shaped [judge] target scanned");
        assert!(
            pkgs.contains("ripgrep"),
            "[check] command string scanned directly",
        );
        Ok(())
    }
}

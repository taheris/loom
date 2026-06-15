use std::path::Path;

use rusqlite::params;
use tracing::{debug, warn};

use crate::identifier::{MoleculeId, SpecLabel};

use super::companions::parse_companions;
use super::db::{CacheDb, drop_and_recreate};
use super::error::CacheError;

/// One active molecule from `bd list --status=open --type=epic`.
///
/// `rebuild` consumes pre-fetched values rather than calling `bd` itself —
/// the caller (e.g. `loom init --rebuild` wiring `BdClient`) is responsible
/// for issuing the CLI calls. Keeps `loom-driver` free of subprocess
/// orchestration and makes rebuild testable without a real `bd` binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveMolecule {
    pub id: MoleculeId,
    pub spec_label: SpecLabel,
    pub base_commit: Option<String>,
}

/// Counts of rows written by [`CacheDb::rebuild`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RebuildReport {
    pub specs: usize,
    pub spec_epics: usize,
    pub work_epics: usize,
    pub companions: usize,
}

impl CacheDb {
    /// Drop all cache tables, recreate the schema, and repopulate from:
    ///
    /// 1. `<workspace>/specs/*.md` — one `specs` row per file (label = file
    ///    stem; path = repo-relative POSIX).
    /// 2. `molecules` argument — one `molecules` row per active molecule.
    /// 3. Each spec's `## Companions` section — one `companions` row per
    ///    listed path. Specs without the section contribute zero rows.
    ///
    /// Iteration counters reset to 0.
    pub fn rebuild(
        &self,
        workspace: &Path,
        molecules: &[ActiveMolecule],
    ) -> Result<RebuildReport, CacheError> {
        let specs_dir = workspace.join("specs");
        let spec_files = collect_spec_files(&specs_dir)?;
        let indexed_specs = collect_indexed_specs(workspace)?;
        let spec_rows = if indexed_specs.is_empty() {
            spec_files
                .iter()
                .map(|(label, content)| (label.clone(), default_spec_path(label), content.clone()))
                .collect::<Vec<_>>()
        } else {
            cross_check_index_and_files(&indexed_specs, &spec_files)?
        };

        let mut by_spec: std::collections::BTreeMap<&str, Vec<&str>> =
            std::collections::BTreeMap::new();
        for mol in molecules {
            by_spec
                .entry(mol.spec_label.as_str())
                .or_default()
                .push(mol.id.as_str());
        }
        if let Some((label, ids)) = by_spec.iter().find(|(_, ids)| ids.len() > 1) {
            return Err(CacheError::DuplicateSpecMolecules {
                label: (*label).to_string(),
                ids: ids.join(", "),
            });
        }
        if !indexed_specs.is_empty()
            && let Some(label) = spec_rows
                .iter()
                .map(|(label, _, _)| label.as_str())
                .find(|label| !by_spec.contains_key(label))
        {
            return Err(CacheError::SpecIndexMismatch {
                detail: format!("indexed spec `{label}` has no loom:spec epic"),
            });
        }

        self.with_conn(|conn| {
            drop_and_recreate(conn)?;
            let mut report = RebuildReport::default();

            for (label, spec_path, content) in &spec_rows {
                conn.execute(
                    "INSERT INTO specs(label, spec_path) VALUES (?1, ?2)",
                    params![label.as_str(), spec_path],
                )?;
                report.specs += 1;

                for path in parse_companions(content) {
                    conn.execute(
                        "INSERT OR IGNORE INTO companions(spec_label, companion_path)
                         VALUES (?1, ?2)",
                        params![label.as_str(), path],
                    )?;
                    report.companions += 1;
                }
            }

            for mol in molecules {
                if !spec_rows.iter().any(|(l, _, _)| l == &mol.spec_label) {
                    warn!(
                        molecule = %mol.id,
                        spec = %mol.spec_label,
                        "skipping molecule whose spec_label has no spec file",
                    );
                    continue;
                }
                conn.execute(
                    "INSERT INTO spec_epics(spec_label, epic_id, todo_cursor)
                     VALUES (?1, ?2, ?3)",
                    params![mol.spec_label.as_str(), mol.id.as_str(), mol.base_commit],
                )?;
                conn.execute(
                    "INSERT INTO work_epics(epic_id, todo_head, is_active, iteration_count)
                     VALUES (?1, ?2, 1, 0)",
                    params![mol.id.as_str(), mol.base_commit],
                )?;
                report.spec_epics += 1;
                report.work_epics += 1;
            }

            debug!(?report, "cache-db rebuild complete");
            Ok(report)
        })
    }
}

fn cross_check_index_and_files(
    indexed_specs: &[(SpecLabel, String)],
    spec_files: &[(SpecLabel, String)],
) -> Result<Vec<(SpecLabel, String, String)>, CacheError> {
    for (label, spec_path) in indexed_specs {
        let expected = default_spec_path(label);
        if spec_path != &expected {
            return Err(CacheError::SpecIndexMismatch {
                detail: format!(
                    "index row for `{label}` points at `{spec_path}`, expected `{expected}`"
                ),
            });
        }
        if !spec_files.iter().any(|(file_label, _)| file_label == label) {
            return Err(CacheError::SpecIndexMismatch {
                detail: format!("indexed spec `{label}` is missing `{spec_path}`"),
            });
        }
    }
    if let Some((label, _)) = spec_files
        .iter()
        .find(|(label, _)| !indexed_specs.iter().any(|(indexed, _)| indexed == label))
    {
        return Err(CacheError::SpecIndexMismatch {
            detail: format!(
                "spec file `{}` is missing from docs/README.md",
                default_spec_path(label)
            ),
        });
    }
    let mut rows = Vec::with_capacity(indexed_specs.len());
    for (label, spec_path) in indexed_specs {
        if let Some((_, content)) = spec_files
            .iter()
            .find(|(file_label, _)| file_label == label)
        {
            rows.push((label.clone(), spec_path.clone(), content.clone()));
        }
    }
    Ok(rows)
}

fn collect_indexed_specs(workspace: &Path) -> Result<Vec<(SpecLabel, String)>, CacheError> {
    let index_path = workspace.join("docs/README.md");
    if !index_path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(index_path)?;
    let mut out = Vec::new();
    for line in content.lines() {
        let Some(start) = line.find("](../specs/") else {
            continue;
        };
        let path_start = start + "](../".len();
        let Some(rest) = line.get(path_start..) else {
            continue;
        };
        let Some(end) = rest.find(')') else {
            continue;
        };
        let spec_path = &rest[..end];
        let Some(label) = spec_path
            .strip_prefix("specs/")
            .and_then(|path| path.strip_suffix(".md"))
        else {
            continue;
        };
        if out
            .iter()
            .any(|(existing, _): &(SpecLabel, String)| existing.as_str() == label)
        {
            return Err(CacheError::SpecIndexMismatch {
                detail: format!("duplicate index row for spec `{label}`"),
            });
        }
        out.push((SpecLabel::new(label), spec_path.to_string()));
    }
    Ok(out)
}

fn collect_spec_files(specs_dir: &Path) -> Result<Vec<(SpecLabel, String)>, CacheError> {
    if !specs_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(specs_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let content = std::fs::read_to_string(&path)?;
        let label = SpecLabel::new(stem.to_string());
        if out
            .iter()
            .any(|(existing, _): &(SpecLabel, String)| existing == &label)
        {
            return Err(CacheError::SpecIndexMismatch {
                detail: format!("duplicate spec file for `{label}`"),
            });
        }
        out.push((label, content));
    }
    out.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    Ok(out)
}

fn default_spec_path(label: &SpecLabel) -> String {
    format!("specs/{}.md", label.as_str())
}

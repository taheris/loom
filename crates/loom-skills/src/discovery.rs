use displaydoc::Display;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;
use walkdir::WalkDir;

use crate::document::{FrontmatterError, RawSkillDocument, SkillDocument};
use crate::registry::{NamedSkill, SkillSet};
use crate::source::{SkillProvenance, SkillSource};

const SKILL_DOCUMENT: &str = "skill.md";
const TUNING_DOCUMENT: &str = "tuning.md";
const OVERRIDE_ROOT: &str = ".loom-override/skills";
const LOOM_SKILLS_SOURCE_DIR: &str = "crates/loom-skills";
const LOOM_SKILLS_BUILTIN_SOURCE: &str = "crates/loom-skills/src/builtin.rs";

/// Severity assigned to a skill diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Warning,
    Error,
}

/// Stable diagnostic kind for source-aware skill loading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticKind {
    Io,
    Document,
    MissingFrontmatter,
    MissingName,
    MissingDescription,
    InvalidName,
    InvalidDescription,
    InvalidPhase,
    InvalidProfile,
}

/// Source-aware diagnostic emitted while loading skill candidates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDiagnostic {
    pub severity: DiagnosticSeverity,
    pub source: SkillSource,
    pub path: PathBuf,
    pub kind: DiagnosticKind,
    pub message: String,
}

/// Loaded skill candidates plus non-fatal warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryReport {
    set: SkillSet,
    warnings: Vec<SkillDiagnostic>,
}

impl DiscoveryReport {
    pub fn new(set: SkillSet, warnings: Vec<SkillDiagnostic>) -> Self {
        Self { set, warnings }
    }

    pub fn set(&self) -> &SkillSet {
        &self.set
    }

    pub fn into_set(self) -> SkillSet {
        self.set
    }

    pub fn warnings(&self) -> &[SkillDiagnostic] {
        &self.warnings
    }

    fn extend_set(&mut self, set: SkillSet) {
        self.set.extend(set);
    }
}

/// Skill discovery failures.
#[derive(Debug, Display, Error)]
pub enum DiscoveryError {
    /// failed to walk configured directory `{path}`
    WalkDir {
        path: PathBuf,
        #[source]
        source: walkdir::Error,
    },
    /// configured skill path `{path}` does not exist
    PathMissing { path: PathBuf },
    /// failed to compute relative path under `{root}` for `{path}`
    StripPrefix {
        root: PathBuf,
        path: PathBuf,
        #[source]
        source: std::path::StripPrefixError,
    },
    /// duplicate case variants for `{basename}` in `{directory}`: {variants:?}
    DuplicateCaseVariant {
        directory: PathBuf,
        basename: String,
        variants: Vec<PathBuf>,
    },
    /// invalid skill `{path}` from `{skill_source:?}`: {message}
    InvalidSkill {
        skill_source: SkillSource,
        path: PathBuf,
        kind: DiagnosticKind,
        message: String,
    },
}

pub fn discover_workspace(
    workspace: impl AsRef<Path>,
    tracked_files: &[PathBuf],
) -> Result<DiscoveryReport, DiscoveryError> {
    let workspace = workspace.as_ref();
    validate_package_case_variants(workspace, tracked_files)?;
    let mut set = SkillSet::default();
    let mut warnings = Vec::new();
    let tuning_by_dir = tuning_documents_by_dir(tracked_files);

    let embedded_paths = embedded_catalog_source_paths(tracked_files);
    for rel in tracked_files
        .iter()
        .filter(|path| file_name_ci(path, SKILL_DOCUMENT))
        .filter(|path| !is_override_path(path))
        .filter(|path| !embedded_paths.contains(&comparable_path(path)))
    {
        let path = workspace.join(rel);
        let tuning_path = tuning_by_dir
            .get(&parent_key(rel))
            .map(|tuning| workspace.join(tuning));
        match load_package(&path, tuning_path, SkillSource::Workspace) {
            Ok(skill) => set.push(skill),
            Err(diagnostic) => warnings.push(SkillDiagnostic {
                severity: DiagnosticSeverity::Warning,
                ..diagnostic
            }),
        }
    }
    Ok(DiscoveryReport::new(set, warnings))
}

pub fn load_configured_paths(
    workspace: impl AsRef<Path>,
    paths: &[PathBuf],
) -> Result<SkillSet, DiscoveryError> {
    load_configured_paths_inner(workspace.as_ref(), paths, &BTreeSet::new())
}

pub fn load_workspace(
    workspace: impl AsRef<Path>,
    tracked_files: &[PathBuf],
    configured_paths: &[PathBuf],
) -> Result<DiscoveryReport, DiscoveryError> {
    let workspace = workspace.as_ref();
    let mut report = discover_workspace(workspace, tracked_files)?;
    let skip = canonical_package_dirs(report.set().skills());
    let configured = load_configured_paths_inner(workspace, configured_paths, &skip)?;
    let overrides = load_overrides(workspace)?;
    report.extend_set(configured);
    report.extend_set(overrides);
    Ok(report)
}

pub fn load_overrides(workspace: impl AsRef<Path>) -> Result<SkillSet, DiscoveryError> {
    let root = workspace.as_ref().join(OVERRIDE_ROOT);
    if !root.exists() {
        return Ok(SkillSet::default());
    }
    let files = markdown_files_under(&root)?;
    validate_package_case_variants(&root, &files)?;
    let package_dirs = package_directories(&files);
    let tuning_by_dir = tuning_documents_by_dir(&files);
    let mut set = SkillSet::default();
    for rel in files {
        let parent = parent_key(&rel);
        if file_name_ci(&rel, TUNING_DOCUMENT) && package_dirs.contains(&parent) {
            continue;
        }
        if package_dirs.contains(&parent) && !file_name_ci(&rel, SKILL_DOCUMENT) {
            continue;
        }
        let path = root.join(&rel);
        let result = if file_name_ci(&rel, SKILL_DOCUMENT) {
            let tuning_path = tuning_by_dir.get(&parent).map(|tuning| root.join(tuning));
            load_package(&path, tuning_path, SkillSource::Override)
        } else {
            load_loose_file(&path, SkillSource::Override)
        };
        match result {
            Ok(skill) => set.push(skill),
            Err(diagnostic) => return Err(invalid_skill(diagnostic)),
        }
    }
    Ok(set)
}

fn load_configured_paths_inner(
    workspace: &Path,
    paths: &[PathBuf],
    skip_canonical: &BTreeSet<PathBuf>,
) -> Result<SkillSet, DiscoveryError> {
    let mut set = SkillSet::default();
    for configured in paths {
        let path = resolve_workspace_path(workspace, configured);
        if path.is_file() {
            match load_loose_file(&path, SkillSource::Configured) {
                Ok(skill) => set.push(skill),
                Err(diagnostic) => return Err(invalid_skill(diagnostic)),
            }
        } else if path.is_dir() {
            for file in markdown_files_under(&path)? {
                let full = path.join(file);
                if is_under_canonical_package(&full, skip_canonical) {
                    continue;
                }
                match load_loose_file(&full, SkillSource::Configured) {
                    Ok(skill) => set.push(skill),
                    Err(diagnostic) => return Err(invalid_skill(diagnostic)),
                }
            }
        } else {
            return Err(DiscoveryError::PathMissing { path });
        }
    }
    Ok(set)
}

fn markdown_files_under(root: &Path) -> Result<Vec<PathBuf>, DiscoveryError> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|source| DiscoveryError::WalkDir {
            path: root.to_path_buf(),
            source,
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        {
            let rel = path
                .strip_prefix(root)
                .map_err(|source| DiscoveryError::StripPrefix {
                    root: root.to_path_buf(),
                    path: path.to_path_buf(),
                    source,
                })?;
            files.push(rel.to_path_buf());
        }
    }
    files.sort();
    Ok(files)
}

fn validate_package_case_variants(root: &Path, files: &[PathBuf]) -> Result<(), DiscoveryError> {
    let package_dirs = package_directories(files);
    let mut grouped: BTreeMap<(PathBuf, String), Vec<PathBuf>> = BTreeMap::new();
    for file in files {
        let basename = lower_file_name(file);
        if basename == SKILL_DOCUMENT || basename == TUNING_DOCUMENT {
            grouped
                .entry((parent_key(file), basename))
                .or_default()
                .push(root.join(file));
        }
    }
    for ((directory, basename), mut variants) in grouped {
        let is_package_tuning = basename == TUNING_DOCUMENT && package_dirs.contains(&directory);
        if (basename == SKILL_DOCUMENT || is_package_tuning) && variants.len() > 1 {
            variants.sort();
            return Err(DiscoveryError::DuplicateCaseVariant {
                directory: root.join(directory),
                basename,
                variants,
            });
        }
    }
    Ok(())
}

fn load_package(
    path: &Path,
    tuning_path: Option<PathBuf>,
    source: SkillSource,
) -> Result<NamedSkill, SkillDiagnostic> {
    let markdown = read_markdown(path, source)?;
    let provenance = SkillProvenance::package(source, path.to_path_buf(), tuning_path, &markdown);
    parse_named(path, source, markdown, provenance)
}

fn load_loose_file(path: &Path, source: SkillSource) -> Result<NamedSkill, SkillDiagnostic> {
    let markdown = read_markdown(path, source)?;
    let provenance = SkillProvenance::loose_file(source, path.to_path_buf(), &markdown);
    parse_named(path, source, markdown, provenance)
}

fn read_markdown(path: &Path, source: SkillSource) -> Result<String, SkillDiagnostic> {
    fs::read_to_string(path).map_err(|err| SkillDiagnostic {
        severity: DiagnosticSeverity::Error,
        source,
        path: path.to_path_buf(),
        kind: DiagnosticKind::Io,
        message: err.to_string(),
    })
}

fn parse_named(
    path: &Path,
    source: SkillSource,
    markdown: String,
    provenance: SkillProvenance,
) -> Result<NamedSkill, SkillDiagnostic> {
    let document =
        SkillDocument::parse(RawSkillDocument::new(markdown, provenance)).map_err(|err| {
            SkillDiagnostic {
                severity: DiagnosticSeverity::Error,
                source,
                path: path.to_path_buf(),
                kind: DiagnosticKind::Document,
                message: err.to_string(),
            }
        })?;
    NamedSkill::from_document(document).map_err(|err| SkillDiagnostic {
        severity: DiagnosticSeverity::Error,
        source,
        path: path.to_path_buf(),
        kind: diagnostic_kind(&err),
        message: err.to_string(),
    })
}

fn diagnostic_kind(err: &FrontmatterError) -> DiagnosticKind {
    match err {
        FrontmatterError::MissingFrontmatter => DiagnosticKind::MissingFrontmatter,
        FrontmatterError::MissingName => DiagnosticKind::MissingName,
        FrontmatterError::MissingDescription => DiagnosticKind::MissingDescription,
        FrontmatterError::InvalidName { .. } => DiagnosticKind::InvalidName,
        FrontmatterError::InvalidDescription { .. } => DiagnosticKind::InvalidDescription,
        FrontmatterError::InvalidPhase { .. } => DiagnosticKind::InvalidPhase,
        FrontmatterError::InvalidProfile { .. } => DiagnosticKind::InvalidProfile,
    }
}

fn invalid_skill(diagnostic: SkillDiagnostic) -> DiscoveryError {
    DiscoveryError::InvalidSkill {
        skill_source: diagnostic.source,
        path: diagnostic.path,
        kind: diagnostic.kind,
        message: diagnostic.message,
    }
}

fn canonical_package_dirs(skills: &[NamedSkill]) -> BTreeSet<PathBuf> {
    skills
        .iter()
        .filter(|skill| skill.source() == SkillSource::Workspace)
        .map(|skill| comparable_path(&skill.provenance().base_dir))
        .collect()
}

fn is_under_canonical_package(path: &Path, skip: &BTreeSet<PathBuf>) -> bool {
    let comparable = comparable_path(path);
    skip.iter()
        .any(|package_dir| comparable.starts_with(package_dir))
}

fn comparable_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        out.push(component.as_os_str());
    }
    out
}

fn is_override_path(path: &Path) -> bool {
    comparable_path(path).starts_with(Path::new(OVERRIDE_ROOT))
}

fn embedded_catalog_source_paths(tracked_files: &[PathBuf]) -> BTreeSet<PathBuf> {
    if !is_loom_source_checkout(tracked_files) {
        return BTreeSet::new();
    }
    crate::builtin::PACKAGES
        .iter()
        .map(|package| {
            comparable_path(&Path::new(LOOM_SKILLS_SOURCE_DIR).join(package.relative_path))
        })
        .collect()
}

fn is_loom_source_checkout(tracked_files: &[PathBuf]) -> bool {
    let marker = comparable_path(Path::new(LOOM_SKILLS_BUILTIN_SOURCE));
    tracked_files
        .iter()
        .any(|path| comparable_path(path) == marker)
}

fn resolve_workspace_path(workspace: &Path, configured: &Path) -> PathBuf {
    if configured.is_absolute() {
        configured.to_path_buf()
    } else {
        workspace.join(configured)
    }
}

fn package_directories(files: &[PathBuf]) -> BTreeSet<PathBuf> {
    files
        .iter()
        .filter(|path| file_name_ci(path, SKILL_DOCUMENT))
        .map(|path| parent_key(path))
        .collect()
}

fn tuning_documents_by_dir(files: &[PathBuf]) -> BTreeMap<PathBuf, PathBuf> {
    let mut out = BTreeMap::new();
    for file in files
        .iter()
        .filter(|path| file_name_ci(path, TUNING_DOCUMENT))
    {
        out.entry(parent_key(file)).or_insert_with(|| file.clone());
    }
    out
}

fn parent_key(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) => parent.to_path_buf(),
        None => PathBuf::new(),
    }
}

fn file_name_ci(path: &Path, expected: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(expected))
}

fn lower_file_name(path: &Path) -> String {
    match path.file_name().and_then(|name| name.to_str()) {
        Some(name) => name.to_ascii_lowercase(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_kind_maps_frontmatter_errors() {
        assert_eq!(
            diagnostic_kind(&FrontmatterError::MissingName),
            DiagnosticKind::MissingName
        );
    }
}

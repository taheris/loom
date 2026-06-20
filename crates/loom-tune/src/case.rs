use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use displaydoc::Display;
use loom_events::identifier::{BeadId, SpecLabel};
use loom_skills::identity::SkillName;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::checker::{CaseRole, CaseSchema, CheckerId, Registry, RegistryError};
use crate::target::{Catalog as TargetCatalog, Target, TargetCatalogError};

/// Globally unique kebab-case identifier for a `loom-case` block.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Id(String);

impl Id {
    pub fn new(value: impl Into<String>) -> Result<Self, ParseIdError> {
        value.into().parse()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Id {
    type Err = ParseIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let char_count = value.chars().count();
        if char_count == 0 || char_count > 96 {
            return Err(ParseIdError::Invalid {
                value: value.to_owned(),
            });
        }
        if !value.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
            || value.starts_with('-')
            || value.ends_with('-')
            || value.contains("--")
            || !value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(ParseIdError::Invalid {
                value: value.to_owned(),
            });
        }
        Ok(Self(value.to_owned()))
    }
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

pub type CaseId = Id;

/// Case id parse failures.
#[derive(Debug, Clone, PartialEq, Eq, Display, Error)]
pub enum ParseIdError {
    /// invalid loom-case id `{value}`: expected 1-96 char lowercase kebab-case starting with a letter
    Invalid { value: String },
}

/// Tuning markdown document kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum DocumentKind {
    Repo,
    Package { owning_skill: SkillName },
}

/// Tuning markdown document input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    path: PathBuf,
    markdown: String,
    kind: DocumentKind,
}

impl Document {
    pub fn repo(path: impl Into<PathBuf>, markdown: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            markdown: markdown.into(),
            kind: DocumentKind::Repo,
        }
    }

    pub fn package(
        path: impl Into<PathBuf>,
        owning_skill: SkillName,
        markdown: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            markdown: markdown.into(),
            kind: DocumentKind::Package { owning_skill },
        }
    }
}

/// Source location for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub path: PathBuf,
    pub line: usize,
}

/// Loaded tuning cases plus document metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoadedCases {
    cases: Vec<Case>,
    documents: Vec<LoadedDocument>,
}

impl LoadedCases {
    pub fn new(cases: Vec<Case>, documents: Vec<LoadedDocument>) -> Self {
        Self { cases, documents }
    }

    pub fn cases(&self) -> &[Case] {
        &self.cases
    }

    pub fn documents(&self) -> &[LoadedDocument] {
        &self.documents
    }
}

/// Loaded tuning document report row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadedDocument {
    pub path: PathBuf,
    pub kind: DocumentKind,
    pub case_count: usize,
}

/// Parsed and validated declared regression case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Case {
    pub id: Id,
    pub checker: CheckerId,
    pub targets: Vec<Target>,
    pub role: CaseRole,
    pub input: Input,
    pub expected: Expected,
    pub source: Source,
}

/// Checker-specific validated input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "schema")]
pub enum Input {
    ReviewFindingRecall {
        patch: RepoPath,
    },
    TodoDecomposition {
        prompt: RepoPath,
    },
    LoopVerifyAfterEdit {
        fixture: RepoPath,
        task: String,
    },
    LoopScopeDiscipline {
        fixture: RepoPath,
        task: String,
    },
    InboxResolutionPath {
        fixture: RepoPath,
        user_response: String,
    },
    TuneApplyHandoff {
        fixture: RepoPath,
        user_response: String,
    },
    AgentContextBeforeEdit {
        fixture: RepoPath,
        task: String,
    },
}

/// Checker-specific expected result schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "schema")]
pub enum Expected {
    ReviewFindingRecall(ReviewExpected),
    TodoDecomposition(TodoExpected),
    LoopVerifyAfterEdit(VerifyAfterEditExpected),
    LoopScopeDiscipline(ScopeDisciplineExpected),
    InboxResolutionPath(InboxResolutionExpected),
    TuneApplyHandoff(TuneApplyExpected),
    AgentContextBeforeEdit(ContextBeforeEditExpected),
}

/// Repository-contained case path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RepoPath {
    pub relative: PathBuf,
    pub kind: PathKind,
}

/// Repository path kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    File,
    Directory,
}

/// Context for loading `loom-case` blocks.
pub struct LoadContext<'a> {
    pub repo_root: &'a Path,
    pub tracked_files: &'a BTreeSet<PathBuf>,
    pub targets: &'a TargetCatalog,
    pub registry: &'a Registry,
    pub disabled_checkers: &'a BTreeSet<CheckerId>,
}

/// Parse and validate tuning documents.
pub fn load_documents(
    documents: &[Document],
    context: &LoadContext<'_>,
) -> Result<LoadedCases, LoadError> {
    let repo_root = canonicalize_context_path(context.repo_root)?;
    let tracked_files = normalize_tracked_files(context.tracked_files);
    let mut cases_by_id = BTreeMap::<Id, Source>::new();
    let mut cases = Vec::new();
    let mut loaded_documents = Vec::new();
    for document in documents {
        let document_path = resolve_document_path(&repo_root, &document.path, &tracked_files)?;
        let blocks = find_case_blocks(&document.markdown, &document_path);
        let mut case_count = 0;
        for block in blocks {
            let case =
                parse_case_block(&block, &document.kind, context, &repo_root, &tracked_files)?;
            if let Some(first) = cases_by_id.insert(case.id.clone(), case.source.clone()) {
                return Err(LoadError::DuplicateId {
                    id: case.id,
                    first,
                    second: block.source,
                });
            }
            cases.push(case);
            case_count += 1;
        }
        loaded_documents.push(LoadedDocument {
            path: document_path,
            kind: document.kind.clone(),
            case_count,
        });
    }
    cases.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(LoadedCases::new(cases, loaded_documents))
}

fn parse_case_block(
    block: &CaseBlock,
    document_kind: &DocumentKind,
    context: &LoadContext<'_>,
    repo_root: &Path,
    tracked_files: &BTreeSet<PathBuf>,
) -> Result<Case, LoadError> {
    let raw = toml::from_str::<RawCase>(&block.body).map_err(|source| LoadError::Toml {
        location: block.source.clone(),
        error: source.to_string(),
    })?;
    if raw.targets.is_empty() {
        return Err(LoadError::EmptyTargets {
            id: raw.id,
            location: block.source.clone(),
        });
    }
    for target in &raw.targets {
        context
            .targets
            .require_known(target)
            .map_err(|source| LoadError::TargetCatalog {
                id: raw.id.clone(),
                source,
            })?;
    }
    if let DocumentKind::Package { owning_skill } = document_kind {
        let owner = Target::Skill {
            name: owning_skill.clone(),
        };
        if !raw.targets.contains(&owner) {
            return Err(LoadError::MissingPackageOwnerTarget {
                id: raw.id,
                owning_skill: owning_skill.clone(),
                location: block.source.clone(),
            });
        }
    }
    let metadata = context
        .registry
        .require_case_checker(&raw.checker, context.disabled_checkers)
        .map_err(|source| LoadError::Checker {
            id: raw.id.clone(),
            source,
        })?;
    if !metadata.supports_role(raw.role) {
        return Err(LoadError::UnsupportedRole {
            id: raw.id,
            checker: raw.checker,
            role: raw.role,
            location: block.source.clone(),
        });
    }
    let schema = metadata.schema.ok_or_else(|| LoadError::MissingSchema {
        id: raw.id.clone(),
        checker: raw.checker.clone(),
        location: block.source.clone(),
    })?;
    let source_dir = block
        .source
        .path
        .parent()
        .ok_or_else(|| LoadError::NoSourceParent {
            location: block.source.clone(),
        })?;
    let schema_context = SchemaContext {
        id: &raw.id,
        checker: &raw.checker,
        source_dir,
        repo_root,
        tracked_files,
    };
    let (input, expected) = decode_schema(schema, raw.input, raw.expected, &schema_context)?;
    Ok(Case {
        id: raw.id,
        checker: raw.checker,
        targets: raw.targets,
        role: raw.role,
        input,
        expected,
        source: block.source.clone(),
    })
}

struct SchemaContext<'a> {
    id: &'a Id,
    checker: &'a CheckerId,
    source_dir: &'a Path,
    repo_root: &'a Path,
    tracked_files: &'a BTreeSet<PathBuf>,
}

fn decode_schema(
    schema: CaseSchema,
    input: Option<toml::Value>,
    expected: Option<toml::Value>,
    context: &SchemaContext<'_>,
) -> Result<(Input, Expected), LoadError> {
    match schema {
        CaseSchema::ReviewFindingRecall => {
            let raw_input = decode_table::<ReviewInput>(input, "input", context)?;
            let raw_expected = decode_table::<ReviewExpected>(expected, "expected", context)?;
            let patch = resolve_schema_path(&raw_input.patch, context, PathKind::File)?;
            Ok((
                Input::ReviewFindingRecall { patch },
                Expected::ReviewFindingRecall(raw_expected),
            ))
        }
        CaseSchema::TodoDecomposition => {
            let raw_input = decode_table::<TodoInput>(input, "input", context)?;
            let raw_expected = decode_table::<TodoExpected>(expected, "expected", context)?;
            let prompt = resolve_schema_path(&raw_input.prompt, context, PathKind::File)?;
            Ok((
                Input::TodoDecomposition { prompt },
                Expected::TodoDecomposition(raw_expected),
            ))
        }
        CaseSchema::LoopVerifyAfterEdit => {
            let raw_input = decode_table::<FixtureTaskInput>(input, "input", context)?;
            let raw_expected =
                decode_table::<VerifyAfterEditExpected>(expected, "expected", context)?;
            let fixture = resolve_schema_path(&raw_input.fixture, context, PathKind::Directory)?;
            Ok((
                Input::LoopVerifyAfterEdit {
                    fixture,
                    task: raw_input.task,
                },
                Expected::LoopVerifyAfterEdit(raw_expected),
            ))
        }
        CaseSchema::LoopScopeDiscipline => {
            let raw_input = decode_table::<FixtureTaskInput>(input, "input", context)?;
            let raw_expected =
                decode_table::<ScopeDisciplineExpected>(expected, "expected", context)?;
            let fixture = resolve_schema_path(&raw_input.fixture, context, PathKind::Directory)?;
            Ok((
                Input::LoopScopeDiscipline {
                    fixture,
                    task: raw_input.task,
                },
                Expected::LoopScopeDiscipline(raw_expected),
            ))
        }
        CaseSchema::InboxResolutionPath => {
            let raw_input = decode_table::<FixtureResponseInput>(input, "input", context)?;
            let raw_expected =
                decode_table::<InboxResolutionExpected>(expected, "expected", context)?;
            let fixture = resolve_schema_path(&raw_input.fixture, context, PathKind::Directory)?;
            Ok((
                Input::InboxResolutionPath {
                    fixture,
                    user_response: raw_input.user_response,
                },
                Expected::InboxResolutionPath(raw_expected),
            ))
        }
        CaseSchema::TuneApplyHandoff => {
            let raw_input = decode_table::<FixtureResponseInput>(input, "input", context)?;
            let raw_expected = decode_table::<TuneApplyExpected>(expected, "expected", context)?;
            let fixture = resolve_schema_path(&raw_input.fixture, context, PathKind::Directory)?;
            Ok((
                Input::TuneApplyHandoff {
                    fixture,
                    user_response: raw_input.user_response,
                },
                Expected::TuneApplyHandoff(raw_expected),
            ))
        }
        CaseSchema::AgentContextBeforeEdit => {
            let raw_input = decode_table::<FixtureTaskInput>(input, "input", context)?;
            let raw_expected =
                decode_table::<ContextBeforeEditExpected>(expected, "expected", context)?;
            let fixture = resolve_schema_path(&raw_input.fixture, context, PathKind::Directory)?;
            Ok((
                Input::AgentContextBeforeEdit {
                    fixture,
                    task: raw_input.task,
                },
                Expected::AgentContextBeforeEdit(raw_expected),
            ))
        }
    }
}

fn decode_table<T: DeserializeOwned>(
    value: Option<toml::Value>,
    table: &'static str,
    context: &SchemaContext<'_>,
) -> Result<T, LoadError> {
    let value = value.ok_or_else(|| LoadError::MissingTable {
        id: context.id.clone(),
        checker: context.checker.clone(),
        table,
    })?;
    value
        .try_into()
        .map_err(|source: toml::de::Error| LoadError::InvalidSchema {
            id: context.id.clone(),
            checker: context.checker.clone(),
            message: source.to_string(),
        })
}

fn resolve_schema_path(
    raw: &str,
    context: &SchemaContext<'_>,
    kind: PathKind,
) -> Result<RepoPath, LoadError> {
    resolve_case_path(
        raw,
        context.id,
        context.source_dir,
        context.repo_root,
        context.tracked_files,
        kind,
    )
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCase {
    id: Id,
    checker: CheckerId,
    targets: Vec<Target>,
    #[serde(default)]
    role: CaseRole,
    input: Option<toml::Value>,
    expected: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewInput {
    patch: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewExpected {
    pub findings: Vec<ReviewFinding>,
    pub max_extra_findings: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewFinding {
    pub contains: Vec<String>,
    pub file: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct TodoInput {
    prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TodoExpected {
    pub min_items: u32,
    pub max_items: u32,
    pub required_specs: Vec<SpecLabel>,
    pub forbidden_specs: Vec<SpecLabel>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureTaskInput {
    fixture: String,
    task: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureResponseInput {
    fixture: String,
    user_response: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyAfterEditExpected {
    pub edited_paths: Vec<String>,
    pub verify_commands: Vec<String>,
    pub marker: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeDisciplineExpected {
    pub allowed_edit_paths: Vec<String>,
    pub forbidden_edit_paths: Vec<String>,
    pub max_changed_files: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxResolutionExpected {
    pub forbidden_commands: Vec<String>,
    pub allowed_terminal_markers: Vec<String>,
    pub must_update_beads: bool,
    pub must_not_push: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TuneApplyExpected {
    pub apply_proposals: Vec<BeadId>,
    pub must_emit_apply: bool,
    pub must_not_push: bool,
    pub must_not_dirty_integration: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextBeforeEditExpected {
    pub must_read_before_edit: Vec<String>,
    pub edited_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CaseBlock {
    body: String,
    source: Source,
}

fn find_case_blocks(markdown: &str, path: &Path) -> Vec<CaseBlock> {
    let mut blocks = Vec::new();
    let mut open: Option<OpenFence> = None;
    for (line_index, line) in markdown.lines().enumerate() {
        let line_number = line_index + 1;
        if let Some(fence) = open.as_mut() {
            if is_closing_fence(line.trim_start(), fence.marker, fence.len) {
                if fence.is_case {
                    blocks.push(CaseBlock {
                        body: fence.body.join("\n"),
                        source: Source {
                            path: path.to_path_buf(),
                            line: fence.start_line,
                        },
                    });
                }
                open = None;
            } else if fence.is_case {
                fence.body.push(line.to_owned());
            }
            continue;
        }
        if let Some(fence) = parse_opening_fence(line.trim_start(), line_number) {
            open = Some(fence);
        }
    }
    blocks
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenFence {
    marker: char,
    len: usize,
    is_case: bool,
    start_line: usize,
    body: Vec<String>,
}

fn parse_opening_fence(line: &str, line_number: usize) -> Option<OpenFence> {
    let marker = line.chars().next()?;
    if marker != '`' && marker != '~' {
        return None;
    }
    let len = line.chars().take_while(|ch| *ch == marker).count();
    if len < 3 {
        return None;
    }
    let info = line[len..].trim();
    Some(OpenFence {
        marker,
        len,
        is_case: info == "loom-case",
        start_line: line_number,
        body: Vec::new(),
    })
}

fn is_closing_fence(line: &str, marker: char, opening_len: usize) -> bool {
    let len = line.chars().take_while(|ch| *ch == marker).count();
    len >= opening_len && line[len..].trim().is_empty()
}

fn canonicalize_context_path(path: &Path) -> Result<PathBuf, LoadError> {
    fs::canonicalize(path).map_err(|source| LoadError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn resolve_document_path(
    repo_root: &Path,
    path: &Path,
    tracked_files: &BTreeSet<PathBuf>,
) -> Result<PathBuf, LoadError> {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_root.join(path)
    };
    let canonical = fs::canonicalize(&candidate).map_err(|source| LoadError::Io {
        path: candidate.clone(),
        source,
    })?;
    let relative = relative_to_repo(repo_root, &canonical)?;
    ensure_not_under_loom(&relative)?;
    ensure_tracked(&relative, tracked_files)?;
    Ok(canonical)
}

fn resolve_case_path(
    raw: &str,
    id: &Id,
    source_dir: &Path,
    repo_root: &Path,
    tracked_files: &BTreeSet<PathBuf>,
    kind: PathKind,
) -> Result<RepoPath, LoadError> {
    let raw_path = Path::new(raw);
    if raw.is_empty() || raw_path.is_absolute() {
        return Err(LoadError::InvalidCasePath {
            id: id.clone(),
            raw: raw.to_owned(),
            reason: "path must be relative".to_owned(),
        });
    }
    let candidate = source_dir.join(raw_path);
    let canonical = fs::canonicalize(&candidate).map_err(|source| LoadError::Io {
        path: candidate.clone(),
        source,
    })?;
    let relative = relative_to_repo(repo_root, &canonical).map_err(|source| match source {
        LoadError::PathEscapesRepo { path } => LoadError::InvalidCasePath {
            id: id.clone(),
            raw: raw.to_owned(),
            reason: format!("path `{}` escapes repo root", path.display()),
        },
        other => other,
    })?;
    ensure_not_under_loom(&relative).map_err(|source| match source {
        LoadError::PathUnderLoom { path } => LoadError::InvalidCasePath {
            id: id.clone(),
            raw: raw.to_owned(),
            reason: format!("path `{}` is under .loom", path.display()),
        },
        other => other,
    })?;
    match kind {
        PathKind::File => validate_file_path(id, raw, &canonical, &relative, tracked_files)?,
        PathKind::Directory => {
            validate_directory_path(id, raw, repo_root, &canonical, tracked_files)?
        }
    }
    Ok(RepoPath { relative, kind })
}

fn validate_file_path(
    id: &Id,
    raw: &str,
    canonical: &Path,
    relative: &Path,
    tracked_files: &BTreeSet<PathBuf>,
) -> Result<(), LoadError> {
    if !canonical.is_file() {
        return Err(LoadError::InvalidCasePath {
            id: id.clone(),
            raw: raw.to_owned(),
            reason: "path must be a file".to_owned(),
        });
    }
    fs::File::open(canonical).map_err(|source| LoadError::Io {
        path: canonical.to_path_buf(),
        source,
    })?;
    if !tracked_files.contains(relative) {
        return Err(LoadError::InvalidCasePath {
            id: id.clone(),
            raw: raw.to_owned(),
            reason: format!("file `{}` is not git-tracked", relative.display()),
        });
    }
    Ok(())
}

fn validate_directory_path(
    id: &Id,
    raw: &str,
    repo_root: &Path,
    canonical: &Path,
    tracked_files: &BTreeSet<PathBuf>,
) -> Result<(), LoadError> {
    if !canonical.is_dir() {
        return Err(LoadError::InvalidCasePath {
            id: id.clone(),
            raw: raw.to_owned(),
            reason: "path must be a directory".to_owned(),
        });
    }
    validate_directory_entries(id, raw, repo_root, canonical, tracked_files)
}

fn validate_directory_entries(
    id: &Id,
    raw: &str,
    repo_root: &Path,
    directory: &Path,
    tracked_files: &BTreeSet<PathBuf>,
) -> Result<(), LoadError> {
    for entry in fs::read_dir(directory).map_err(|source| LoadError::Io {
        path: directory.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| LoadError::Io {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let canonical = fs::canonicalize(&path).map_err(|source| LoadError::Io {
            path: path.clone(),
            source,
        })?;
        let relative = relative_to_repo(repo_root, &canonical).map_err(|source| match source {
            LoadError::PathEscapesRepo { path } => LoadError::InvalidCasePath {
                id: id.clone(),
                raw: raw.to_owned(),
                reason: format!("path `{}` escapes repo root", path.display()),
            },
            other => other,
        })?;
        ensure_not_under_loom(&relative).map_err(|source| match source {
            LoadError::PathUnderLoom { path } => LoadError::InvalidCasePath {
                id: id.clone(),
                raw: raw.to_owned(),
                reason: format!("path `{}` is under .loom", path.display()),
            },
            other => other,
        })?;
        let metadata = fs::metadata(&canonical).map_err(|source| LoadError::Io {
            path: canonical.clone(),
            source,
        })?;
        if metadata.is_dir() {
            validate_directory_entries(id, raw, repo_root, &canonical, tracked_files)?;
        } else if metadata.is_file() {
            fs::File::open(&canonical).map_err(|source| LoadError::Io {
                path: canonical.clone(),
                source,
            })?;
            if !tracked_files.contains(&relative) {
                return Err(LoadError::InvalidCasePath {
                    id: id.clone(),
                    raw: raw.to_owned(),
                    reason: format!("file `{}` is not git-tracked", relative.display()),
                });
            }
        } else {
            return Err(LoadError::InvalidCasePath {
                id: id.clone(),
                raw: raw.to_owned(),
                reason: format!("path `{}` is not a file or directory", relative.display()),
            });
        }
    }
    Ok(())
}

fn relative_to_repo(repo_root: &Path, path: &Path) -> Result<PathBuf, LoadError> {
    let relative = path
        .strip_prefix(repo_root)
        .map_err(|_| LoadError::PathEscapesRepo {
            path: path.to_path_buf(),
        })?;
    Ok(normalize_relative(relative))
}

fn ensure_not_under_loom(relative: &Path) -> Result<(), LoadError> {
    if relative
        .components()
        .next()
        .is_some_and(|component| component.as_os_str() == ".loom")
    {
        Err(LoadError::PathUnderLoom {
            path: relative.to_path_buf(),
        })
    } else {
        Ok(())
    }
}

fn ensure_tracked(relative: &Path, tracked_files: &BTreeSet<PathBuf>) -> Result<(), LoadError> {
    if tracked_files.contains(relative) {
        Ok(())
    } else {
        Err(LoadError::DocumentNotTracked {
            path: relative.to_path_buf(),
        })
    }
}

fn normalize_tracked_files(paths: &BTreeSet<PathBuf>) -> BTreeSet<PathBuf> {
    paths.iter().map(|path| normalize_relative(path)).collect()
}

fn normalize_relative(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => normalized.push(".."),
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

/// Tuning document load failures.
#[derive(Debug, Error, Display)]
pub enum LoadError {
    /// failed to read path `{path}`
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// tuning document path `{path}` is not git-tracked
    DocumentNotTracked { path: PathBuf },
    /// path `{path}` escapes the repository root
    PathEscapesRepo { path: PathBuf },
    /// path `{path}` resolves under `.loom`
    PathUnderLoom { path: PathBuf },
    /// failed to parse loom-case at `{location:?}`: {error}
    Toml { location: Source, error: String },
    /// duplicate loom-case id `{id}` first seen at `{first:?}` and repeated at `{second:?}`
    DuplicateId {
        id: Id,
        first: Source,
        second: Source,
    },
    /// loom-case `{id}` has an empty targets array
    EmptyTargets { id: Id, location: Source },
    /// loom-case `{id}` references an invalid target
    TargetCatalog {
        id: Id,
        #[source]
        source: TargetCatalogError,
    },
    /// package loom-case `{id}` is missing owning skill target `skill:{owning_skill}`
    MissingPackageOwnerTarget {
        id: Id,
        owning_skill: SkillName,
        location: Source,
    },
    /// loom-case `{id}` references an invalid checker
    Checker {
        id: Id,
        #[source]
        source: RegistryError,
    },
    /// loom-case `{id}` role `{role:?}` is not supported by checker `{checker}`
    UnsupportedRole {
        id: Id,
        checker: CheckerId,
        role: CaseRole,
        location: Source,
    },
    /// loom-case `{id}` checker `{checker}` has no case schema
    MissingSchema {
        id: Id,
        checker: CheckerId,
        location: Source,
    },
    /// loom-case source `{location:?}` has no parent directory
    NoSourceParent { location: Source },
    /// loom-case `{id}` is missing `{table}` table for checker `{checker}`
    MissingTable {
        id: Id,
        checker: CheckerId,
        table: &'static str,
    },
    /// loom-case `{id}` schema for checker `{checker}` is invalid: {message}
    InvalidSchema {
        id: Id,
        checker: CheckerId,
        message: String,
    },
    /// loom-case `{id}` path `{raw}` is invalid: {reason}
    InvalidCasePath { id: Id, raw: String, reason: String },
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::checker::Registry;
    use crate::target::{Catalog as TargetCatalog, Target};

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, body).expect("write file");
    }

    fn tracked(paths: &[&str]) -> BTreeSet<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    fn target_catalog() -> TargetCatalog {
        TargetCatalog::new([
            "skill:loom-review-finding-recall"
                .parse::<Target>()
                .expect("skill target"),
            "phase:review".parse::<Target>().expect("phase target"),
        ])
    }

    fn valid_markdown() -> String {
        r#"
````markdown
```loom-case
id = "ignored-example"
```
````

```loom-case
id = "review-recall-case"
checker = "behavior.review.finding-recall"
targets = ["skill:loom-review-finding-recall", "phase:review"]

[input]
patch = "cases/review.diff"

[expected]
max_extra_findings = 2

[[expected.findings]]
contains = ["missing test"]
file = "src/lib.rs"
```
"#
        .to_owned()
    }

    fn load(
        repo: &Path,
        markdown: String,
        tracked_files: &BTreeSet<PathBuf>,
    ) -> Result<LoadedCases, LoadError> {
        let registry = Registry::builtin().expect("registry");
        let disabled = BTreeSet::new();
        let targets = target_catalog();
        let document = Document::repo(repo.join("docs/tuning.md"), markdown);
        load_documents(
            &[document],
            &LoadContext {
                repo_root: repo,
                tracked_files,
                targets: &targets,
                registry: &registry,
                disabled_checkers: &disabled,
            },
        )
    }

    #[test]
    fn case_id_enforces_tuning_case_id_contract() {
        assert!(Id::new("review-recall-1").is_ok());
        let too_long = format!("a{}", "b".repeat(96));
        for input in [
            "",
            "1-starts-digit",
            "Upper",
            "double--dash",
            "trailing-",
            &too_long,
        ] {
            assert!(Id::new(input).is_err(), "{input}");
        }
    }

    #[test]
    fn loom_case_parser_ignores_nested_examples_and_validates_schema() {
        let repo = TempDir::new().expect("tempdir");
        write(&repo.path().join("docs/tuning.md"), &valid_markdown());
        write(&repo.path().join("docs/cases/review.diff"), "diff --git\n");
        let loaded = load(
            repo.path(),
            valid_markdown(),
            &tracked(&["docs/tuning.md", "docs/cases/review.diff"]),
        )
        .expect("loads");
        assert_eq!(loaded.cases().len(), 1);
        assert_eq!(loaded.cases()[0].id.as_str(), "review-recall-case");
    }

    #[test]
    fn loom_case_rejects_unknown_top_level_fields() {
        let repo = TempDir::new().expect("tempdir");
        let markdown = valid_markdown().replace(
            "targets = [\"skill:loom-review-finding-recall\", \"phase:review\"]",
            "targets = [\"skill:loom-review-finding-recall\", \"phase:review\"]\nunknown = true",
        );
        write(&repo.path().join("docs/tuning.md"), &markdown);
        write(&repo.path().join("docs/cases/review.diff"), "diff --git\n");
        let err = load(
            repo.path(),
            markdown,
            &tracked(&["docs/tuning.md", "docs/cases/review.diff"]),
        )
        .expect_err("unknown field rejects");
        assert!(matches!(err, LoadError::Toml { .. }));
    }

    #[test]
    fn loom_case_rejects_duplicate_ids_across_documents() {
        let repo = TempDir::new().expect("tempdir");
        write(&repo.path().join("docs/tuning.md"), &valid_markdown());
        write(&repo.path().join("docs/other.md"), &valid_markdown());
        write(&repo.path().join("docs/cases/review.diff"), "diff --git\n");
        let registry = Registry::builtin().expect("registry");
        let disabled = BTreeSet::new();
        let targets = target_catalog();
        let docs = [
            Document::repo(repo.path().join("docs/tuning.md"), valid_markdown()),
            Document::repo(repo.path().join("docs/other.md"), valid_markdown()),
        ];
        let err = load_documents(
            &docs,
            &LoadContext {
                repo_root: repo.path(),
                tracked_files: &tracked(&[
                    "docs/tuning.md",
                    "docs/other.md",
                    "docs/cases/review.diff",
                ]),
                targets: &targets,
                registry: &registry,
                disabled_checkers: &disabled,
            },
        )
        .expect_err("duplicate rejects");
        assert!(matches!(err, LoadError::DuplicateId { .. }));
    }

    #[test]
    fn loom_case_rejects_untracked_or_escaped_paths() {
        let repo = TempDir::new().expect("tempdir");
        write(&repo.path().join("docs/tuning.md"), &valid_markdown());
        write(&repo.path().join("docs/cases/review.diff"), "diff --git\n");
        let err = load(repo.path(), valid_markdown(), &tracked(&["docs/tuning.md"]))
            .expect_err("untracked path rejects");
        assert!(matches!(err, LoadError::InvalidCasePath { .. }));

        let outside = TempDir::new().expect("outside");
        write(&outside.path().join("escape.diff"), "diff --git\n");
        let escaped = valid_markdown().replace(
            "cases/review.diff",
            outside
                .path()
                .join("escape.diff")
                .to_str()
                .expect("utf8 path"),
        );
        let err = load(
            repo.path(),
            escaped,
            &tracked(&["docs/tuning.md", "docs/cases/review.diff"]),
        )
        .expect_err("absolute path rejects");
        assert!(matches!(err, LoadError::InvalidCasePath { .. }));
    }

    #[test]
    fn package_tuning_case_requires_owning_skill_target() {
        let repo = TempDir::new().expect("tempdir");
        let markdown = valid_markdown().replace(
            "targets = [\"skill:loom-review-finding-recall\", \"phase:review\"]",
            "targets = [\"phase:review\"]",
        );
        write(&repo.path().join("skills/review/tuning.md"), &markdown);
        write(
            &repo.path().join("skills/review/cases/review.diff"),
            "diff --git\n",
        );
        let registry = Registry::builtin().expect("registry");
        let disabled = BTreeSet::new();
        let targets = target_catalog();
        let doc = Document::package(
            repo.path().join("skills/review/tuning.md"),
            SkillName::new("loom-review-finding-recall").expect("skill"),
            markdown,
        );
        let err = load_documents(
            &[doc],
            &LoadContext {
                repo_root: repo.path(),
                tracked_files: &tracked(&[
                    "skills/review/tuning.md",
                    "skills/review/cases/review.diff",
                ]),
                targets: &targets,
                registry: &registry,
                disabled_checkers: &disabled,
            },
        )
        .expect_err("owner target required");
        assert!(matches!(err, LoadError::MissingPackageOwnerTarget { .. }));
    }

    #[test]
    fn retired_behavior_checker_is_a_case_error() {
        let repo = TempDir::new().expect("tempdir");
        let markdown = valid_markdown().replace(
            "behavior.review.finding-recall",
            "behavior.review.legacy-recall",
        );
        write(&repo.path().join("docs/tuning.md"), &markdown);
        write(&repo.path().join("docs/cases/review.diff"), "diff --git\n");
        let err = load(
            repo.path(),
            markdown,
            &tracked(&["docs/tuning.md", "docs/cases/review.diff"]),
        )
        .expect_err("retired checker rejects case");
        assert!(matches!(err, LoadError::Checker { .. }));
    }

    #[test]
    fn unknown_target_is_a_case_error() {
        let repo = TempDir::new().expect("tempdir");
        let markdown = valid_markdown().replace("phase:review", "phase:unknown");
        write(&repo.path().join("docs/tuning.md"), &markdown);
        write(&repo.path().join("docs/cases/review.diff"), "diff --git\n");
        let err = load(
            repo.path(),
            markdown,
            &tracked(&["docs/tuning.md", "docs/cases/review.diff"]),
        )
        .expect_err("unknown target rejects case");
        assert!(matches!(err, LoadError::TargetCatalog { .. }));
    }

    #[test]
    fn disabled_behavior_checker_is_a_case_error() {
        let repo = TempDir::new().expect("tempdir");
        write(&repo.path().join("docs/tuning.md"), &valid_markdown());
        write(&repo.path().join("docs/cases/review.diff"), "diff --git\n");
        let registry = Registry::builtin().expect("registry");
        let disabled = registry
            .validate_disabled(&[CheckerId::new("behavior.review.finding-recall").expect("id")])
            .expect("disabled");
        let targets = target_catalog();
        let doc = Document::repo(repo.path().join("docs/tuning.md"), valid_markdown());
        let err = load_documents(
            &[doc],
            &LoadContext {
                repo_root: repo.path(),
                tracked_files: &tracked(&["docs/tuning.md", "docs/cases/review.diff"]),
                targets: &targets,
                registry: &registry,
                disabled_checkers: &disabled,
            },
        )
        .expect_err("disabled checker rejects case");
        assert!(matches!(err, LoadError::Checker { .. }));
    }
}

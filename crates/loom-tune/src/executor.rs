use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;

use displaydoc::Display;
use loom_events::AgentEvent;
use loom_protocol::gate::{DispatchScope, FindingValidator, parse_exit_signal, parse_walk_output};
use loom_protocol::inbox::{self, TerminalMarker};
use thiserror::Error;

use crate::case::{Case, Expected, LoadedCases};
use crate::checker::{Registry, RegistryError};
use crate::gate::{CaseResult, Scores};
use crate::plan::{FrozenPlan, PlannedCaseId, SelectedCase};
use crate::score::{Score, ScoreError};
use crate::target::Target;

#[cfg(test)]
mod tests;
mod trace;

use trace::{Action, operations};

/// Current and candidate text for one tuned artifact; never behavioral evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub target: Target,
    pub current: String,
    pub candidate: String,
}

impl Artifact {
    pub fn new(target: Target, current: impl Into<String>, candidate: impl Into<String>) -> Self {
        Self {
            target,
            current: current.into(),
            candidate: candidate.into(),
        }
    }
}

/// Driver-captured events and final byte/permission changes, independent of Git commits.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub events: Vec<AgentEvent>,
    pub changed_paths: BTreeSet<String>,
    pub workspace: PathBuf,
}

/// A terminal transcript, optionally accompanied by actual replay observations.
#[derive(Debug, Clone)]
pub struct Evidence {
    pub output: String,
    pub recorded: Option<Recorded>,
}

impl Evidence {
    pub fn text(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            recorded: None,
        }
    }

    fn changes(&self) -> Result<&BTreeSet<String>, Error> {
        self.recorded
            .as_ref()
            .map(|recorded| &recorded.changed_paths)
            .ok_or_else(|| unavailable("final repository changes were not captured"))
    }
}

/// Captured current/candidate replay evidence for one frozen case.
#[derive(Debug, Clone)]
pub struct Replay {
    pub case_id: PlannedCaseId,
    pub current: Evidence,
    pub candidate: Evidence,
}

impl Replay {
    pub const fn new(case_id: PlannedCaseId, current: Evidence, candidate: Evidence) -> Self {
        Self {
            case_id,
            current,
            candidate,
        }
    }
}

/// Reject selections without a checkable expected-result oracle before launching agents.
///
/// # Errors
/// Mined text has no behavioral oracle and cannot be evaluated as a regression case.
pub fn require_checkable(plan: &FrozenPlan) -> Result<(), Error> {
    for selected in &plan.selected_cases {
        if matches!(selected.case_id, PlannedCaseId::Mined(_)) {
            return Err(Error::UncheckableSelection {
                case_id: selected.case_id.clone(),
            });
        }
    }
    Ok(())
}

/// Score actual replay evidence, never claims about actions in final-answer prose.
///
/// # Errors
/// Returns an error for missing, mismatched, or unsupported evidence and registry failures.
pub fn run(
    plan: &FrozenPlan,
    cases: &LoadedCases,
    replays: &[Replay],
    registry: &Registry,
    finding_validator: &dyn FindingValidator,
) -> Result<Vec<CaseResult>, Error> {
    require_checkable(plan)?;
    let case_by_id = cases
        .cases()
        .iter()
        .map(|case| (case.id.clone(), case))
        .collect::<BTreeMap<_, _>>();
    let mut replay_by_id = BTreeMap::new();
    for replay in replays {
        if replay_by_id.insert(&replay.case_id, replay).is_some() {
            return Err(unavailable("duplicate replay identity"));
        }
    }
    let mut results = Vec::with_capacity(plan.selected_cases.len());
    for selected in &plan.selected_cases {
        let PlannedCaseId::Declared(id) = &selected.case_id else {
            return Err(Error::UncheckableSelection {
                case_id: selected.case_id.clone(),
            });
        };
        let case = case_by_id
            .get(id)
            .ok_or_else(|| Error::MissingDeclaredCase {
                case_id: selected.case_id.clone(),
            })?;
        let replay = replay_by_id
            .get(&selected.case_id)
            .ok_or_else(|| Error::MissingReplay {
                case_id: selected.case_id.clone(),
            })?;
        results.push(run_declared(
            selected,
            case,
            replay,
            registry,
            finding_validator,
        )?);
    }
    Ok(results)
}

fn run_declared(
    selected: &SelectedCase,
    case: &Case,
    replay: &Replay,
    registry: &Registry,
    validator: &dyn FindingValidator,
) -> Result<CaseResult, Error> {
    registry.require_active(&case.checker)?;
    if selected.checker != case.checker {
        return Err(unavailable("loaded checker differs from frozen selection"));
    }
    Ok(CaseResult::new(
        selected,
        score(&replay.current, &case.expected, validator)?,
        score(&replay.candidate, &case.expected, validator)?,
    ))
}

fn score(
    evidence: &Evidence,
    expected: &Expected,
    validator: &dyn FindingValidator,
) -> Result<Scores, Error> {
    match expected {
        Expected::ReviewFindingRecall(expected) => {
            score_review(&evidence.output, expected, validator)
        }
        Expected::TodoDecomposition(expected) => score_todo(&evidence.output, expected),
        Expected::LoopScopeDiscipline(expected) => {
            let changes = evidence.changes()?;
            let allowed = patterns(&expected.allowed_edit_paths)?;
            let forbidden = patterns(&expected.forbidden_edit_paths)?;
            binary_score(
                complete(&evidence.output)
                    && !changes.is_empty()
                    && changes.len() <= expected.max_changed_files as usize
                    && changes.iter().all(|path| {
                        matches_path(&allowed, path) && !matches_path(&forbidden, path)
                    }),
            )
        }
        Expected::LoopVerifyAfterEdit(expected) => {
            if expected.marker != "LOOM_COMPLETE" {
                return Err(unavailable(
                    "verify-after-edit only supports the loop completion protocol",
                ));
            }
            let edits = patterns(&expected.edited_paths)?;
            let trace = operations(evidence)?;
            require_edit_coverage(evidence, &edits, &trace)?;
            for operation in &trace {
                if let Action::Command(command) = &operation.action
                    && !expected
                        .verify_commands
                        .iter()
                        .any(|expected| command.trim() == expected.trim())
                {
                    return Err(unavailable(
                        "opaque shell command may conceal an edit or verifier",
                    ));
                }
            }
            let last_edit = trace.iter().filter(|operation| operation.success && matches!(&operation.action, Action::Edit(path) if matches_path(&edits, path)))
                .map(|operation| operation.end).max();
            let verified = last_edit.is_some_and(|last| !expected.verify_commands.is_empty() && expected.verify_commands.iter().all(|command| {
                trace.iter().any(|operation| operation.success && operation.start > last && matches!(&operation.action, Action::Command(actual) if actual.trim() == command.trim()))
            }));
            binary_score(
                complete(&evidence.output) && expected_changes(evidence, &edits)? && verified,
            )
        }
        Expected::AgentContextBeforeEdit(expected) => {
            let edits = patterns(&expected.edited_paths)?;
            let trace = operations(evidence)?;
            require_edit_coverage(evidence, &edits, &trace)?;
            if trace
                .iter()
                .any(|operation| matches!(operation.action, Action::Command(_)))
            {
                return Err(unavailable(
                    "opaque shell commands cannot establish read/edit ordering",
                ));
            }
            let first_edit = trace.iter().filter(|operation| matches!(&operation.action, Action::Edit(path) if matches_path(&edits, path)))
                .map(|operation| operation.start).min();
            let read_first = first_edit.is_some_and(|first| !expected.must_read_before_edit.is_empty() && expected.must_read_before_edit.iter().all(|path| {
                trace.iter().any(|operation| operation.success && operation.end < first && matches!(&operation.action, Action::Read(actual) if actual == path))
            }));
            binary_score(
                complete(&evidence.output) && expected_changes(evidence, &edits)? && read_first,
            )
        }
        Expected::InboxResolutionPath(expected) => {
            let Ok(marker) = inbox::parse(&evidence.output) else {
                return binary_score(false);
            };
            let marker_name = match marker {
                TerminalMarker::Complete => "LOOM_COMPLETE",
                TerminalMarker::Apply { .. } => "LOOM_APPLY",
            };
            if !expected
                .allowed_terminal_markers
                .iter()
                .any(|allowed| allowed == marker_name)
            {
                return binary_score(false);
            }
            if !safe_commands(
                evidence,
                &expected.forbidden_commands,
                expected.must_not_push,
            )? {
                return binary_score(false);
            }
            if expected.must_update_beads {
                return Err(unavailable(
                    "isolated Beads state transitions are not captured; a bd command or text claim is insufficient",
                ));
            }
            binary_score(true)
        }
        Expected::TuneApplyHandoff(expected) => {
            let Ok(marker) = inbox::parse(&evidence.output) else {
                return binary_score(false);
            };
            let correct_marker = match marker {
                TerminalMarker::Complete => {
                    !expected.must_emit_apply && expected.apply_proposals.is_empty()
                }
                TerminalMarker::Apply { proposals } => {
                    proposals.iter().collect::<HashSet<_>>()
                        == expected.apply_proposals.iter().collect::<HashSet<_>>()
                }
            };
            if !correct_marker || !safe_commands(evidence, &[], expected.must_not_push)? {
                return binary_score(false);
            }
            if expected.must_not_dirty_integration
                && evidence.changes()?.iter().any(|path| {
                    path == ".loom/integration" || path.starts_with(".loom/integration/")
                })
            {
                return binary_score(false);
            }
            binary_score(true)
        }
    }
}

fn safe_commands(
    evidence: &Evidence,
    forbidden: &[String],
    must_not_push: bool,
) -> Result<bool, Error> {
    if forbidden.is_empty() && !must_not_push {
        return Ok(true);
    }
    let trace = operations(evidence)?;
    let commands = trace
        .iter()
        .filter_map(|operation| match &operation.action {
            Action::Command(command) => Some(command),
            _ => None,
        })
        .collect::<Vec<_>>();
    if commands.iter().any(|command| {
        forbidden
            .iter()
            .any(|forbidden| command.contains(forbidden))
            || (must_not_push && command.contains("git push"))
    }) {
        return Ok(false);
    }
    if !commands.is_empty() {
        return Err(unavailable(
            "opaque shell execution cannot prove absence of forbidden commands or pushes",
        ));
    }
    Ok(true)
}

fn require_edit_coverage(
    evidence: &Evidence,
    expected: &[glob::Pattern],
    trace: &[trace::Operation],
) -> Result<(), Error> {
    for path in evidence
        .changes()?
        .iter()
        .filter(|path| matches_path(expected, path))
    {
        if !trace.iter().any(|operation| {
            operation.success && matches!(&operation.action, Action::Edit(actual) if actual == path)
        }) {
            return Err(unavailable(
                "repository mutation has no successful edit event; ordering is unavailable",
            ));
        }
    }
    Ok(())
}

fn expected_changes(evidence: &Evidence, expected: &[glob::Pattern]) -> Result<bool, Error> {
    let changes = evidence.changes()?;
    Ok(!expected.is_empty()
        && expected.iter().all(|pattern| {
            changes
                .iter()
                .any(|path| matches_path(std::slice::from_ref(pattern), path))
        }))
}

fn patterns(patterns: &[String]) -> Result<Vec<glob::Pattern>, Error> {
    patterns
        .iter()
        .map(|pattern| {
            glob::Pattern::new(pattern)
                .map_err(|source| unavailable(format!("invalid expected path glob: {source}")))
        })
        .collect()
}

fn matches_path(patterns: &[glob::Pattern], path: &str) -> bool {
    patterns.iter().any(|pattern| {
        pattern.matches_with(
            path,
            glob::MatchOptions {
                case_sensitive: true,
                require_literal_separator: true,
                require_literal_leading_dot: false,
            },
        )
    })
}

fn complete(output: &str) -> bool {
    matches!(inbox::parse(output), Ok(TerminalMarker::Complete))
}

fn score_todo(output: &str, expected: &crate::case::TodoExpected) -> Result<Scores, Error> {
    let lines = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let Some((terminal, before)) = lines.split_last() else {
        return binary_score(false);
    };
    if before.iter().any(|line| line.starts_with("LOOM_")) {
        return binary_score(false);
    }
    let Ok(todo) = loom_protocol::todo::parse_todo_success(terminal) else {
        return binary_score(false);
    };
    let specs = todo
        .specs
        .iter()
        .map(|spec| &spec.label)
        .collect::<HashSet<_>>();
    let items = todo
        .specs
        .iter()
        .filter_map(|spec| match &spec.outcome {
            loom_protocol::todo::TodoSpecOutcome::Decomposed { beads } => Some(beads.len()),
            loom_protocol::todo::TodoSpecOutcome::NoWork { .. } => None,
        })
        .sum::<usize>();
    binary_score(
        items >= expected.min_items as usize
            && items <= expected.max_items as usize
            && expected
                .required_specs
                .iter()
                .all(|spec| specs.contains(spec))
            && expected
                .forbidden_specs
                .iter()
                .all(|spec| !specs.contains(spec)),
    )
}

fn score_review(
    output: &str,
    expected: &crate::case::ReviewExpected,
    validator: &dyn FindingValidator,
) -> Result<Scores, Error> {
    if parse_exit_signal(output).is_none() {
        return binary_score(false);
    }
    let Ok(findings) = parse_walk_output(output, DispatchScope::Tree, validator) else {
        return binary_score(false);
    };
    let mut used = BTreeSet::new();
    let matched = expected
        .findings
        .iter()
        .filter(|expected| {
            let found = findings.iter().enumerate().find(|(index, finding)| {
                !used.contains(index)
                    && expected
                        .contains
                        .iter()
                        .all(|term| contains_case_insensitive(finding.evidence(), term))
                    && expected.file.as_ref().is_none_or(|file| {
                        contains_case_insensitive(finding.evidence(), file)
                            || match &finding.target() {
                                loom_protocol::gate::FindingTarget::TestPath { path }
                                | loom_protocol::gate::FindingTarget::Template { path } => {
                                    path == file
                                }
                                loom_protocol::gate::FindingTarget::LockSite {
                                    file: actual,
                                    ..
                                } => actual == file,
                                loom_protocol::gate::FindingTarget::StyleRule {
                                    subject, ..
                                } => subject == file,
                                _ => false,
                            }
                    })
            });
            if let Some((index, _)) = found {
                used.insert(index);
                true
            } else {
                false
            }
        })
        .count();
    let extras = findings.len().saturating_sub(matched);
    let within_limit = expected
        .max_extra_findings
        .is_none_or(|limit| extras <= limit as usize);
    let soft = if expected.findings.is_empty() {
        1.0
    } else {
        count_as_f64(matched) / count_as_f64(expected.findings.len())
    };
    scores(
        if matched == expected.findings.len() && within_limit {
            1.0
        } else {
            0.0
        },
        if within_limit { soft } else { 0.0 },
    )
}

fn binary_score(pass: bool) -> Result<Scores, Error> {
    let value = if pass { 1.0 } else { 0.0 };
    scores(value, value)
}

fn count_as_f64(value: usize) -> f64 {
    let value = u64::try_from(value).unwrap_or(u64::MAX);
    let high = u32::try_from(value >> 32).unwrap_or(u32::MAX);
    let low = u32::try_from(value & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}

fn scores(hard: f64, soft: f64) -> Result<Scores, Error> {
    Ok(Scores::new(Score::new(hard)?, Score::new(soft)?))
}

fn contains_case_insensitive(text: &str, term: &str) -> bool {
    text.to_ascii_lowercase()
        .contains(&term.to_ascii_lowercase())
}

fn unavailable(detail: impl Into<String>) -> Error {
    Error::EvidenceUnavailable {
        detail: detail.into(),
    }
}

/// Behavioral checker execution failures: unavailable evidence is never a success score.
#[derive(Debug, Display, Error)]
pub enum Error {
    /// selected declared case `{case_id}` was not loaded
    MissingDeclaredCase { case_id: PlannedCaseId },
    /// selected case `{case_id}` has no captured current/candidate replay
    MissingReplay { case_id: PlannedCaseId },
    /// selected mined case `{case_id}` has no checkable expected-result oracle; not evaluated
    UncheckableSelection { case_id: PlannedCaseId },
    /// behavioral evidence unavailable; not evaluated: {detail}
    EvidenceUnavailable { detail: String },
    /// checker registry rejected executor metadata
    Registry(#[from] RegistryError),
    /// checker score was invalid
    Score(#[from] ScoreError),
}

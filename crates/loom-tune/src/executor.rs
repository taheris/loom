use std::collections::{BTreeMap, BTreeSet};

use displaydoc::Display;
use thiserror::Error;

use crate::case::{Case, Expected, LoadedCases};
use crate::checker::CheckerId;
use crate::gate::{CaseResult, Scores};
use crate::plan::{FrozenPlan, PlannedCaseId, SelectedCase};
use crate::score::{Score, ScoreError};
use crate::target::Target;

/// Current and candidate text for one tuned artifact.
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

/// Captured output from current and candidate agent replays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replay {
    pub case_id: PlannedCaseId,
    pub current_output: String,
    pub candidate_output: String,
}

impl Replay {
    pub fn new(
        case_id: PlannedCaseId,
        current_output: impl Into<String>,
        candidate_output: impl Into<String>,
    ) -> Self {
        Self {
            case_id,
            current_output: current_output.into(),
            candidate_output: candidate_output.into(),
        }
    }
}

/// Score outputs captured by behavioral checker replays for a frozen plan.
pub fn run(
    plan: &FrozenPlan,
    cases: &LoadedCases,
    replays: &[Replay],
) -> Result<Vec<CaseResult>, Error> {
    let case_by_id = cases
        .cases()
        .iter()
        .map(|case| (case.id.clone(), case))
        .collect::<BTreeMap<_, _>>();
    let replay_by_id = replays
        .iter()
        .map(|replay| (replay.case_id.clone(), replay))
        .collect::<BTreeMap<_, _>>();
    let mut results = Vec::with_capacity(plan.selected_cases.len());
    for selected in &plan.selected_cases {
        let replay = replay_by_id
            .get(&selected.case_id)
            .ok_or_else(|| Error::MissingReplay {
                case_id: selected.case_id.clone(),
            })?;
        let result = match &selected.case_id {
            PlannedCaseId::Declared(id) => {
                let case = case_by_id
                    .get(id)
                    .ok_or_else(|| Error::MissingDeclaredCase {
                        case_id: selected.case_id.clone(),
                    })?;
                run_declared(selected, case, replay)?
            }
            PlannedCaseId::Mined(_) => run_mined(selected, replay)?,
        };
        results.push(result);
    }
    Ok(results)
}

/// Terms a checker expects the tuned guidance to make salient.
pub fn expected_terms(expected: &Expected) -> Vec<String> {
    let mut terms = BTreeSet::new();
    match expected {
        Expected::ReviewFindingRecall(expected) => {
            for finding in &expected.findings {
                terms.extend(finding.contains.iter().cloned());
                if let Some(file) = &finding.file {
                    terms.insert(file.clone());
                }
            }
        }
        Expected::TodoDecomposition(expected) => {
            terms.extend(expected.required_specs.iter().map(ToString::to_string));
            terms.extend(expected.forbidden_specs.iter().map(ToString::to_string));
        }
        Expected::LoopVerifyAfterEdit(expected) => {
            terms.extend(expected.edited_paths.iter().cloned());
            terms.extend(expected.verify_commands.iter().cloned());
            terms.insert(expected.marker.clone());
        }
        Expected::LoopScopeDiscipline(expected) => {
            terms.extend(expected.allowed_edit_paths.iter().cloned());
            terms.extend(expected.forbidden_edit_paths.iter().cloned());
        }
        Expected::InboxResolutionPath(expected) => {
            terms.extend(expected.forbidden_commands.iter().cloned());
            terms.extend(expected.allowed_terminal_markers.iter().cloned());
        }
        Expected::TuneApplyHandoff(expected) => {
            terms.extend(expected.apply_proposals.iter().map(ToString::to_string));
            if expected.must_emit_apply {
                terms.insert("LOOM_APPLY".to_owned());
            }
        }
        Expected::AgentContextBeforeEdit(expected) => {
            terms.extend(expected.must_read_before_edit.iter().cloned());
            terms.extend(expected.edited_paths.iter().cloned());
        }
    }
    terms
        .into_iter()
        .filter(|term| !term.trim().is_empty())
        .collect()
}

fn run_declared(
    selected: &SelectedCase,
    case: &Case,
    replay: &Replay,
) -> Result<CaseResult, Error> {
    require_known_checker(&case.checker)?;
    let current = score_output(&replay.current_output, &case.expected)?;
    let candidate = score_output(&replay.candidate_output, &case.expected)?;
    Ok(CaseResult::new(selected, current, candidate))
}

fn run_mined(selected: &SelectedCase, replay: &Replay) -> Result<CaseResult, Error> {
    let current = score_presence(&replay.current_output)?;
    let candidate = score_presence(&replay.candidate_output)?;
    Ok(CaseResult::new(selected, current, candidate))
}

fn require_known_checker(checker: &CheckerId) -> Result<(), Error> {
    match checker.as_str() {
        "behavior.review.finding-recall"
        | "behavior.todo.decomposition"
        | "behavior.loop.verify-after-edit"
        | "behavior.loop.scope-discipline"
        | "behavior.inbox.resolution-path"
        | "behavior.tune.apply-handoff"
        | "behavior.agent.context-before-edit" => Ok(()),
        _ => Err(Error::UnsupportedChecker {
            checker: checker.clone(),
        }),
    }
}

fn score_output(output: &str, expected: &Expected) -> Result<Scores, ScoreError> {
    if let Expected::ReviewFindingRecall(expected) = expected {
        return score_review_output(output, expected);
    }
    let terms = expected_terms(expected);
    if terms.is_empty() {
        return score_presence(output);
    }
    let matched = terms
        .iter()
        .filter(|term| contains_case_insensitive(output, term))
        .count();
    let soft = matched as f64 / terms.len() as f64;
    let hard = if matched == terms.len() { 1.0 } else { 0.0 };
    scores(hard, soft)
}

fn score_review_output(
    output: &str,
    expected: &crate::case::ReviewExpected,
) -> Result<Scores, ScoreError> {
    let findings = output
        .lines()
        .filter(|line| line.trim_start().starts_with("LOOM_FINDING:"))
        .collect::<Vec<_>>();
    if expected.findings.is_empty() {
        return scores(1.0, 1.0);
    }
    let matched = expected
        .findings
        .iter()
        .filter(|expected| {
            findings.iter().any(|finding| {
                expected
                    .contains
                    .iter()
                    .all(|term| contains_case_insensitive(finding, term))
                    && expected
                        .file
                        .as_ref()
                        .is_none_or(|file| contains_case_insensitive(finding, file))
            })
        })
        .count();
    let within_extra_limit = expected.max_extra_findings.is_none_or(|limit| {
        findings.len().saturating_sub(expected.findings.len()) <= limit as usize
    });
    let soft = matched as f64 / expected.findings.len() as f64;
    let hard = if matched == expected.findings.len() && within_extra_limit {
        1.0
    } else {
        0.0
    };
    scores(hard, soft)
}

fn score_presence(output: &str) -> Result<Scores, ScoreError> {
    let present = !output.trim().is_empty();
    let value = if present { 1.0 } else { 0.0 };
    scores(value, value)
}

fn scores(hard: f64, soft: f64) -> Result<Scores, ScoreError> {
    Ok(Scores::new(Score::new(hard)?, Score::new(soft)?))
}

fn contains_case_insensitive(text: &str, term: &str) -> bool {
    text.to_ascii_lowercase()
        .contains(&term.to_ascii_lowercase())
}

/// Behavioral checker execution failures.
#[derive(Debug, Display, Error)]
pub enum Error {
    /// selected declared case `{case_id}` was not loaded
    MissingDeclaredCase { case_id: PlannedCaseId },
    /// selected case `{case_id}` has no captured current/candidate replay
    MissingReplay { case_id: PlannedCaseId },
    /// checker `{checker}` has no executor implementation
    UnsupportedChecker { checker: CheckerId },
    /// checker score was invalid
    Score(#[from] ScoreError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::{ReviewExpected, ReviewFinding};
    use crate::checker::CheckerId;
    use crate::plan::{Pool, SelectedCase};

    #[test]
    fn expected_terms_extracts_review_predicates() {
        let terms = expected_terms(&Expected::ReviewFindingRecall(ReviewExpected {
            findings: vec![ReviewFinding {
                contains: vec!["missing test".to_owned()],
                file: Some("src/lib.rs".to_owned()),
            }],
            max_extra_findings: None,
        }));
        assert_eq!(terms, vec!["missing test", "src/lib.rs"]);
    }

    #[test]
    fn declared_checker_scores_replayed_agent_findings() {
        let selected = SelectedCase {
            case_id: PlannedCaseId::Declared(crate::case::Id::new("case-a").expect("case id")),
            checker: CheckerId::new("behavior.review.finding-recall").expect("checker"),
            pool: Pool::DeclaredRegression,
        };
        let case = Case {
            id: crate::case::Id::new("case-a").expect("case id"),
            checker: selected.checker.clone(),
            targets: vec!["skill:loom-context-before-edit".parse().expect("target")],
            role: crate::checker::CaseRole::Regression,
            input: crate::case::Input::ReviewFindingRecall {
                patch: crate::case::RepoPath {
                    relative: "cases/review.diff".into(),
                    kind: crate::case::PathKind::File,
                },
            },
            expected: Expected::ReviewFindingRecall(ReviewExpected {
                findings: vec![ReviewFinding {
                    contains: vec!["missing test".to_owned()],
                    file: None,
                }],
                max_extra_findings: None,
            }),
            source: crate::case::Source {
                path: "docs/tuning.md".into(),
                line: 1,
            },
        };
        let replay = Replay::new(
            selected.case_id.clone(),
            "LOOM_COMPLETE",
            r#"LOOM_FINDING: {"evidence":"missing test"}"#,
        );
        let result = run_declared(&selected, &case, &replay).expect("checker runs");
        assert_eq!(result.current.soft.get(), 0.0);
        assert_eq!(result.candidate.soft.get(), 1.0);
    }
}

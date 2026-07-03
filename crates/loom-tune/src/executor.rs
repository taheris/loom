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

/// Execute selected behavioral checkers for a frozen plan.
pub fn run(
    plan: &FrozenPlan,
    cases: &LoadedCases,
    artifacts: &[Artifact],
) -> Result<Vec<CaseResult>, Error> {
    let case_by_id = cases
        .cases()
        .iter()
        .map(|case| (case.id.clone(), case))
        .collect::<BTreeMap<_, _>>();
    let mut results = Vec::with_capacity(plan.selected_cases.len());
    for selected in &plan.selected_cases {
        let result = match &selected.case_id {
            PlannedCaseId::Declared(id) => {
                let case = case_by_id
                    .get(id)
                    .ok_or_else(|| Error::MissingDeclaredCase {
                        case_id: selected.case_id.clone(),
                    })?;
                run_declared(selected, case, artifacts)?
            }
            PlannedCaseId::Mined(_) => run_mined(selected, artifacts)?,
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
    artifacts: &[Artifact],
) -> Result<CaseResult, Error> {
    require_known_checker(&case.checker)?;
    let relevant = artifacts_for_targets(artifacts, &case.targets);
    if relevant.is_empty() {
        return Err(Error::MissingArtifact {
            case_id: selected.case_id.clone(),
            targets: case.targets.clone(),
        });
    }
    let current = score_text(
        &join_artifact_text(&relevant, TextSide::Current),
        &case.expected,
    )?;
    let candidate = score_text(
        &join_artifact_text(&relevant, TextSide::Candidate),
        &case.expected,
    )?;
    Ok(CaseResult::new(selected, current, candidate))
}

fn run_mined(selected: &SelectedCase, artifacts: &[Artifact]) -> Result<CaseResult, Error> {
    if artifacts.is_empty() {
        return Err(Error::MissingArtifact {
            case_id: selected.case_id.clone(),
            targets: Vec::new(),
        });
    }
    let current = score_presence(artifacts.iter().map(|artifact| artifact.current.as_str()))?;
    let candidate = score_presence(artifacts.iter().map(|artifact| artifact.candidate.as_str()))?;
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

fn artifacts_for_targets<'a>(artifacts: &'a [Artifact], targets: &[Target]) -> Vec<&'a Artifact> {
    artifacts
        .iter()
        .filter(|artifact| targets.iter().any(|target| target == &artifact.target))
        .collect()
}

#[derive(Debug, Clone, Copy)]
enum TextSide {
    Current,
    Candidate,
}

fn join_artifact_text(artifacts: &[&Artifact], side: TextSide) -> String {
    let mut text = String::new();
    for artifact in artifacts {
        match side {
            TextSide::Current => text.push_str(&artifact.current),
            TextSide::Candidate => text.push_str(&artifact.candidate),
        }
        text.push('\n');
    }
    text
}

fn score_text(text: &str, expected: &Expected) -> Result<Scores, ScoreError> {
    let terms = expected_terms(expected);
    if terms.is_empty() {
        return scores(1.0, 1.0);
    }
    let matched = terms
        .iter()
        .filter(|term| contains_case_insensitive(text, term))
        .count();
    let soft = matched as f64 / terms.len() as f64;
    let hard = if matched == terms.len() { 1.0 } else { 0.0 };
    scores(hard, soft)
}

fn score_presence<'a>(texts: impl Iterator<Item = &'a str>) -> Result<Scores, ScoreError> {
    let mut count = 0_usize;
    let mut present = 0_usize;
    for text in texts {
        count += 1;
        if !text.trim().is_empty() {
            present += 1;
        }
    }
    let soft = if count == 0 {
        0.0
    } else {
        present as f64 / count as f64
    };
    let hard = if count == present { 1.0 } else { 0.0 };
    scores(hard, soft)
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
    /// selected case `{case_id}` has no tuned artifact for targets {targets:?}
    MissingArtifact {
        case_id: PlannedCaseId,
        targets: Vec<Target>,
    },
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
    fn declared_checker_scores_candidate_artifact_terms() {
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
        let artifact = Artifact::new(
            "skill:loom-context-before-edit".parse().expect("target"),
            "read files first",
            "read files first and report missing test findings",
        );
        let result = run_declared(&selected, &case, &[artifact]).expect("checker runs");
        assert_eq!(result.current.soft.get(), 0.0);
        assert_eq!(result.candidate.soft.get(), 1.0);
    }
}

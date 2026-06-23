use std::cmp::Ordering;
use std::collections::BTreeMap;

use displaydoc::Display;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::checker::{CheckerId, Registry, RegistryError};
use crate::plan::{FrozenPlan, PlannedCaseId, Pool, SelectedCase};
use crate::score::{Score, ScoreError};

const DEFAULT_SOFT_REGRESSION_EPSILON: f64 = 0.01;

/// Hard/soft score pair emitted for one behavioral checker run.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Scores {
    pub hard: Score,
    pub soft: Score,
}

impl Scores {
    pub fn new(hard: Score, soft: Score) -> Self {
        Self { hard, soft }
    }
}

/// Raw current-vs-candidate result for one selected case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseResult {
    pub case_id: PlannedCaseId,
    pub checker: CheckerId,
    pub pool: Pool,
    pub current: Scores,
    pub candidate: Scores,
}

impl CaseResult {
    pub fn new(selected: &SelectedCase, current: Scores, candidate: Scores) -> Self {
        Self {
            case_id: selected.case_id.clone(),
            checker: selected.checker.clone(),
            pool: selected.pool,
            current,
            candidate,
        }
    }
}

/// Candidate outcome for one selected behavioral case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Improved,
    Regressed,
    PersistentFail,
    StableSuccess,
}

/// Candidate gate state after all selected behavior has been evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Passed,
    Blocked,
}

/// Evaluated current-vs-candidate result for one selected case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluatedCase {
    pub case_id: PlannedCaseId,
    pub checker: CheckerId,
    pub pool: Pool,
    pub current: Scores,
    pub candidate: Scores,
    pub outcome: Outcome,
}

/// Equal-weight aggregate comparison for mined selection evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Aggregate {
    pub current: Scores,
    pub candidate: Scores,
    pub outcome: Outcome,
}

/// Behavioral gate report for a candidate artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub state: State,
    pub cases: Vec<EvaluatedCase>,
    pub mined_selection: Option<Aggregate>,
}

/// Evaluate selected behavioral cases against the frozen checker plan.
pub fn evaluate(
    plan: &FrozenPlan,
    results: impl IntoIterator<Item = CaseResult>,
    registry: &Registry,
) -> Result<Report, Error> {
    let mut by_id = BTreeMap::new();
    for result in results {
        let case_id = result.case_id.clone();
        if by_id.insert(case_id.clone(), result).is_some() {
            return Err(Error::DuplicateResult { case_id });
        }
    }

    let mut cases = Vec::with_capacity(plan.selected_cases.len());
    for selected in &plan.selected_cases {
        let Some(result) = by_id.remove(&selected.case_id) else {
            return Err(Error::MissingResult {
                case_id: selected.case_id.clone(),
            });
        };
        if result.checker != selected.checker || result.pool != selected.pool {
            return Err(Error::PlanMismatch {
                case_id: selected.case_id.clone(),
                expected_checker: selected.checker.clone(),
                actual_checker: result.checker,
                expected_pool: selected.pool,
                actual_pool: result.pool,
            });
        }
        let epsilon = soft_regression_epsilon(registry, &selected.checker)?;
        cases.push(EvaluatedCase {
            case_id: selected.case_id.clone(),
            checker: selected.checker.clone(),
            pool: selected.pool,
            current: result.current,
            candidate: result.candidate,
            outcome: classify(result.current, result.candidate, epsilon),
        });
    }
    if let Some(case_id) = by_id.into_keys().next() {
        return Err(Error::UnexpectedResult { case_id });
    }

    let mined_selection = aggregate_mined_selection(&cases)?;
    let declared_regressed = cases
        .iter()
        .any(|case| case.pool == Pool::DeclaredRegression && case.outcome == Outcome::Regressed);
    let mined_regressed = mined_selection
        .as_ref()
        .is_some_and(|aggregate| aggregate.outcome == Outcome::Regressed);
    let state = if declared_regressed || mined_regressed {
        State::Blocked
    } else {
        State::Passed
    };
    Ok(Report {
        state,
        cases,
        mined_selection,
    })
}

fn aggregate_mined_selection(cases: &[EvaluatedCase]) -> Result<Option<Aggregate>, Error> {
    let selected = cases
        .iter()
        .filter(|case| case.pool == Pool::MinedSelection)
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Ok(None);
    }
    let current = average_scores(selected.iter().map(|case| case.current))?;
    let candidate = average_scores(selected.iter().map(|case| case.candidate))?;
    Ok(Some(Aggregate {
        current,
        candidate,
        outcome: classify(current, candidate, DEFAULT_SOFT_REGRESSION_EPSILON),
    }))
}

fn average_scores(scores: impl Iterator<Item = Scores>) -> Result<Scores, ScoreError> {
    let mut hard = 0.0;
    let mut soft = 0.0;
    let mut count = 0_usize;
    for score in scores {
        hard += score.hard.get();
        soft += score.soft.get();
        count += 1;
    }
    let divisor = count as f64;
    Ok(Scores {
        hard: Score::new(hard / divisor)?,
        soft: Score::new(soft / divisor)?,
    })
}

fn classify(current: Scores, candidate: Scores, epsilon: f64) -> Outcome {
    match candidate.hard.get().total_cmp(&current.hard.get()) {
        Ordering::Less => Outcome::Regressed,
        Ordering::Greater => Outcome::Improved,
        Ordering::Equal if candidate.soft.get() < current.soft.get() - epsilon => {
            Outcome::Regressed
        }
        Ordering::Equal if candidate.soft.get() > current.soft.get() + epsilon => Outcome::Improved,
        Ordering::Equal if candidate.hard.get().total_cmp(&1.0) == Ordering::Equal => {
            Outcome::StableSuccess
        }
        Ordering::Equal => Outcome::PersistentFail,
    }
}

fn soft_regression_epsilon(registry: &Registry, checker: &CheckerId) -> Result<f64, Error> {
    let metadata = registry.require_active(checker)?;
    let epsilon = metadata
        .soft_regression_epsilon
        .parse::<f64>()
        .map_err(|_| Error::InvalidEpsilon {
            checker: checker.clone(),
            value: metadata.soft_regression_epsilon.clone(),
        })?;
    if epsilon.is_finite() && epsilon >= 0.0 {
        Ok(epsilon)
    } else {
        Err(Error::InvalidEpsilon {
            checker: checker.clone(),
            value: metadata.soft_regression_epsilon.clone(),
        })
    }
}

/// Behavioral gate failures.
#[derive(Debug, Display, Error)]
pub enum Error {
    /// behavioral result `{case_id}` was reported more than once
    DuplicateResult { case_id: PlannedCaseId },
    /// behavioral result `{case_id}` was not selected by the frozen checker plan
    UnexpectedResult { case_id: PlannedCaseId },
    /// selected behavioral case `{case_id}` has no candidate result
    MissingResult { case_id: PlannedCaseId },
    /// behavioral result `{case_id}` does not match the frozen checker plan: expected `{expected_checker}` / `{expected_pool:?}`, got `{actual_checker}` / `{actual_pool:?}`
    PlanMismatch {
        case_id: PlannedCaseId,
        expected_checker: CheckerId,
        actual_checker: CheckerId,
        expected_pool: Pool,
        actual_pool: Pool,
    },
    /// checker registry rejected behavioral gate metadata
    Registry(#[from] RegistryError),
    /// checker `{checker}` has invalid soft regression epsilon `{value}`
    InvalidEpsilon { checker: CheckerId, value: String },
    /// aggregate behavioral score is invalid
    Score(#[from] ScoreError),
}

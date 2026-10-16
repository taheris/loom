//! Immutable dispatch instructions shared with read-only prompt consumers.

use super::{GoalReference, GoalReferences, ParseError, Resolved};
use crate::gate::Finding;
use loom_events::identifier::BeadId;

/// Resolved instructions, never verifier results or publication authority.
///
/// ```compile_fail
/// let _: loom_protocol::acceptance::dispatch::Acceptance = serde_json::from_str("{}").unwrap();
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Acceptance {
    Implementation(Resolved),
    Remediation(Remediation),
}

/// Complete immutable processed-batch goals after native attribution checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remediation {
    goals: Vec<Goal>,
}

impl Remediation {
    /// Retain a nonempty, distinct set of already checked goals.
    ///
    /// # Errors
    /// Rejects empty or duplicate attribution.
    pub fn new(goals: Vec<Goal>) -> Result<Self, ParseError> {
        GoalReferences::new(goals.iter().map(|goal| goal.reference.clone()).collect())?;
        Ok(Self { goals })
    }

    pub fn goals(&self) -> &[Goal] {
        &self.goals
    }
}

/// Native membership is checked by the workflow before constructing this value.
/// The finding itself must already have passed current-workspace resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Goal {
    reference: GoalReference,
    task: BeadId,
    work_root: BeadId,
    finding: Finding,
}

impl Goal {
    /// Bind native attribution to a resolved finding without accepting editable wire data.
    ///
    /// # Errors
    /// Rejects a reference naming another finding.
    pub fn new(
        reference: GoalReference,
        task: BeadId,
        work_root: BeadId,
        finding: Finding,
    ) -> Result<Self, ParseError> {
        if reference.finding_hash.as_str() != finding.hash() {
            return Err(ParseError::GoalIdentity);
        }
        Ok(Self {
            reference,
            task,
            work_root,
            finding,
        })
    }

    pub const fn reference(&self) -> &GoalReference {
        &self.reference
    }
    pub const fn task(&self) -> &BeadId {
        &self.task
    }
    pub const fn work_root(&self) -> &BeadId {
        &self.work_root
    }
    pub const fn finding(&self) -> &Finding {
        &self.finding
    }
}

/// Read-only rendering of the exact dispatched instructions with workspace-relative sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    acceptance: Acceptance,
    workspace: std::path::PathBuf,
}

impl Acceptance {
    pub fn context(&self, workspace: &std::path::Path) -> Context {
        Context {
            acceptance: self.clone(),
            workspace: workspace.to_path_buf(),
        }
    }
}

impl std::fmt::Display for Context {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            formatter,
            "Resolved dispatch instructions, not passing evidence or publication permission."
        )?;
        match &self.acceptance {
            Acceptance::Implementation(resolved) => {
                writeln!(formatter, "Purpose: implementation")?;
                for obligation in resolved.obligations() {
                    writeln!(
                        formatter,
                        "\n### {}\n\n{}\n\nBinding: {}\nSource: {}:{}",
                        obligation.reference(),
                        obligation.criterion_text(),
                        obligation.annotation(),
                        obligation
                            .source()
                            .strip_prefix(&self.workspace)
                            .unwrap_or_else(|_| obligation.source())
                            .display(),
                        obligation.line()
                    )?;
                }
            }
            Acceptance::Remediation(resolved) => {
                writeln!(
                    formatter,
                    "Purpose: remediation\nAcceptance covers this processed batch, not every retained finding."
                )?;
                for goal in resolved.goals() {
                    writeln!(
                        formatter,
                        "\n### {} / {}\n\nTask: {}\nWork root: {}\nConcern: {}\nRoute: {}\nBonds: {:?}\nTarget: {:?}\nEvidence:\n{}",
                        goal.reference().batch,
                        goal.reference().finding_hash.as_str(),
                        goal.task(),
                        goal.work_root(),
                        goal.finding().token().as_wire(),
                        goal.finding().route().as_wire(),
                        goal.finding().bonds(),
                        goal.finding().target(),
                        goal.finding().evidence()
                    )?;
                }
            }
        }
        Ok(())
    }
}

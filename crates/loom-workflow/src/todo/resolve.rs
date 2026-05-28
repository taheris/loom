//! Single-query spec → molecule resolution for `loom todo`.
//!
//! Replaces the four-tier walk. One `bd find --type=epic --label=spec:<X>
//! --status=open` call answers the only question worth asking: does the
//! spec already have an active molecule? Three outcomes — zero, one, or
//! many open epics — map to [`ResolverOutcome`] so callers branch on a
//! sealed enum instead of a multi-tier decision tree.

use loom_driver::bd::{BdClient, CommandRunner, ListOpts};
use loom_driver::identifier::{MoleculeId, SpecLabel};

use super::error::TodoError;

/// Outcome of [`resolve_molecule`].
///
/// `Existing` and `None` are the productive paths; `InvariantViolation`
/// surfaces the structural-invariant break (>1 open epic for the same
/// spec) so callers can refuse to proceed and name the conflicting ids in
/// the error message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolverOutcome {
    Existing(MoleculeId),
    None,
    InvariantViolation(Vec<MoleculeId>),
}

/// Resolve the spec's active molecule via one `bd find` call.
///
/// Implementation is the single bd query named in `specs/harness.md`
/// *Workflow commands*: zero results → `None`, one → `Existing(id)`,
/// more than one → `InvariantViolation(ids)`.
pub async fn resolve_molecule<R: CommandRunner>(
    bd: &BdClient<R>,
    label: &SpecLabel,
) -> Result<ResolverOutcome, TodoError> {
    let beads = bd
        .list(ListOpts {
            issue_type: Some("epic".to_string()),
            label: Some(format!("spec:{}", label.as_str())),
            status: Some("open".to_string()),
            ..Default::default()
        })
        .await?;
    let outcome = match beads.len() {
        0 => ResolverOutcome::None,
        1 => ResolverOutcome::Existing(MoleculeId::new(beads[0].id.as_str())),
        _ => ResolverOutcome::InvariantViolation(
            beads
                .iter()
                .map(|b| MoleculeId::new(b.id.as_str()))
                .collect(),
        ),
    };
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::bd::{BdError, RunOutput};
    use std::ffi::OsString;
    use std::sync::Mutex;
    use std::time::Duration;

    struct ScriptedRunner {
        responses: Mutex<Vec<RunOutput>>,
    }

    impl ScriptedRunner {
        fn new(responses: Vec<RunOutput>) -> Self {
            Self {
                responses: Mutex::new(responses),
            }
        }
    }

    impl CommandRunner for ScriptedRunner {
        async fn run(&self, _args: Vec<OsString>, _t: Duration) -> Result<RunOutput, BdError> {
            Ok(self.responses.lock().unwrap().remove(0))
        }
    }

    fn ok_stdout(body: &str) -> RunOutput {
        RunOutput {
            status: 0,
            stdout: body.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    fn epic_body(ids: &[&str], label: &str) -> String {
        let entries: Vec<String> = ids
            .iter()
            .map(|id| {
                format!(
                    r#"{{
                        "id": "{id}",
                        "title": "{label}: epic",
                        "status": "open",
                        "priority": 2,
                        "issue_type": "epic",
                        "labels": ["spec:{label}"],
                        "metadata": {{}}
                    }}"#
                )
            })
            .collect();
        format!("[{}]", entries.join(","))
    }

    #[tokio::test]
    async fn zero_results_resolves_to_none() {
        let runner = ScriptedRunner::new(vec![ok_stdout("[]")]);
        let bd = BdClient::with_runner(runner);
        let label = SpecLabel::new("alpha");
        let outcome = resolve_molecule(&bd, &label).await.expect("resolve ok");
        assert_eq!(outcome, ResolverOutcome::None);
    }

    #[tokio::test]
    async fn one_result_resolves_to_existing() {
        let body = epic_body(&["lm-mol"], "alpha");
        let runner = ScriptedRunner::new(vec![ok_stdout(&body)]);
        let bd = BdClient::with_runner(runner);
        let label = SpecLabel::new("alpha");
        let outcome = resolve_molecule(&bd, &label).await.expect("resolve ok");
        assert_eq!(
            outcome,
            ResolverOutcome::Existing(MoleculeId::new("lm-mol"))
        );
    }

    /// `specs/harness.md` *Workflow commands* SC: when bd returns more than
    /// one open epic for a single spec, the resolver refuses to proceed
    /// and surfaces every conflicting id.
    #[tokio::test]
    async fn todo_single_query_resolution_with_invariant_violation_refusal() {
        let body = epic_body(&["lm-aaa", "lm-bbb"], "alpha");
        let runner = ScriptedRunner::new(vec![ok_stdout(&body)]);
        let bd = BdClient::with_runner(runner);
        let label = SpecLabel::new("alpha");
        let outcome = resolve_molecule(&bd, &label).await.expect("resolve ok");
        match outcome {
            ResolverOutcome::InvariantViolation(ids) => {
                assert_eq!(
                    ids,
                    vec![MoleculeId::new("lm-aaa"), MoleculeId::new("lm-bbb"),],
                );
            }
            other => panic!("expected InvariantViolation, got {other:?}"),
        }
    }
}

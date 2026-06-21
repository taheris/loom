//! Spec → molecule resolution via `bd find`.
//!
//! Under the at-most-one-open-work-epic-per-spec invariant, the spec's
//! active/current molecule is the non-`loom:spec` open epic returned by
//! `bd find --type=epic --label=spec:<X> --status=open`. Spec epics are
//! durable metadata carriers and are ignored here. Zero work-epic results
//! means no molecule (callers either mint one or treat the spec as
//! pristine); more than one is a structural invariant violation that
//! refuses to proceed.

use displaydoc::Display;
use thiserror::Error;

use loom_driver::bd::{BdClient, BdError, CommandRunner, CreateOpts, ListOpts};
use loom_driver::identifier::{MoleculeId, SpecLabel};

/// Failures from [`resolve_open_epic`].
#[derive(Debug, Display, Error)]
pub enum ResolveError {
    /// bd query failed while resolving the active molecule
    Bd(#[from] BdError),
    /// multiple open epics found for spec `{label}`: {ids}; close all but one before re-running
    InvariantViolation { label: String, ids: String },
}

/// Resolve the spec's active/current molecule via `bd find --type=epic
/// --label=spec:<X> --status=open`, ignoring `loom:spec` metadata epics.
/// Returns the open work epic's id, `None` when no open work epic exists,
/// or [`ResolveError::InvariantViolation`] when more than one open work
/// epic exists for the spec.
pub async fn resolve_open_epic<R: CommandRunner>(
    bd: &BdClient<R>,
    label: &SpecLabel,
) -> Result<Option<MoleculeId>, ResolveError> {
    let beads = bd
        .list(ListOpts {
            issue_type: Some("epic".to_string()),
            label: Some(format!("spec:{}", label.as_str())),
            status: Some("open".to_string()),
            ..Default::default()
        })
        .await?;
    let work_epics = beads
        .iter()
        .filter(|bead| !bead.labels.iter().any(|label| label.is_spec_epic()))
        .collect::<Vec<_>>();
    match work_epics.len() {
        0 => Ok(None),
        1 => Ok(Some(MoleculeId::new(work_epics[0].id.as_str()))),
        _ => {
            let ids = work_epics
                .iter()
                .map(|b| b.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Err(ResolveError::InvariantViolation {
                label: label.to_string(),
                ids,
            })
        }
    }
}

/// One spec's resolved bonding target for the standing safety net.
/// `was_minted` is true when [`resolve_or_mint_open_epic`] minted a
/// fresh epic because no open epic existed; the binary uses that to
/// surface "auto-create" lines on stdout per `specs/gate.md`
/// § *Standing-safety-net bonding*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEpic {
    pub label: SpecLabel,
    pub molecule_id: MoleculeId,
    pub was_minted: bool,
}

/// Per-spec resolve-or-mint loop for `loom gate mint --tree`. Calls
/// [`resolve_or_mint_open_epic`] for every label in `labels` and returns
/// the resolved epics in the same order. Stops on the first
/// [`ResolveError::InvariantViolation`] so the operator sees the
/// conflicting epic IDs before any further work happens.
pub async fn resolve_or_mint_open_epics<R: CommandRunner>(
    bd: &BdClient<R>,
    labels: &[SpecLabel],
    head_commit: &str,
) -> Result<Vec<ResolvedEpic>, ResolveError> {
    let mut resolved = Vec::with_capacity(labels.len());
    for label in labels {
        resolved.push(resolve_or_mint_open_epic(bd, label, head_commit).await?);
    }
    Ok(resolved)
}

/// Single-tier bonding-target resolver for `loom gate mint --tree`.
///
/// Runs the same `bd find --type=epic --label=spec:<X> --status=open`
/// query as [`resolve_open_epic`], ignoring `loom:spec` metadata epics;
/// on zero work-epic results, mints a fresh epic
/// via `bd create --type=epic --title="<X>" --labels="spec:<X>"
/// --metadata "loom.base_commit=<head_commit>"` and returns it with
/// `was_minted = true`. More-than-one open epics propagate as
/// [`ResolveError::InvariantViolation`], matching the gate.md
/// "structural invariant violation, refuse to proceed" branch.
pub async fn resolve_or_mint_open_epic<R: CommandRunner>(
    bd: &BdClient<R>,
    label: &SpecLabel,
    head_commit: &str,
) -> Result<ResolvedEpic, ResolveError> {
    if let Some(molecule_id) = resolve_open_epic(bd, label).await? {
        return Ok(ResolvedEpic {
            label: label.clone(),
            molecule_id,
            was_minted: false,
        });
    }
    let metadata = serde_json::json!({ "loom.base_commit": head_commit }).to_string();
    let bead_id = bd
        .create(CreateOpts {
            title: label.as_str().to_owned(),
            description: String::new(),
            issue_type: Some("epic".to_string()),
            labels: vec![format!("spec:{}", label.as_str())],
            metadata: Some(metadata),
            ..CreateOpts::default()
        })
        .await?;
    Ok(ResolvedEpic {
        label: label.clone(),
        molecule_id: MoleculeId::new(bead_id.as_str()),
        was_minted: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::bd::{BdClient, CommandRunner, RunOutput};
    use std::ffi::OsString;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct ScriptedRunner {
        responses: Mutex<Vec<RunOutput>>,
        invocations: Arc<Mutex<Vec<Vec<OsString>>>>,
    }

    impl ScriptedRunner {
        fn new(responses: Vec<RunOutput>) -> Self {
            Self {
                responses: Mutex::new(responses),
                invocations: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn invocations_handle(&self) -> Arc<Mutex<Vec<Vec<OsString>>>> {
            Arc::clone(&self.invocations)
        }
    }

    impl CommandRunner for ScriptedRunner {
        async fn run(&self, args: Vec<OsString>, _t: Duration) -> Result<RunOutput, BdError> {
            self.invocations.lock().unwrap().push(args);
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

    #[tokio::test]
    async fn resolve_or_mint_mints_when_zero_open_epics() {
        let runner = ScriptedRunner::new(vec![ok_stdout("[]"), ok_stdout("lm-newepic\n")]);
        let bd = BdClient::with_runner(runner);
        let label = SpecLabel::new("acme");
        let resolved = resolve_or_mint_open_epic(&bd, &label, "deadbeef")
            .await
            .expect("resolve_or_mint ok");
        assert_eq!(resolved.label, label);
        assert_eq!(resolved.molecule_id, MoleculeId::new("lm-newepic"));
        assert!(resolved.was_minted);
    }

    #[tokio::test]
    async fn resolve_or_mint_does_not_clobber_existing_epic() {
        let existing = r#"[{
            "id": "lm-existing",
            "title": "acme",
            "status": "open",
            "priority": 2,
            "issue_type": "epic",
            "labels": ["spec:acme"],
            "metadata": {}
        }]"#;
        let runner = ScriptedRunner::new(vec![ok_stdout(existing)]);
        let bd = BdClient::with_runner(runner);
        let label = SpecLabel::new("acme");
        let resolved = resolve_or_mint_open_epic(&bd, &label, "deadbeef")
            .await
            .expect("resolve_or_mint ok");
        assert_eq!(resolved.molecule_id, MoleculeId::new("lm-existing"));
        assert!(!resolved.was_minted);
    }

    #[tokio::test]
    async fn resolve_or_mint_ignores_metadata_spec_epic() {
        let only_spec_epic = r#"[{
            "id": "lm-spec",
            "title": "loom spec: acme",
            "status": "open",
            "priority": 2,
            "issue_type": "epic",
            "labels": ["loom:spec", "spec:acme"],
            "metadata": {"loom.todo_cursor":"deadbeef"}
        }]"#;
        let runner =
            ScriptedRunner::new(vec![ok_stdout(only_spec_epic), ok_stdout("lm-newwork\n")]);
        let bd = BdClient::with_runner(runner);
        let label = SpecLabel::new("acme");
        let resolved = resolve_or_mint_open_epic(&bd, &label, "deadbeef")
            .await
            .expect("resolve_or_mint ok");
        assert_eq!(resolved.molecule_id, MoleculeId::new("lm-newwork"));
        assert!(
            resolved.was_minted,
            "spec epics are metadata carriers and must not satisfy work-epic resolution",
        );
    }

    #[tokio::test]
    async fn resolve_or_mint_refuses_on_more_than_one_open_epic() {
        let conflict = r#"[
            {"id":"lm-a","title":"acme","status":"open","priority":2,"issue_type":"epic","labels":["spec:acme"],"metadata":{}},
            {"id":"lm-b","title":"acme","status":"open","priority":2,"issue_type":"epic","labels":["spec:acme"],"metadata":{}}
        ]"#;
        let runner = ScriptedRunner::new(vec![ok_stdout(conflict)]);
        let bd = BdClient::with_runner(runner);
        let label = SpecLabel::new("acme");
        let err = resolve_or_mint_open_epic(&bd, &label, "deadbeef")
            .await
            .expect_err("must refuse");
        match err {
            ResolveError::InvariantViolation { label: l, ids } => {
                assert_eq!(l, "acme");
                assert!(ids.contains("lm-a"));
                assert!(ids.contains("lm-b"));
            }
            other => panic!("expected InvariantViolation, got {other:?}"),
        }
    }

    /// `loom gate mint --tree` (all-specs sweep) mints a fresh epic per
    /// spec when the single-tier resolution returns zero open epics. Pins
    /// the safety property from `specs/gate.md` § *Standing-safety-net
    /// bonding*: concerns about a spec with no active work get a fresh
    /// container, not silently dropped. The orchestrator is the
    /// `loom-workflow::resolve` helper that walks `labels` and resolves
    /// each spec independently — every spec the mint visits must mint
    /// its own molecule + epic when the bd query returns zero.
    #[tokio::test]
    async fn tree_scope_auto_creates_epics_for_missing_current_molecule_specs() {
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout("lm-newalpha\n"),
            ok_stdout("[]"),
            ok_stdout("lm-newbeta\n"),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let labels = [SpecLabel::new("alpha"), SpecLabel::new("beta")];
        let resolved = resolve_or_mint_open_epics(&bd, &labels, "head-sha")
            .await
            .expect("resolve_or_mint_open_epics ok");

        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].label, SpecLabel::new("alpha"));
        assert_eq!(resolved[0].molecule_id, MoleculeId::new("lm-newalpha"));
        assert!(
            resolved[0].was_minted,
            "alpha must report was_minted=true so the binary can surface the auto-create line",
        );
        assert_eq!(resolved[1].label, SpecLabel::new("beta"));
        assert_eq!(resolved[1].molecule_id, MoleculeId::new("lm-newbeta"));
        assert!(resolved[1].was_minted);

        let calls = invocations.lock().unwrap().clone();
        let creates: Vec<_> = calls
            .iter()
            .filter(|args| args.iter().any(|a| a == "create"))
            .collect();
        assert_eq!(
            creates.len(),
            2,
            "one bd create per spec with no open epic. calls={calls:?}",
        );
    }

    /// `loom gate mint --tree` MUST NOT clobber an existing open epic for
    /// a spec: when the single-tier resolution returns one result, the
    /// orchestrator bonds fix-ups to that molecule without minting a new
    /// one. Pins `specs/gate.md` § *Standing-safety-net bonding*'s
    /// "one result → bonds fix-ups to its molecule" branch.
    #[tokio::test]
    async fn tree_scope_orchestrator_does_not_clobber_existing_current_molecule() {
        let existing = r#"[{
            "id": "lm-existing",
            "title": "alpha",
            "status": "open",
            "priority": 2,
            "issue_type": "epic",
            "labels": ["spec:alpha"],
            "metadata": {}
        }]"#;
        let runner = ScriptedRunner::new(vec![ok_stdout(existing)]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let labels = [SpecLabel::new("alpha")];
        let resolved = resolve_or_mint_open_epics(&bd, &labels, "head-sha")
            .await
            .expect("resolve ok");

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].molecule_id, MoleculeId::new("lm-existing"));
        assert!(
            !resolved[0].was_minted,
            "existing epic must NOT be reported as minted — mint bonds fix-ups to it",
        );

        let calls = invocations.lock().unwrap().clone();
        let create_count = calls
            .iter()
            .filter(|args| args.iter().any(|a| a == "create"))
            .count();
        assert_eq!(
            create_count, 0,
            "no bd create must fire when an open epic already exists. calls={calls:?}",
        );
    }

    #[tokio::test]
    async fn resolve_or_mint_passes_metadata_with_head_commit() {
        let runner = ScriptedRunner::new(vec![ok_stdout("[]"), ok_stdout("lm-newepic\n")]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let label = SpecLabel::new("acme");
        let _ = resolve_or_mint_open_epic(&bd, &label, "deadbeef")
            .await
            .expect("ok");
        let invocations = invocations.lock().unwrap().clone();
        assert_eq!(invocations.len(), 2, "list then create");
        let create = &invocations[1];
        let args: Vec<String> = create
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(args.iter().any(|a| a == "create"));
        assert!(args.iter().any(|a| a == "--type"));
        assert!(args.iter().any(|a| a == "epic"));
        assert!(args.iter().any(|a| a == "--title"));
        assert!(args.iter().any(|a| a == "acme"));
        assert!(args.iter().any(|a| a == "--labels"));
        assert!(args.iter().any(|a| a == "spec:acme"));
        assert!(args.iter().any(|a| a == "--metadata"));
        assert!(
            args.iter()
                .any(|a| a.contains("loom.base_commit") && a.contains("deadbeef")),
            "metadata arg missing loom.base_commit/head_commit. args={args:?}",
        );
    }
}

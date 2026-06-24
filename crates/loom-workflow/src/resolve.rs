//! Spec metadata and legacy molecule resolution helpers.
//!
//! Tree-scope mint validates `loom:spec spec:<label>` epics as metadata
//! carriers only. Missing metadata epics are created and immediately
//! closed; duplicate metadata epics refuse the tree plan before
//! remediation work is allocated. The open work-epic resolver remains for
//! legacy molecule-scoped recovery paths that still need a loopable parent.

use displaydoc::Display;
use thiserror::Error;

use loom_driver::bd::{BdClient, BdError, CommandRunner, CreateOpts, ListOpts};
use loom_driver::identifier::{BeadId, MoleculeId, SpecLabel};

pub const SPEC_METADATA_CLOSE_REASON: &str = "spec metadata carrier";

const SPEC_METADATA_STATUSES: &str = "open,in_progress,blocked,deferred,closed";

/// Failures from spec metadata or legacy work-epic resolution.
#[derive(Debug, Display, Error)]
pub enum ResolveError {
    /// bd query failed while resolving the active molecule
    Bd(#[from] BdError),
    /// multiple open epics found for spec `{label}`: {ids}; close all but one before re-running
    InvariantViolation { label: String, ids: String },
    /// duplicate loom:spec epics found for spec `{label}`: {ids}; close or relabel all but one before re-running
    DuplicateSpecEpics { label: String, ids: String },
}

/// Metadata-epic resolution result for one indexed spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSpecEpic {
    pub label: SpecLabel,
    pub action: SpecEpicAction,
}

/// Action tree-mint planning took while ensuring one metadata epic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecEpicAction {
    Existing(BeadId),
    Created(BeadId),
    WouldCreate,
}

/// Ensure exactly one `loom:spec spec:<label>` metadata epic exists.
pub async fn ensure_spec_metadata_epic<R: CommandRunner>(
    bd: &BdClient<R>,
    label: &SpecLabel,
    dry_run: bool,
) -> Result<ResolvedSpecEpic, ResolveError> {
    let beads = bd
        .list(ListOpts {
            issue_type: Some("epic".to_string()),
            label: Some(format!("spec:{}", label.as_str())),
            status: Some(SPEC_METADATA_STATUSES.to_string()),
            ..Default::default()
        })
        .await?;
    let spec_epics = beads
        .into_iter()
        .filter(|bead| bead.labels.iter().any(|label| label.is_spec_epic()))
        .collect::<Vec<_>>();
    match spec_epics.len() {
        0 if dry_run => Ok(ResolvedSpecEpic {
            label: label.clone(),
            action: SpecEpicAction::WouldCreate,
        }),
        0 => {
            let bead_id = bd
                .create(CreateOpts {
                    title: format!("loom spec: {label}"),
                    description: format!("Spec metadata epic for `{label}`."),
                    issue_type: Some("epic".to_string()),
                    priority: Some(2),
                    labels: vec!["loom:spec".to_string(), format!("spec:{label}")],
                    ..CreateOpts::default()
                })
                .await?;
            bd.close(&bead_id, Some(SPEC_METADATA_CLOSE_REASON)).await?;
            Ok(ResolvedSpecEpic {
                label: label.clone(),
                action: SpecEpicAction::Created(bead_id),
            })
        }
        1 => Ok(ResolvedSpecEpic {
            label: label.clone(),
            action: SpecEpicAction::Existing(spec_epics[0].id.clone()),
        }),
        _ => {
            let ids = spec_epics
                .iter()
                .map(|b| b.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Err(ResolveError::DuplicateSpecEpics {
                label: label.to_string(),
                ids,
            })
        }
    }
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

/// One spec's resolved legacy work-epic bonding target.
/// `was_minted` is true when [`resolve_or_mint_open_epic`] minted a
/// fresh work epic because no open work epic existed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEpic {
    pub label: SpecLabel,
    pub molecule_id: MoleculeId,
    pub was_minted: bool,
}

/// Per-spec resolve-or-mint loop for legacy molecule-scoped recovery.
/// Calls [`resolve_or_mint_open_epic`] for every label in `labels` and
/// returns the resolved epics in the same order. Stops on the first
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

/// Resolve or mint one legacy work epic for a spec.
///
/// Runs the same `bd find --type=epic --label=spec:<X> --status=open`
/// query as [`resolve_open_epic`], ignoring `loom:spec` metadata epics;
/// on zero work-epic results, mints a fresh epic via
/// `bd create --type=epic --title="<X>" --labels="spec:<X>" --metadata
/// "loom.base_commit=<head_commit>"` and returns it with
/// `was_minted = true`. More-than-one open work epics propagate as
/// [`ResolveError::InvariantViolation`].
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

    #[tokio::test]
    async fn ensure_spec_metadata_epic_creates_and_closes_missing_metadata_carrier() {
        let runner =
            ScriptedRunner::new(vec![ok_stdout("[]"), ok_stdout("lm-spec\n"), ok_stdout("")]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let label = SpecLabel::new("alpha");

        let resolved = ensure_spec_metadata_epic(&bd, &label, false)
            .await
            .expect("metadata ensure ok");

        assert_eq!(resolved.label, label);
        assert_eq!(
            resolved.action,
            SpecEpicAction::Created(BeadId::new("lm-spec").expect("valid bead id")),
        );
        let calls = invocations.lock().unwrap().clone();
        assert_eq!(calls.len(), 3, "list, create, close: {calls:?}");
        let create: Vec<String> = calls[1]
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(create.iter().any(|arg| arg == "create"), "{create:?}");
        assert!(
            create.iter().any(|arg| arg == "loom:spec,spec:alpha"),
            "{create:?}"
        );
        let close: Vec<String> = calls[2]
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            close,
            ["close", "lm-spec", "--reason", SPEC_METADATA_CLOSE_REASON],
        );
    }

    #[tokio::test]
    async fn ensure_spec_metadata_epic_accepts_existing_closed_metadata_carrier() {
        let existing = r#"[{
            "id": "lm-existing",
            "title": "loom spec: alpha",
            "status": "closed",
            "priority": 2,
            "issue_type": "epic",
            "labels": ["loom:spec", "spec:alpha"],
            "metadata": {}
        }]"#;
        let runner = ScriptedRunner::new(vec![ok_stdout(existing)]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);

        let resolved = ensure_spec_metadata_epic(&bd, &SpecLabel::new("alpha"), false)
            .await
            .expect("metadata ensure ok");

        assert_eq!(
            resolved.action,
            SpecEpicAction::Existing(BeadId::new("lm-existing").expect("valid bead id")),
        );
        let calls = invocations.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "existing metadata epic needs no writes");
    }

    #[tokio::test]
    async fn ensure_spec_metadata_epic_refuses_duplicate_metadata_carriers() {
        let duplicate = r#"[
            {"id":"lm-a","title":"loom spec: alpha","status":"open","priority":2,"issue_type":"epic","labels":["loom:spec","spec:alpha"],"metadata":{}},
            {"id":"lm-b","title":"loom spec: alpha","status":"closed","priority":2,"issue_type":"epic","labels":["loom:spec","spec:alpha"],"metadata":{}}
        ]"#;
        let runner = ScriptedRunner::new(vec![ok_stdout(duplicate)]);
        let bd = BdClient::with_runner(runner);

        let err = ensure_spec_metadata_epic(&bd, &SpecLabel::new("alpha"), false)
            .await
            .expect_err("duplicate metadata epics refuse");

        match err {
            ResolveError::DuplicateSpecEpics { label, ids } => {
                assert_eq!(label, "alpha");
                assert!(ids.contains("lm-a"), "{ids}");
                assert!(ids.contains("lm-b"), "{ids}");
            }
            other => panic!("expected DuplicateSpecEpics, got {other:?}"),
        }
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

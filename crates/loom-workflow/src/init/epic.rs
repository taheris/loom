//! Durable spec metadata and independent work-epic discovery for cache reconstruction.

use loom_driver::bd::{BdClient, Bead, CommandRunner, Label, ListOpts};
use loom_driver::identifier::SpecLabel;
use loom_driver::state::{RebuildEpic, SpecEpicRow, WorkEpicRow};
use loom_protocol::oid::GitOid;
use loom_protocol::todo::TodoFingerprint;
use serde::de::DeserializeOwned;

use super::InitError;

/// Read all spec carriers (including closed ones) and open pending/active work roots.
/// This discovery is read-only; it never invents or inherits missing metadata.
///
/// # Errors
/// Returns a typed error for failed Beads queries or inconsistent durable metadata.
pub async fn fetch_epics<R: CommandRunner>(
    bd: &BdClient<R>,
) -> Result<Vec<RebuildEpic>, InitError> {
    let beads = bd
        .list(ListOpts {
            all: true,
            issue_type: Some("epic".into()),
            limit: Some(0),
            ..ListOpts::default()
        })
        .await?;
    let mut out = Vec::new();
    for bead in beads {
        if !bead
            .labels
            .iter()
            .any(|label| label.is_spec_epic() || label.is_todo_stage() || label.is_active())
        {
            continue;
        }
        let detail = bd.show(&bead.id).await?;
        if let Some(epic) = parse_epic(&detail)? {
            out.push(epic);
        }
    }
    Ok(out)
}

fn parse_epic(bead: &Bead) -> Result<Option<RebuildEpic>, InitError> {
    let spec = bead.labels.iter().any(Label::is_spec_epic);
    let pending = bead.labels.iter().any(Label::is_todo_stage);
    let active = bead.labels.iter().any(Label::is_active);
    let epic_id = bead
        .id
        .as_str()
        .parse()
        .map_err(|source| InitError::InvalidMoleculeId { source })?;
    if (spec && (pending || active)) || (pending && active) {
        return Err(invalid(
            bead,
            "labels",
            "conflicting spec/pending/active roles",
        ));
    }
    let labels = bead
        .labels
        .iter()
        .filter_map(Label::spec_label)
        .collect::<Vec<_>>();
    if spec {
        let [label] = labels.as_slice() else {
            return Err(invalid(
                bead,
                "labels",
                "a spec epic requires exactly one spec:<label>",
            ));
        };
        let cursor = metadata::<GitOid>(bead, "loom.todo_cursor")?;
        return Ok(Some(RebuildEpic::Spec(SpecEpicRow {
            spec_label: label.clone(),
            epic_id,
            todo_cursor: cursor.map(|cursor| cursor.to_string()),
        })));
    }
    if bead.status == "closed" || (!pending && !active) {
        return Ok(None);
    }
    let head = metadata::<GitOid>(bead, "loom.todo_head")?;
    let base = metadata::<GitOid>(bead, "loom.base_commit")?;
    let (base, fingerprint) = if pending || head.is_some() {
        let head = head.ok_or_else(|| invalid(bead, "loom.todo_head", "missing todo head"))?;
        if base.is_some() {
            return Err(invalid(
                bead,
                "loom.base_commit",
                "todo work epics use loom.todo_head, not a second anchor",
            ));
        }
        let fingerprint = metadata::<TodoFingerprint>(bead, "loom.todo_fingerprint")?
            .ok_or_else(|| invalid(bead, "loom.todo_fingerprint", "missing todo fingerprint"))?;
        let mut specs = metadata::<Vec<SpecLabel>>(bead, "loom.todo_specs")?
            .ok_or_else(|| invalid(bead, "loom.todo_specs", "missing changed-spec labels"))?;
        let mut expected = labels;
        specs.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        expected.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        if specs.is_empty() || specs.windows(2).any(|pair| pair[0] == pair[1]) || specs != expected
        {
            return Err(invalid(
                bead,
                "loom.todo_specs",
                "changed specs must be nonempty, unique and match spec labels",
            ));
        }
        (head, Some(fingerprint.to_string()))
    } else {
        let base = base
            .ok_or_else(|| invalid(bead, "loom.base_commit", "missing remediation range anchor"))?;
        (base, None)
    };
    Ok(Some(RebuildEpic::Work(WorkEpicRow {
        epic_id,
        base_commit: Some(base.to_string()),
        todo_fingerprint: fingerprint,
        is_active: active,
        iteration_count: 0,
    })))
}

fn metadata<T: DeserializeOwned>(bead: &Bead, key: &'static str) -> Result<Option<T>, InitError> {
    bead.metadata
        .get(key)
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|error| invalid(bead, key, error.to_string()))
        })
        .transpose()
}

fn invalid(bead: &Bead, key: &'static str, detail: impl Into<String>) -> InitError {
    InitError::InvalidEpicMetadata {
        id: bead.id.to_string(),
        key,
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::bd::{BdError, RunOutput};
    use std::ffi::OsString;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    const HEAD: &str = "0123456789012345678901234567890123456789";
    const CURSOR: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";

    fn bead(id: &str, labels: &[&str], status: &str, metadata: serde_json::Value) -> Bead {
        let mut value = serde_json::json!({
            "id": id, "title": id, "status": status, "issue_type": "epic", "priority": 2,
            "labels": labels
        });
        value["metadata"] = metadata;
        serde_json::from_value(value).unwrap()
    }

    struct Runner {
        beads: Vec<Bead>,
        calls: Arc<Mutex<Vec<Vec<String>>>>,
    }

    impl CommandRunner for Runner {
        async fn run(&self, args: Vec<OsString>, _timeout: Duration) -> Result<RunOutput, BdError> {
            let args = args
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            self.calls.lock().unwrap().push(args.clone());
            let found = match args[0].as_str() {
                "list" => self.beads.clone(),
                "show" => self
                    .beads
                    .iter()
                    .filter(|bead| bead.id.as_str() == args[1])
                    .cloned()
                    .collect(),
                other => panic!("rebuild must be read-only, got {other}"),
            };
            Ok(RunOutput {
                status: 0,
                stdout: serde_json::to_vec(&found).unwrap(),
                stderr: Vec::new(),
            })
        }
    }

    #[tokio::test]
    async fn rebuild_discovers_closed_spec_carriers_and_independent_work_batches() {
        let fingerprint = "f".repeat(64);
        let runner = Runner {
            calls: Arc::default(),
            beads: vec![
                bead(
                    "lm-speca",
                    &["loom:spec", "spec:alpha"],
                    "closed",
                    serde_json::json!({"loom.todo_cursor": CURSOR}),
                ),
                bead(
                    "lm-specb",
                    &["loom:spec", "spec:beta"],
                    "open",
                    serde_json::json!({"loom.todo_cursor": CURSOR}),
                ),
                bead(
                    "lm-work",
                    &["loom:active", "spec:alpha", "spec:beta"],
                    "open",
                    serde_json::json!({"loom.todo_head": HEAD, "loom.todo_fingerprint": fingerprint, "loom.todo_specs": ["alpha", "beta"]}),
                ),
                bead(
                    "lm-pending",
                    &["loom:todo", "spec:alpha"],
                    "open",
                    serde_json::json!({"loom.todo_head": HEAD, "loom.todo_fingerprint": fingerprint, "loom.todo_specs": ["alpha"]}),
                ),
                bead("lm-unrelated", &[], "open", serde_json::json!({})),
            ],
        };
        let calls = runner.calls.clone();
        let bd = BdClient::with_runner(runner);
        let epics = fetch_epics(&bd).await.unwrap();
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("specs")).unwrap();
        std::fs::create_dir(directory.path().join("docs")).unwrap();
        for label in ["alpha", "beta"] {
            std::fs::write(directory.path().join(format!("specs/{label}.md")), "# spec").unwrap();
        }
        std::fs::write(
            directory.path().join("docs/README.md"),
            "[alpha](../specs/alpha.md)\n[beta](../specs/beta.md)\n",
        )
        .unwrap();
        let report = super::super::run(
            directory.path(),
            super::super::InitOpts { rebuild: true },
            &epics,
        )
        .unwrap();
        let report = report.rebuild.unwrap();
        assert_eq!(report.spec_epics, 2);
        assert_eq!(report.work_epics, 2);
        let cache =
            loom_driver::state::CacheDb::open(directory.path().join(".loom/cache.db")).unwrap();
        let work = cache
            .work_epic(&"lm-work".parse().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(work.base_commit.as_deref(), Some(HEAD));
        assert_eq!(work.todo_fingerprint.as_deref(), Some(fingerprint.as_str()));
        assert!(work.is_active);
        assert!(
            !cache
                .work_epic(&"lm-pending".parse().unwrap())
                .unwrap()
                .unwrap()
                .is_active
        );
        assert_eq!(
            cache
                .spec_epic(&"alpha".parse().unwrap())
                .unwrap()
                .unwrap()
                .todo_cursor
                .as_deref(),
            Some(CURSOR)
        );
        let calls = calls.lock().unwrap();
        assert!(calls[0].contains(&"--limit=0".to_string()));
        assert!(calls[0].contains(&"--all".to_string()));
        assert_eq!(calls.len(), 5);
    }

    #[test]
    fn rebuild_rejects_malformed_metadata_instead_of_inheriting_it() {
        for metadata in [
            serde_json::json!({"loom.todo_head": "bad"}),
            serde_json::json!({}),
        ] {
            let pending = bead("lm-work", &["loom:todo", "spec:alpha"], "open", metadata);
            assert!(matches!(
                parse_epic(&pending),
                Err(InitError::InvalidEpicMetadata {
                    key: "loom.todo_head",
                    ..
                })
            ));
        }
        let spec = bead(
            "lm-spec",
            &["loom:spec", "spec:alpha"],
            "closed",
            serde_json::json!({"loom.todo_cursor": 123}),
        );
        assert!(parse_epic(&spec).is_err());
    }

    #[test]
    fn rebuild_preserves_remediation_anchor_without_fabricating_spec_epic() {
        let work = bead(
            "lm-remediation",
            &["loom:active", "spec:alpha"],
            "open",
            serde_json::json!({"loom.base_commit": HEAD}),
        );
        let Some(RebuildEpic::Work(row)) = parse_epic(&work).unwrap() else {
            panic!("work epic")
        };
        assert_eq!(row.base_commit.as_deref(), Some(HEAD));
        assert_eq!(row.todo_fingerprint, None);
    }

    #[test]
    fn rebuild_rejects_conflicting_roles_and_spec_sets() {
        let conflict = bead(
            "lm-conflict",
            &["loom:spec", "loom:active", "spec:alpha"],
            "open",
            serde_json::json!({}),
        );
        assert!(parse_epic(&conflict).is_err());
        let pending = bead(
            "lm-work",
            &["loom:todo", "spec:alpha"],
            "open",
            serde_json::json!({"loom.todo_head": HEAD, "loom.todo_fingerprint": "f".repeat(64), "loom.todo_specs": ["beta"]}),
        );
        assert!(matches!(
            parse_epic(&pending),
            Err(InitError::InvalidEpicMetadata {
                key: "loom.todo_specs",
                ..
            })
        ));
    }
}

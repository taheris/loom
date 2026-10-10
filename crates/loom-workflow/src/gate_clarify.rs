//! Direct clarification admission requires one canonical decision brief across
//! the target's notes and description. Defective briefs become blocked repair
//! items with their producer context preserved.

use std::path::PathBuf;

use loom_driver::bd::{BdClient, BdError, CommandRunner, UpdateOpts};
use loom_driver::identifier::BeadId;
use loom_events::{DriverEventPayload, DriverKind};
use serde::Serialize;

use crate::inbox::parse_options_in;

/// Cause string written into the target bead's notes when the gate
/// downgrades a direct-emit `LOOM_CLARIFY` because the bead's notes ∪
/// description lacks a well-formed options block.
///
/// Shared with the
/// mint-side per-finding processing path so a single cause label
/// surfaces from both enforcement sites.
pub const CLARIFY_WITHOUT_OPTIONS_CAUSE: &str = "clarify-without-options";

/// Production route that requested clarify validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClarifySourceRoute {
    LoopMarker,
    TodoMarker,
    ReviewVerdict,
}

impl ClarifySourceRoute {
    const fn as_wire(self) -> &'static str {
        match self {
            Self::LoopMarker => "loop-marker",
            Self::TodoMarker => "todo-marker",
            Self::ReviewVerdict => "review-verdict",
        }
    }
}

/// Result of parsing the target's notes and description as an Options block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OptionsParseResult {
    WellFormed,
    MissingOrMalformed,
}

/// Context attached to clarify route observability events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClarifyRouteContext {
    pub source_route: ClarifySourceRoute,
    pub identity: String,
    pub gate_log_path: Option<PathBuf>,
}

/// Outcome of [`apply_clarify_or_blocked`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClarifyApplyOutcome {
    /// The target bead carried a well-formed options block; `loom:clarify`
    /// was applied per the existing direct-emit path.
    Clarify,
    /// The target bead's notes ∪ description had no well-formed options
    /// block; `loom:blocked` was applied with cause
    /// [`CLARIFY_WITHOUT_OPTIONS_CAUSE`] instead.
    BlockedClarifyWithoutOptions,
}

/// Applied clarify decision plus the evidence needed for route events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClarifyApplyReport {
    pub outcome: ClarifyApplyOutcome,
    pub options_parse_result: OptionsParseResult,
    pub evidence_hash: String,
    pub evidence_excerpt: String,
}

impl ClarifyApplyReport {
    /// Build events that must follow the successful Beads mutation.
    #[must_use]
    pub fn routing_events(
        &self,
        bead: &BeadId,
        context: &ClarifyRouteContext,
    ) -> Vec<DriverEventPayload> {
        let effective_label = match self.outcome {
            ClarifyApplyOutcome::Clarify => "loom:clarify",
            ClarifyApplyOutcome::BlockedClarifyWithoutOptions => "loom:blocked",
        };
        let mut events = Vec::with_capacity(2);
        if matches!(
            self.outcome,
            ClarifyApplyOutcome::BlockedClarifyWithoutOptions
        ) {
            events.push(DriverEventPayload::new(
                DriverKind::ClarifyDowngraded,
                format!("clarify route downgraded to blocked for {bead}"),
                serde_json::json!({
                    "source_route": context.source_route.as_wire(),
                    "identity": context.identity,
                    "options_parse_result": self.options_parse_result,
                    "evidence_hash": self.evidence_hash,
                    "evidence_excerpt": self.evidence_excerpt,
                    "cause": CLARIFY_WITHOUT_OPTIONS_CAUSE,
                    "gate_log_path": context.gate_log_path,
                }),
            ));
        }
        events.push(DriverEventPayload::new(
            DriverKind::BdStateTransition,
            format!("Beads state updated for {bead}: {effective_label}"),
            serde_json::json!({
                "source_route": context.source_route.as_wire(),
                "identity": context.identity,
                "bead_id": bead,
                "mutation": "update",
                "status": "blocked",
                "added_labels": [effective_label],
                "notes_cause": if matches!(
                    self.outcome,
                    ClarifyApplyOutcome::BlockedClarifyWithoutOptions
                ) {
                    Some(CLARIFY_WITHOUT_OPTIONS_CAUSE)
                } else {
                    None
                },
            }),
        ));
        events
    }
}

pub(crate) fn evidence_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

pub(crate) fn evidence_excerpt(text: &str) -> String {
    const LIMIT: usize = 240;

    let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = flattened.chars();
    let excerpt = chars.by_ref().take(LIMIT).collect::<String>();
    if chars.next().is_some() {
        format!("{excerpt}…")
    } else {
        excerpt
    }
}

/// Admit one unique Options brief from the target's notes and description;
/// otherwise retain a blocked repair item with its original context.
///
/// Either path
/// transitions the bead to `status=blocked` so `bd ready` excludes it
/// pending human resolution.
///
/// The persistence-boundary contract (`specs/gate.md` § *Persistence
/// boundary*) is preserved: the agent owns writing the options block
/// before emitting `LOOM_CLARIFY`; this helper only stamps the verdict
/// label and (on the downgrade path) the cause notes.
///
/// # Errors
///
/// Returns an error when workflow setup, execution, or state validation fails.
pub async fn apply_clarify_or_blocked<R: CommandRunner>(
    bd: &BdClient<R>,
    bead: &BeadId,
) -> Result<ClarifyApplyOutcome, BdError> {
    Ok(apply_clarify_or_blocked_report(bd, bead).await?.outcome)
}

/// Apply clarify validation and retain diagnostics for route observability.
///
/// # Errors
///
/// Returns an error when workflow setup, execution, or state validation fails.
pub async fn apply_clarify_or_blocked_report<R: CommandRunner>(
    bd: &BdClient<R>,
    bead: &BeadId,
) -> Result<ClarifyApplyReport, BdError> {
    let snapshot = bd.show(bead).await?;
    let mut union = snapshot.notes.clone().unwrap_or_default();
    if !union.is_empty() {
        union.push('\n');
    }
    union.push_str(&snapshot.description);

    match parse_options_in(snapshot.notes.as_deref(), &snapshot.description) {
        Ok(_) => {
            bd.update(
                bead,
                UpdateOpts {
                    status: Some(loom_driver::bd::Status::Blocked),
                    add_labels: vec!["loom:clarify".to_string()],
                    remove_labels: vec!["loom:blocked".to_string()],
                    ..UpdateOpts::default()
                },
            )
            .await?;
            Ok(ClarifyApplyReport {
                outcome: ClarifyApplyOutcome::Clarify,
                options_parse_result: OptionsParseResult::WellFormed,
                evidence_hash: evidence_hash(&union),
                evidence_excerpt: evidence_excerpt(&union),
            })
        }
        Err(error) => {
            let repair = format!("## Decision brief repair\n\n{}", error.repair_message());
            let notes = match snapshot.notes {
                Some(notes) if !notes.trim().is_empty() => {
                    if notes.contains(&repair) {
                        notes
                    } else {
                        format!("{notes}\n\n{repair}")
                    }
                }
                _ => repair,
            };
            bd.update(
                bead,
                UpdateOpts {
                    status: Some(loom_driver::bd::Status::Blocked),
                    add_labels: vec!["loom:blocked".to_string()],
                    remove_labels: vec!["loom:clarify".to_string()],
                    notes: Some(notes),
                    ..UpdateOpts::default()
                },
            )
            .await?;
            Ok(ClarifyApplyReport {
                outcome: ClarifyApplyOutcome::BlockedClarifyWithoutOptions,
                options_parse_result: OptionsParseResult::MissingOrMalformed,
                evidence_hash: evidence_hash(&union),
                evidence_excerpt: evidence_excerpt(&union),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::bd::RunOutput;
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
            self.invocations.lock().expect("not poisoned").push(args);
            let mut responses = self.responses.lock().expect("not poisoned");
            assert!(!responses.is_empty(), "ScriptedRunner exhausted");
            Ok(responses.remove(0))
        }
    }

    fn ok(body: &str) -> RunOutput {
        RunOutput {
            status: 0,
            stdout: body.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    fn bead_row(id: &str, description: &str, notes: Option<&str>) -> String {
        let notes_field = match notes {
            Some(n) => format!(r#", "notes": {}"#, serde_json::to_string(n).expect("json")),
            None => String::new(),
        };
        format!(
            r#"[{{"id":"{id}","title":"t","status":"open","priority":2,"issue_type":"task","description":{desc}{notes}}}]"#,
            desc = serde_json::to_string(description).expect("json"),
            notes = notes_field,
        )
    }

    fn argv(invocations: &Arc<Mutex<Vec<Vec<OsString>>>>) -> Vec<Vec<String>> {
        invocations
            .lock()
            .expect("not poisoned")
            .iter()
            .map(|args| {
                args.iter()
                    .map(|s| s.to_string_lossy().into_owned())
                    .collect()
            })
            .collect()
    }

    #[tokio::test]
    async fn canonical_options_brief_is_unique_and_supplies_decision_summary() {
        use crate::inbox::{InboxKind, build_inbox_context, build_queue, build_rows};
        use loom_driver::bd::{Bead, Label};
        use loom_templates::SkillIndexMarkdown;
        use std::path::Path;

        let brief = "## Options — canonical question\n\n### Option 1 — Keep\nCost: debt.\n\n### Option 2 — Change\nCost: churn.\n";
        let fenced = format!("```markdown\n{brief}```\n");
        let duplicate = format!("{brief}\n{brief}");
        let overlong = format!(
            "## Options — {}\n### Option 1 — Keep\nCost: debt.",
            "x".repeat(51)
        );
        for (notes, description, valid) in [
            (None, brief, true),
            (Some(brief), "plain description", true),
            (Some(fenced.as_str()), brief, true),
            (Some(brief), fenced.as_str(), true),
            (Some("```markdown\n"), brief, true),
            (None, "plain description", false),
            (None, fenced.as_str(), false),
            (Some(brief), brief, false),
            (None, duplicate.as_str(), false),
            (Some("## Options\n"), brief, false),
            (None, overlong.as_str(), false),
            (
                None,
                "## Options —\n### Option 1 — Keep\nCost: debt.",
                false,
            ),
            (
                None,
                "## Options — question\n### Option 2 — Keep\nCost: debt.",
                false,
            ),
            (
                None,
                "## Options — question\n### Option 1 —\nCost: debt.",
                false,
            ),
            (None, "## Options — question\n### Option 1 — Keep\n", false),
            (
                Some("## Options — question\n### Option 1 — Keep\n"),
                "plain description",
                false,
            ),
            (
                Some("## Options — question\n"),
                "### Option 1 — Keep\nCost: debt.",
                false,
            ),
        ] {
            let row = bead_row("lm-decision", description, notes);
            let mut beads: Vec<Bead> = serde_json::from_str(&row).expect("bead snapshot");
            beads[0].labels = vec![Label::new("loom:clarify").expect("label")];
            let snapshot = beads.clone();
            let parsed = parse_options_in(notes, description);
            assert_eq!(parsed.is_ok(), valid, "{notes:?} / {description}");

            let queue = build_queue(&beads, None, None, true);
            assert_eq!(queue.len(), 1);
            assert_eq!(queue[0].brief, parsed);
            assert_eq!(
                queue[0].kind,
                if valid {
                    InboxKind::Clarify
                } else {
                    InboxKind::Blocked
                }
            );
            let rows = build_rows(&queue, None);
            let context = build_inbox_context(
                Path::new("/workspace"),
                String::new(),
                Vec::new(),
                &queue,
                "/workspace/.loom/scratch/inbox/scratch.md".into(),
                SkillIndexMarkdown::empty(),
            );
            let item = &context.inbox_items[0];
            if valid {
                assert_eq!(rows[0].summary, "canonical question");
                assert_eq!(item.options_summary.as_deref(), Some("canonical question"));
                assert_eq!(item.options.len(), 2);
                assert_eq!(item.options[0].title.as_deref(), Some("Keep"));
                assert_eq!(item.options[1].body.as_deref(), Some("Cost: churn."));
            } else {
                assert_eq!(rows[0].summary, "Repair Options brief");
                assert!(item.options_summary.is_none());
                assert!(item.options.is_empty());
                let repair = queue[0].brief_repair().expect("repair diagnostic");
                assert!(repair.chars().count() < 400);
                assert!(
                    item.notes
                        .as_deref()
                        .expect("repair context")
                        .contains(&repair)
                );
            }
            assert_eq!(beads, snapshot, "queue/context must not mutate bead state");

            let runner = ScriptedRunner::new(vec![ok(&row), ok("")]);
            let invocations = runner.invocations_handle();
            let bd = BdClient::with_runner(runner);
            let report = apply_clarify_or_blocked_report(&bd, &beads[0].id)
                .await
                .expect("admission");
            assert_eq!(
                report.outcome,
                if valid {
                    ClarifyApplyOutcome::Clarify
                } else {
                    ClarifyApplyOutcome::BlockedClarifyWithoutOptions
                }
            );
            assert!(report.evidence_excerpt.chars().count() <= 241);
            let calls = argv(&invocations);
            let update = &calls[1];
            let label = if valid {
                "loom:clarify"
            } else {
                "loom:blocked"
            };
            assert!(update.windows(2).any(|args| args == ["--add-label", label]));
            if !valid {
                let repair_notes = &update[update
                    .iter()
                    .position(|arg| arg == "--notes")
                    .expect("notes flag")
                    + 1];
                assert!(repair_notes.contains(CLARIFY_WITHOUT_OPTIONS_CAUSE));
                if let Some(notes) = notes {
                    assert!(repair_notes.starts_with(notes), "preserve producer context");
                }
                assert_eq!(
                    parse_options_in(Some(repair_notes), description),
                    parsed,
                    "repair diagnostics must not complete or hide a defective brief"
                );
            }
        }
    }

    #[tokio::test]
    async fn well_formed_block_in_description_applies_clarify() {
        let description = "\
## Options — pick a path

### Option 1 — first
body
";
        let runner = ScriptedRunner::new(vec![ok(&bead_row("lm-x.1", description, None)), ok("")]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let bead = BeadId::new("lm-x.1").expect("id");
        let outcome = apply_clarify_or_blocked(&bd, &bead).await.expect("ok");
        assert_eq!(outcome, ClarifyApplyOutcome::Clarify);
        let calls = argv(&invocations);
        let update = &calls[1];
        assert!(
            update.iter().any(|a| a == "loom:clarify"),
            "loom:clarify expected: {update:?}",
        );
        assert!(
            update
                .windows(2)
                .any(|args| args == ["--remove-label", "loom:blocked"]),
            "obsolete blocked label must be removed: {update:?}",
        );
    }

    #[tokio::test]
    async fn well_formed_block_in_notes_applies_clarify() {
        let notes = "## Options — pick\n\n### Option 1 — t\nbody\n";
        let runner = ScriptedRunner::new(vec![
            ok(&bead_row("lm-x.1", "plain description", Some(notes))),
            ok(""),
        ]);
        let bd = BdClient::with_runner(runner);
        let bead = BeadId::new("lm-x.1").expect("id");
        let outcome = apply_clarify_or_blocked(&bd, &bead).await.expect("ok");
        assert_eq!(outcome, ClarifyApplyOutcome::Clarify);
    }

    #[tokio::test]
    async fn missing_options_block_downgrades_to_blocked_with_cause() {
        let runner = ScriptedRunner::new(vec![
            ok(&bead_row("lm-x.1", "Just prose, no Options heading.", None)),
            ok(""),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let bead = BeadId::new("lm-x.1").expect("id");
        let outcome = apply_clarify_or_blocked(&bd, &bead).await.expect("ok");
        assert_eq!(outcome, ClarifyApplyOutcome::BlockedClarifyWithoutOptions);
        let calls = argv(&invocations);
        let update = &calls[1];
        assert!(
            update.iter().any(|a| a == "loom:blocked"),
            "loom:blocked expected on downgrade: {update:?}",
        );
        assert!(
            update
                .windows(2)
                .any(|args| args == ["--remove-label", "loom:clarify"]),
            "defective clarify label must be removed: {update:?}",
        );
        assert!(
            update
                .iter()
                .any(|a| a.contains(CLARIFY_WITHOUT_OPTIONS_CAUSE)),
            "notes must cite cause: {update:?}",
        );
    }

    #[tokio::test]
    async fn malformed_block_summary_empty_downgrades() {
        let description = "## Options —\n\n### Option 1 — t\nbody\n";
        let runner = ScriptedRunner::new(vec![ok(&bead_row("lm-x.1", description, None)), ok("")]);
        let bd = BdClient::with_runner(runner);
        let bead = BeadId::new("lm-x.1").expect("id");
        let outcome = apply_clarify_or_blocked(&bd, &bead).await.expect("ok");
        assert_eq!(outcome, ClarifyApplyOutcome::BlockedClarifyWithoutOptions);
    }

    #[tokio::test]
    async fn clarify_downgrade_builds_driver_events_and_bd_breadcrumb() {
        let runner = ScriptedRunner::new(vec![
            ok(&bead_row(
                "lm-x.1",
                "clarify question without options",
                None,
            )),
            ok(""),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let bead = BeadId::new("lm-x.1").expect("id");
        let report = apply_clarify_or_blocked_report(&bd, &bead)
            .await
            .expect("apply downgrade");
        let events = report.routing_events(
            &bead,
            &ClarifyRouteContext {
                source_route: ClarifySourceRoute::LoopMarker,
                identity: "LOOM_CLARIFY".to_string(),
                gate_log_path: Some(PathBuf::from(".loom/logs/harness/lm-x.1.jsonl")),
            },
        );

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].driver_kind, DriverKind::ClarifyDowngraded);
        assert_eq!(events[1].driver_kind, DriverKind::BdStateTransition);
        assert_eq!(events[0].payload["cause"], CLARIFY_WITHOUT_OPTIONS_CAUSE);
        assert_eq!(
            events[0].payload["options_parse_result"],
            "missing-or-malformed"
        );
        assert!(
            events[0].payload["evidence_hash"]
                .as_str()
                .is_some_and(|hash| !hash.is_empty())
        );

        let calls = argv(&invocations);
        let update = &calls[1];
        assert!(update.iter().any(|arg| arg == "loom:blocked"));
        assert!(
            update
                .iter()
                .any(|arg| arg.contains(CLARIFY_WITHOUT_OPTIONS_CAUSE))
        );
    }
}

//! Verdict-gate direct-emit `LOOM_CLARIFY` validation.
//!
//! When the agent self-reports `LOOM_CLARIFY`, the verdict gate inspects
//! the target bead's notes ∪ description for a well-formed `## Options —
//! <summary>` heading with at least one `### Option <N> — <title>`
//! subsection (per `specs/gate.md` § *Options Format Contract*). A
//! well-formed block applies `loom:clarify`; an absent or malformed
//! block downgrades to `loom:blocked` carrying
//! [`CLARIFY_WITHOUT_OPTIONS_CAUSE`] so `loom msg`'s queue is not handed
//! an empty options block.
//!
//! Target bead for direct-emit is the bead under dispatch for the
//! `loop` / `review` phases and the molecule epic for the `todo_*`
//! phases (`specs/templates.md` § Decomposition Discipline).

use loom_driver::bd::{BdClient, BdError, CommandRunner, UpdateOpts};
use loom_driver::identifier::BeadId;
use loom_protocol::gate::options::has_well_formed_block;

/// Cause string written into the target bead's notes when the gate
/// downgrades a direct-emit `LOOM_CLARIFY` because the bead's notes ∪
/// description lacks a well-formed options block. Shared with the
/// mint-side per-finding processing path so a single cause label
/// surfaces from both enforcement sites.
pub const CLARIFY_WITHOUT_OPTIONS_CAUSE: &str = "clarify-without-options";

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

/// Inspect the target bead's notes ∪ description for a well-formed
/// options block; apply `loom:clarify` when found, `loom:blocked` with
/// cause [`CLARIFY_WITHOUT_OPTIONS_CAUSE`] otherwise. Either path
/// transitions the bead to `status=blocked` so `bd ready` excludes it
/// pending human resolution.
///
/// The persistence-boundary contract (`specs/gate.md` § *Persistence
/// boundary*) is preserved: the agent owns writing the options block
/// before emitting `LOOM_CLARIFY`; this helper only stamps the verdict
/// label and (on the downgrade path) the cause notes.
pub async fn apply_clarify_or_blocked<R: CommandRunner>(
    bd: &BdClient<R>,
    bead: &BeadId,
) -> Result<ClarifyApplyOutcome, BdError> {
    let snapshot = bd.show(bead).await?;
    let mut union = snapshot.notes.unwrap_or_default();
    if !union.is_empty() {
        union.push('\n');
    }
    union.push_str(&snapshot.description);

    if has_well_formed_block(&union) {
        bd.update(
            bead,
            UpdateOpts {
                status: Some("blocked".to_string()),
                add_labels: vec!["loom:clarify".to_string()],
                ..UpdateOpts::default()
            },
        )
        .await?;
        Ok(ClarifyApplyOutcome::Clarify)
    } else {
        bd.update(
            bead,
            UpdateOpts {
                status: Some("blocked".to_string()),
                add_labels: vec!["loom:blocked".to_string()],
                notes: Some(CLARIFY_WITHOUT_OPTIONS_CAUSE.to_string()),
                ..UpdateOpts::default()
            },
        )
        .await?;
        Ok(ClarifyApplyOutcome::BlockedClarifyWithoutOptions)
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
            !update.iter().any(|a| a == "loom:blocked"),
            "loom:blocked must NOT be applied when block is well-formed: {update:?}",
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
            !update.iter().any(|a| a == "loom:clarify"),
            "loom:clarify MUST NOT be applied on malformed block: {update:?}",
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
}

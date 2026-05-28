//! Walk orchestration that drives the mint pipeline's two Finding sources.
//!
//! Per `specs/gate.md` § *Findings and Minting* and § *Scope-dependent walk*
//! the mint walk varies by scope:
//!
//! - `--bead <id>` / `--diff <range>` / `--files <paths>` — invoke ONLY the
//!   LLM rubric agent process. Deterministic verifier failures are NOT
//!   normalised into Finding records at these scopes; the loop's preceding
//!   `verify --bead <id>` step has already handled them as `previous_failure`
//!   recovery context.
//! - `--tree` — invoke the deterministic verifier dispatcher first,
//!   collecting failed verdicts; then invoke the LLM rubric. Both sources
//!   flow into the same per-Finding pipeline. There is no shell-level
//!   `LOOM_FINDING` line for verify-side findings — only the in-driver
//!   record, normalised per the mapping table at `specs/gate.md` § *Emit
//!   shape* (verifier outcome → token/target/bonds).
//!
//! The orchestration layer is the seam between [`crate::mint`]'s
//! `mint_findings_with_options` and the two emit sources; it returns a
//! single ordered `Vec<Finding>` ready for the dedup → bonding-lead → mint
//! state machine. Idempotency vs partial failure is structural: the mint
//! pipeline's narrow `status=open` dedup query is what skips
//! already-minted findings on re-run, so the walk doesn't carry state
//! across invocations.

use std::path::PathBuf;

use displaydoc::Display;
use loom_driver::identifier::{BeadId, SpecLabel};
use loom_gate::Annotation;
use thiserror::Error;

use crate::review::{
    ConcernToken, Finding, FindingTarget, FindingValidator, WalkOutputError, parse_walk_output,
};

/// Resolved mint walk scope. Mirrors the `--bead` / `--diff` / `--files` /
/// `--tree` CLI flag the operator passed to `loom gate mint`; the
/// orchestration layer dispatches per variant. The CLI is responsible
/// for resolving defaults (per `specs/gate.md` § *Default for bare
/// invocation*) before constructing this value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MintScope {
    Bead(BeadId),
    Diff(String),
    Files(Vec<PathBuf>),
    Tree,
}

impl MintScope {
    /// True iff the scope walks deterministic verifiers in addition to the
    /// LLM rubric. Only `--tree` does (per `specs/gate.md` § *Scope-
    /// dependent walk*).
    #[must_use]
    pub fn runs_verifiers(&self) -> bool {
        matches!(self, Self::Tree)
    }
}

/// One deterministic-verifier failure surfaced by the dispatch layer at
/// tree scope. The orchestration normalises each into a typed
/// [`Finding`] via [`verifier_failure_to_finding`] per the mapping table
/// at `specs/gate.md` § *Emit shape*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifierFailure {
    /// The annotation whose verifier ran (or could not run). Carries the
    /// owning spec via `source_spec`, which is what populates `bonds`.
    pub annotation: Annotation,
    pub kind: VerifierFailureKind,
    /// Evidence text from the verifier's JSON `evidence` field (else
    /// stderr tail / dispatch-error message). Stored verbatim on the
    /// minted fix-up bead's description.
    pub evidence: String,
}

/// Categorised failure mode for a deterministic verifier dispatch
/// outcome. Variants correspond row-by-row to the verifier-outcome
/// mapping table at `specs/gate.md` § *Emit shape*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifierFailureKind {
    /// `[check]` / `[test]` / `[system]` exit ≠ 0 (and ≠ 2, ≠ 77).
    Failed,
    /// Dispatch error — exit code 2: command not found, missing
    /// prerequisite, runner unknown.
    DispatchError,
    /// Integrity gate forward-resolution failure: the annotation's
    /// target does not resolve for its tier.
    UnresolvedAnnotation,
    /// Integrity gate stub-pointing: the annotation's verifier body
    /// invokes the `_pending_stub` sigil.
    StubPointing,
    /// Integrity gate atomic-acceptance violation: the criterion at
    /// `criterion_anchor` carries `count` annotations (expected 1).
    MultipleAnnotations {
        count: usize,
        criterion_anchor: String,
    },
}

/// Errors raised by the walk orchestration. Variants carry the
/// underlying failure source so the CLI can route specifics back to the
/// operator (e.g. spec-label parse error name + offending path).
#[derive(Debug, Display, Error)]
pub enum WalkError {
    /// LLM rubric agent process failed: {0}
    Rubric(String),
    /// deterministic verifier dispatch failed: {0}
    Verifiers(String),
    /// rubric stdout parse failed: {0}
    Parse(#[from] WalkOutputError),
    /// owning spec file `{path}` has no parseable spec label
    SpecLabel { path: PathBuf },
}

/// Abstracts the two side-effect-bearing surfaces the orchestration
/// depends on so the walk logic stays pure and is exercised under fakes
/// in tests. Production wires `run_rubric` to the existing review-agent
/// invocation in [`crate::review::runner`] and `run_verifiers` to the
/// deterministic dispatcher in [`loom_gate::dispatch`] plus the
/// integrity gate's resolver chain.
pub trait MintWalker: Send {
    /// Run the LLM rubric agent for `scope` and return its raw stdout
    /// (the buffer the `LOOM_FINDING:` / terminal-marker parsers consume).
    fn run_rubric(
        &mut self,
        scope: &MintScope,
    ) -> impl std::future::Future<Output = Result<String, WalkError>> + Send;

    /// Run every deterministic verifier in scope at tree scope and
    /// return one [`VerifierFailure`] per failed dispatch outcome.
    /// Implementations MUST NOT invoke this when `scope` is not
    /// [`MintScope::Tree`]; the orchestration only calls it on tree
    /// scope (the trait method takes `scope` so production
    /// implementations can fan out per-spec, not because non-tree
    /// scopes are valid here).
    fn run_verifiers(
        &mut self,
        scope: &MintScope,
    ) -> impl std::future::Future<Output = Result<Vec<VerifierFailure>, WalkError>> + Send;
}

/// Top-level walk: run the configured sources for `scope`, normalise
/// any verifier failures into typed `Finding` records, parse the rubric
/// stdout, and return the combined ordered vector for the mint pipeline.
///
/// Order: verifier-side findings come first (in dispatch order), rubric
/// findings come second (in stdout order). Both share the same dedup
/// fingerprint scheme, so order only affects the end-of-run summary's
/// per-finding lines — not which beads end up minted.
pub async fn walk<W: MintWalker, V: FindingValidator + ?Sized>(
    walker: &mut W,
    scope: &MintScope,
    validator: &V,
) -> Result<Vec<Finding>, WalkError> {
    let mut findings = Vec::new();
    if scope.runs_verifiers() {
        let failures = walker.run_verifiers(scope).await?;
        for failure in failures {
            findings.push(verifier_failure_to_finding(failure)?);
        }
    }
    let rubric_stdout = walker.run_rubric(scope).await?;
    let parsed = parse_walk_output(&rubric_stdout, validator)?;
    findings.extend(parsed);
    Ok(findings)
}

/// Normalise one [`VerifierFailure`] into a typed [`Finding`] per the
/// mapping at `specs/gate.md` § *Emit shape*. The owning spec for
/// `bonds` is derived from the annotation's `source_spec` path
/// (basename minus `.md`) — the same spec-section auto-include the
/// verifier's input set uses.
pub fn verifier_failure_to_finding(failure: VerifierFailure) -> Result<Finding, WalkError> {
    let owning = spec_label_from_path(&failure.annotation.source_spec)?;
    let target_string = failure.annotation.target.clone();
    let (token, target) = match failure.kind {
        VerifierFailureKind::Failed => (
            ConcernToken::VerifierFailed,
            FindingTarget::Annotation { target_string },
        ),
        VerifierFailureKind::DispatchError => (
            ConcernToken::DispatchError,
            FindingTarget::Annotation { target_string },
        ),
        VerifierFailureKind::UnresolvedAnnotation => (
            ConcernToken::UnresolvedAnnotation,
            FindingTarget::Annotation { target_string },
        ),
        VerifierFailureKind::StubPointing => (
            ConcernToken::StubPointing,
            FindingTarget::Annotation { target_string },
        ),
        VerifierFailureKind::MultipleAnnotations {
            criterion_anchor, ..
        } => (
            ConcernToken::MultipleAnnotations,
            FindingTarget::Criterion {
                spec: owning.clone(),
                anchor: criterion_anchor,
            },
        ),
    };
    Ok(Finding {
        token,
        bonds: vec![owning],
        target,
        evidence: failure.evidence,
    })
}

fn spec_label_from_path(path: &std::path::Path) -> Result<SpecLabel, WalkError> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.parse::<SpecLabel>().ok())
        .ok_or_else(|| WalkError::SpecLabel {
            path: path.to_path_buf(),
        })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use loom_driver::bd::{BdClient, BdError, CommandRunner, RunOutput};
    use loom_driver::identifier::BeadId;
    use loom_gate::{Annotation, Tier};

    use super::*;
    use crate::mint::{FindingOutcome, MintOptions, mint_findings_with_options};
    use crate::review::{LOOM_FINDING_PREFIX, TargetKind};

    fn spec(s: &str) -> SpecLabel {
        s.parse().expect("valid spec label")
    }

    fn annotation(tier: Tier, target: &str, source_spec: &str) -> Annotation {
        Annotation {
            tier,
            target: target.to_owned(),
            source_spec: PathBuf::from(source_spec),
            line: 1,
            criterion_line: 1,
            pending: false,
        }
    }

    /// `FindingValidator` implementation that admits everything. The walk
    /// orchestration tests don't care about Layer-3/Layer-5 resolution —
    /// they care about *which sources* contribute findings under each
    /// scope. The pure parse/validation tests live in
    /// [`crate::review::finding::tests`] and exercise the strict
    /// validator paths separately.
    struct AlwaysValid;

    impl FindingValidator for AlwaysValid {
        fn spec_label_is_known(&self, _label: &SpecLabel) -> bool {
            true
        }
        fn criterion_anchor_resolves(&self, _spec: &SpecLabel, _anchor: &str) -> bool {
            true
        }
        fn annotation_resolves(&self, _target_string: &str) -> bool {
            true
        }
        fn file_exists(&self, _path: &str) -> bool {
            true
        }
        fn invariant_resolves(&self, _spec: &SpecLabel, _section: &str, _tag: &str) -> bool {
            true
        }
    }

    /// Walker fake that returns canned outputs and tracks per-method call
    /// counts so tests can assert which sources fired for a given scope.
    #[derive(Default)]
    struct FakeWalker {
        rubric_stdout: String,
        verifier_failures: Vec<VerifierFailure>,
        rubric_calls: usize,
        verifier_calls: usize,
        observed_scopes: Vec<MintScope>,
    }

    impl MintWalker for FakeWalker {
        async fn run_rubric(&mut self, scope: &MintScope) -> Result<String, WalkError> {
            self.rubric_calls += 1;
            self.observed_scopes.push(scope.clone());
            Ok(self.rubric_stdout.clone())
        }

        async fn run_verifiers(
            &mut self,
            scope: &MintScope,
        ) -> Result<Vec<VerifierFailure>, WalkError> {
            self.verifier_calls += 1;
            self.observed_scopes.push(scope.clone());
            Ok(self.verifier_failures.clone())
        }
    }

    fn finding_line(payload: &str) -> String {
        format!("{LOOM_FINDING_PREFIX} {payload}")
    }

    /// Spec contract `specs/gate.md` § *Scope-dependent walk*
    /// (criterion `mint_bead_scope_walks_llm_rubric_only_not_verifiers`,
    /// gate.md:1433-1437): at `--bead <id>` / `--diff <range>` /
    /// `--files <paths>` scope, the walk runs ONLY the LLM rubric agent;
    /// the deterministic verifier dispatcher MUST NOT fire because the
    /// loop's preceding `verify --bead <id>` step has already handled
    /// verify-side failures as `previous_failure` recovery context.
    ///
    /// Pinning the no-call here is what makes the scope-dependent
    /// behaviour structural: if a future refactor accidentally always
    /// invoked the verifier dispatcher, this test catches it before
    /// per-bead findings start double-reporting.
    #[tokio::test]
    async fn mint_bead_scope_walks_llm_rubric_only_not_verifiers() {
        let rubric = format!(
            "preamble\n{}\nLOOM_CONCERN: spec-coherence-fail -- found one\n",
            finding_line(
                r#"{"token":"spec-coherence-fail","bonds":["gate"],"target":{"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"},"evidence":"e"}"#
            ),
        );
        let mut walker = FakeWalker {
            rubric_stdout: rubric,
            // verifier_failures must NEVER reach the per-Finding loop on
            // bead scope — populate the slot anyway so the assertion
            // proves the dispatcher was skipped (not just empty).
            verifier_failures: vec![VerifierFailure {
                annotation: annotation(Tier::Check, "would-not-fire", "specs/gate.md"),
                kind: VerifierFailureKind::Failed,
                evidence: "should not appear".into(),
            }],
            ..FakeWalker::default()
        };
        let scope = MintScope::Bead(BeadId::new("lm-loop.1").expect("valid"));
        let findings = walk(&mut walker, &scope, &AlwaysValid)
            .await
            .expect("walk succeeds");
        assert_eq!(walker.rubric_calls, 1, "rubric ran exactly once");
        assert_eq!(
            walker.verifier_calls, 0,
            "deterministic verifiers MUST NOT run on bead scope",
        );
        assert_eq!(
            findings.len(),
            1,
            "only the rubric finding reaches the mint pipeline: {findings:?}",
        );
        assert_eq!(findings[0].token, ConcernToken::SpecCoherenceFail);

        // The same property holds for --diff and --files scopes.
        for scope in [
            MintScope::Diff("HEAD".into()),
            MintScope::Files(vec![PathBuf::from("src/lib.rs")]),
        ] {
            let mut w = FakeWalker {
                rubric_stdout: "no findings\n".into(),
                verifier_failures: vec![VerifierFailure {
                    annotation: annotation(Tier::Check, "x", "specs/gate.md"),
                    kind: VerifierFailureKind::Failed,
                    evidence: "y".into(),
                }],
                ..FakeWalker::default()
            };
            let _ = walk(&mut w, &scope, &AlwaysValid).await.expect("walk");
            assert_eq!(
                w.verifier_calls, 0,
                "non-tree scope skips verifiers: {scope:?}"
            );
        }
    }

    /// Spec contract `specs/gate.md` § *Scope-dependent walk* (criterion
    /// `mint_tree_scope_walks_verifiers_and_rubric_emitting_findings_from_both`,
    /// gate.md:1437-1441): at `--tree` scope, the walk invokes BOTH the
    /// deterministic verifier dispatcher and the LLM rubric. Both
    /// sources feed into the same per-Finding loop; the verifier failures
    /// are normalised in-driver into typed Finding records (no
    /// shell-level `LOOM_FINDING:` line for them) per the mapping table
    /// at gate.md:583-590.
    #[tokio::test]
    async fn mint_tree_scope_walks_verifiers_and_rubric_emitting_findings_from_both() {
        let rubric = format!(
            "preamble\n{}\n{}\nLOOM_CONCERN: orphan-integration -- two findings\n",
            finding_line(
                r#"{"token":"orphan-integration","bonds":["harness"],"target":{"kind":"Contract","id":"molecule-lifecycle"},"evidence":"contract"}"#
            ),
            finding_line(
                r#"{"token":"style-rule-violation","bonds":["gate"],"target":{"kind":"StyleRule","rule_id":"RS-19"},"evidence":"style"}"#
            ),
        );
        // Two verifier-side failures across different categories — one
        // dispatch error, one verify failure — so the mapping table's
        // multi-token coverage gets exercised.
        let mut walker = FakeWalker {
            rubric_stdout: rubric,
            verifier_failures: vec![
                VerifierFailure {
                    annotation: annotation(
                        Tier::Check,
                        "cargo run -p loom-walk -- nonexistent",
                        "specs/gate.md",
                    ),
                    kind: VerifierFailureKind::DispatchError,
                    evidence: "command not found".into(),
                },
                VerifierFailure {
                    annotation: annotation(
                        Tier::Test,
                        "crate::module::failing_test",
                        "specs/harness.md",
                    ),
                    kind: VerifierFailureKind::Failed,
                    evidence: "assertion failed".into(),
                },
            ],
            ..FakeWalker::default()
        };
        let findings = walk(&mut walker, &MintScope::Tree, &AlwaysValid)
            .await
            .expect("walk succeeds");
        assert_eq!(walker.rubric_calls, 1, "rubric ran exactly once");
        assert_eq!(walker.verifier_calls, 1, "verifiers ran exactly once");
        assert_eq!(findings.len(), 4, "both sources contribute: {findings:?}");

        // Verifier-side findings appear first (dispatch order), each
        // normalised to the spec-mandated token + target shape.
        assert_eq!(findings[0].token, ConcernToken::DispatchError);
        assert_eq!(findings[0].target.kind(), TargetKind::Annotation);
        assert_eq!(
            findings[0].bonds,
            vec![spec("gate")],
            "owning spec is derived from annotation.source_spec",
        );
        assert_eq!(findings[1].token, ConcernToken::VerifierFailed);
        assert_eq!(findings[1].target.kind(), TargetKind::Annotation);
        assert_eq!(findings[1].bonds, vec![spec("harness")]);

        // Rubric-side findings follow, in stdout order.
        assert_eq!(findings[2].token, ConcernToken::OrphanIntegration);
        assert_eq!(findings[3].token, ConcernToken::StyleRuleViolation);
    }

    /// Spec contract `specs/gate.md` § *Findings and Minting* (criterion
    /// `mint_walk_emits_loom_finding_json_lines_streamed_per_finding`,
    /// gate.md:1369-1373): the walk's stdout emits `LOOM_FINDING: <json>`
    /// lines one-per-finding as findings are identified (not batched at
    /// end-of-walk). The parser side is unit-tested in
    /// [`crate::review::finding::tests`]; this is the integration side
    /// that drives a real walk through the orchestration and asserts the
    /// stream-shape semantics — every line in the stdout buffer becomes
    /// one typed Finding, in stdout order.
    #[tokio::test]
    async fn mint_walk_emits_loom_finding_json_lines_streamed_per_finding() {
        // Three findings interleaved with prose so the test pins
        // "one line per finding, stdout-order" rather than "batched at
        // end-of-walk". A batched emit would either lose interleaved
        // ordering or collapse adjacent payloads.
        let rubric = format!(
            "preamble before any findings\n\
             {a}\n\
             intermediate prose between findings\n\
             {b}\n\
             still more prose\n\
             {c}\n\
             trailing summary\n\
             LOOM_CONCERN: spec-coherence-fail -- three findings\n",
            a = finding_line(
                r#"{"token":"spec-coherence-fail","bonds":["gate"],"target":{"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"},"evidence":"first"}"#
            ),
            b = finding_line(
                r#"{"token":"orphan-integration","bonds":["harness"],"target":{"kind":"Contract","id":"molecule-lifecycle"},"evidence":"second"}"#
            ),
            c = finding_line(
                r#"{"token":"style-rule-violation","bonds":["gate"],"target":{"kind":"StyleRule","rule_id":"COM-1"},"evidence":"third"}"#
            ),
        );
        let mut walker = FakeWalker {
            rubric_stdout: rubric,
            ..FakeWalker::default()
        };
        let findings = walk(&mut walker, &MintScope::Diff("HEAD".into()), &AlwaysValid)
            .await
            .expect("walk succeeds");

        assert_eq!(
            findings.len(),
            3,
            "every LOOM_FINDING line becomes one Finding: {findings:?}",
        );
        // Stable stdout order — first finding emitted is findings[0].
        assert_eq!(findings[0].evidence, "first");
        assert_eq!(findings[1].evidence, "second");
        assert_eq!(findings[2].evidence, "third");
        // Tagged-target enum deserialised by `kind`.
        assert_eq!(findings[0].target.kind(), TargetKind::Criterion);
        assert_eq!(findings[1].target.kind(), TargetKind::Contract);
        assert_eq!(findings[2].target.kind(), TargetKind::StyleRule);
    }

    /// Bd runner that hands back canned [`RunOutput`]s in order and
    /// records every argv it spawned. Used by the idempotency test to
    /// script a partial-failure first pass followed by a successful
    /// retry; the per-call assertions key off the recorded argv.
    struct ScriptedRunner {
        responses: Mutex<Vec<Result<RunOutput, BdError>>>,
        invocations: Arc<Mutex<Vec<Vec<String>>>>,
    }

    impl ScriptedRunner {
        fn new(responses: Vec<Result<RunOutput, BdError>>) -> Self {
            Self {
                responses: Mutex::new(responses),
                invocations: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn invocations_handle(&self) -> Arc<Mutex<Vec<Vec<String>>>> {
            Arc::clone(&self.invocations)
        }
    }

    impl CommandRunner for ScriptedRunner {
        async fn run(
            &self,
            args: Vec<std::ffi::OsString>,
            _t: std::time::Duration,
        ) -> Result<RunOutput, BdError> {
            self.invocations.lock().expect("not poisoned").push(
                args.iter()
                    .map(|s| s.to_string_lossy().into_owned())
                    .collect(),
            );
            let mut responses = self.responses.lock().expect("not poisoned");
            assert!(
                !responses.is_empty(),
                "ScriptedRunner: no more responses queued (got args {args:?})",
            );
            responses.remove(0)
        }
    }

    fn ok_stdout(body: &str) -> Result<RunOutput, BdError> {
        Ok(RunOutput {
            status: 0,
            stdout: body.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }

    fn epic_list(id: &str, label: &str) -> String {
        format!(
            r#"[{{"id":"{id}","title":"{label}: epic","status":"open","priority":2,"issue_type":"epic","labels":["spec:{label}"]}}]"#,
        )
    }

    fn fixup_row(id: &str, fingerprint: &str) -> String {
        format!(
            r#"{{"id":"{id}","title":"existing","status":"open","priority":2,"issue_type":"task","labels":["loom:mint:{fingerprint}"]}}"#,
        )
    }

    /// Spec contract `specs/gate.md` § *Findings and Minting* (criterion
    /// `mint_idempotent_after_partial_failure_retries_only_unfinished_findings`,
    /// gate.md:1441-1446): a crash mid-run leaves successfully-minted
    /// beads with their fingerprint labels; the next mint invocation's
    /// dedup query (`bd list --label=loom:mint:<fp> --status=open`)
    /// matches the surviving bead and skips it, retrying only the
    /// findings that didn't reach `bd create` on the prior run.
    ///
    /// The walk's idempotency is structurally a property of the
    /// mint-pipeline's narrow `status=open` dedup query — not the walk
    /// itself — so this test exercises the walk *plus* the mint pipeline
    /// end-to-end. Scenario:
    ///   pass 1: findings [A, B, C]
    ///     - A: dedup empty → epic resolves → bd create succeeds (lm-A)
    ///     - B: dedup empty → epic resolves → bd create FAILS (script
    ///       returns an error)
    ///     - C: dedup empty → epic resolves → bd create succeeds (lm-C)
    ///         (mint pipeline records B as Errored and keeps going per
    ///         the existing `mint_findings_with_options` contract)
    ///   pass 2: same findings [A, B, C], same walk output
    ///     - A: dedup returns lm-A (open) → SkippedDedup
    ///     - B: dedup empty (never minted) → epic → create succeeds
    ///     - C: dedup returns lm-C (open) → SkippedDedup
    #[tokio::test]
    async fn mint_idempotent_after_partial_failure_retries_only_unfinished_findings() {
        // Build three findings with deterministic, distinct fingerprints
        // so the dedup-by-fingerprint argv is checkable.
        let finding_a = Finding {
            token: ConcernToken::SpecCoherenceFail,
            bonds: vec![spec("gate")],
            target: FindingTarget::Criterion {
                spec: spec("gate"),
                anchor: "verifier-honesty".into(),
            },
            evidence: "A".into(),
        };
        let finding_b = Finding {
            token: ConcernToken::OrphanIntegration,
            bonds: vec![spec("gate")],
            target: FindingTarget::Contract {
                id: "molecule-lifecycle".into(),
            },
            evidence: "B".into(),
        };
        let finding_c = Finding {
            token: ConcernToken::StyleRuleViolation,
            bonds: vec![spec("gate")],
            target: FindingTarget::StyleRule {
                rule_id: "RS-19".into(),
            },
            evidence: "C".into(),
        };
        let fp_a = finding_a.fingerprint();
        let fp_b = finding_b.fingerprint();
        let fp_c = finding_c.fingerprint();
        let findings = vec![finding_a.clone(), finding_b.clone(), finding_c.clone()];

        // --- Pass 1 ----------------------------------------------------
        // A: dedup empty, epic lookup hit, create OK.
        // B: dedup empty, epic lookup hit, create FAILS (simulated crash
        //    or transient bd error).
        // C: dedup empty, epic lookup hit, create OK.
        let pass1_responses: Vec<Result<RunOutput, BdError>> = vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout("lm-onea.1\n"),
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            Err(BdError::Spawn(std::io::Error::other(
                "simulated mid-run crash",
            ))),
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout("lm-threec.1\n"),
        ];
        let runner = ScriptedRunner::new(pass1_responses);
        let bd = BdClient::with_runner(runner);
        let summary1 =
            mint_findings_with_options(&bd, &findings, "head-sha", &MintOptions::default()).await;
        assert_eq!(summary1.minted, 2, "A and C succeed on pass 1");
        assert_eq!(summary1.errors, 1, "B fails on pass 1");
        // Confirm fingerprints reach the bd-list query argv so the
        // re-run can find them.
        let pass1_a_minted = summary1
            .findings
            .iter()
            .find(
                |o| matches!(o, FindingOutcome::Minted { fingerprint, .. } if fingerprint == &fp_a),
            )
            .expect("pass 1 minted A");
        assert!(matches!(pass1_a_minted, FindingOutcome::Minted { .. }));
        assert!(summary1.findings.iter().any(
            |o| matches!(o, FindingOutcome::Errored { fingerprint, .. } if fingerprint == &fp_b),
        ));

        // --- Pass 2 ----------------------------------------------------
        // A's bead still carries its mint label → dedup returns 1 hit →
        // SkippedDedup. B never minted → dedup empty → mint succeeds.
        // C's bead still open → dedup returns 1 hit → SkippedDedup.
        let pass2_responses: Vec<Result<RunOutput, BdError>> = vec![
            ok_stdout(&format!("[{}]", fixup_row("lm-onea.1", &fp_a))),
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout("lm-twob.1\n"),
            ok_stdout(&format!("[{}]", fixup_row("lm-threec.1", &fp_c))),
        ];
        let runner2 = ScriptedRunner::new(pass2_responses);
        let invocations2 = runner2.invocations_handle();
        let bd2 = BdClient::with_runner(runner2);
        let summary2 =
            mint_findings_with_options(&bd2, &findings, "head-sha", &MintOptions::default()).await;
        assert_eq!(summary2.minted, 1, "only B mints on pass 2 (retry)");
        assert_eq!(summary2.skipped, 2, "A and C are dedup-skipped on pass 2");
        assert_eq!(summary2.errors, 0, "no errors on pass 2");
        assert_eq!(summary2.refused, 0);

        // The B-only retry must call `bd create` exactly once on pass 2.
        // Snapshot the recorded argv so the lock guard is dropped before
        // the next `.await` in this test (clippy::await_holding_lock).
        let pass2_calls: Vec<Vec<String>> = invocations2.lock().expect("not poisoned").clone();
        let create_calls = pass2_calls
            .iter()
            .filter(|c| c.iter().any(|a| a == "create"))
            .count();
        assert_eq!(
            create_calls, 1,
            "pass 2 retries ONLY the unfinished finding (B): {pass2_calls:?}",
        );
        // The dedup queries for A and C must restrict to status=open so
        // closed beads are not surfaced (no re-mint of closed work) and
        // a still-open fingerprinted bead is found.
        let dedup_calls = pass2_calls
            .iter()
            .filter(|c| {
                c.iter().any(|a| a == "list")
                    && c.iter().any(|a| a == "--status=open")
                    && c.iter()
                        .any(|a| a.starts_with("--label=loom:mint:") && !a.contains("epic"))
            })
            .count();
        assert!(
            dedup_calls >= 2,
            "dedup query runs once per finding on pass 2: {pass2_calls:?}",
        );

        // Cross-pass invariant: the fingerprints stayed stable between
        // passes (same finding identity ⇒ same dedup key).
        let pass2_a_skip = summary2.findings.iter().find(|o| {
            matches!(o, FindingOutcome::SkippedDedup { existing_bead, .. } if existing_bead.as_str() == "lm-onea.1")
        });
        assert!(
            pass2_a_skip.is_some(),
            "pass 2 must dedup against the original lm-onea.1 bead: {:?}",
            summary2.findings,
        );

        // The walk-orchestration story end-to-end: a walker returning the
        // same Findings on both passes leaves the system in the same
        // converged state (every finding has an open fix-up bead, mint
        // queue is drained). Exercise that property by running the walk
        // through the orchestration on pass 2's bd state via the fake
        // walker — the orchestration emits the same Findings, the mint
        // pipeline dedups against pass 1's labels, and the run converges
        // with zero new errors.
        let mut walker_pass2 = FakeWalker {
            rubric_stdout: rubric_for_three(&finding_a, &finding_b, &finding_c),
            ..FakeWalker::default()
        };
        let parsed = walk(&mut walker_pass2, &MintScope::Tree, &AlwaysValid)
            .await
            .expect("walk parses");
        let fingerprints: std::collections::HashSet<String> =
            parsed.iter().map(Finding::fingerprint).collect();
        let expected: std::collections::HashSet<String> = [fp_a, fp_b, fp_c].into_iter().collect();
        assert_eq!(
            fingerprints, expected,
            "walk emits the same fingerprints across runs — the dedup key is stable",
        );

        // Pass 2 outcomes form a complete map of (fingerprint -> resolved bead id) — proves no finding fell through.
        let fingerprint_to_bead: HashMap<String, BeadId> = summary2
            .findings
            .iter()
            .filter_map(|o| match o {
                FindingOutcome::Minted {
                    fingerprint,
                    bead_id,
                    ..
                }
                | FindingOutcome::SkippedDedup {
                    fingerprint,
                    existing_bead: bead_id,
                } => Some((fingerprint.clone(), bead_id.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            fingerprint_to_bead.len(),
            3,
            "every finding terminates in either Minted or SkippedDedup on pass 2: {fingerprint_to_bead:?}",
        );
    }

    fn rubric_for_three(a: &Finding, b: &Finding, c: &Finding) -> String {
        let line_a = finding_line(&serde_json::to_string(a).expect("serialize"));
        let line_b = finding_line(&serde_json::to_string(b).expect("serialize"));
        let line_c = finding_line(&serde_json::to_string(c).expect("serialize"));
        format!(
            "{line_a}\n{line_b}\n{line_c}\nLOOM_CONCERN: orphan-integration -- three findings\n"
        )
    }

    /// `verifier_failure_to_finding` covers every spec-mandated mapping
    /// row at gate.md:583-590. Pinned per-row so adding a new
    /// `VerifierFailureKind` variant without updating the mapping fails
    /// here rather than in a downstream consumer.
    #[test]
    fn verifier_failure_mapping_per_spec_table() {
        let ann = annotation(
            Tier::Check,
            "cargo run -p loom-walk -- foo",
            "specs/gate.md",
        );
        let cases = [
            (VerifierFailureKind::Failed, ConcernToken::VerifierFailed),
            (
                VerifierFailureKind::DispatchError,
                ConcernToken::DispatchError,
            ),
            (
                VerifierFailureKind::UnresolvedAnnotation,
                ConcernToken::UnresolvedAnnotation,
            ),
            (
                VerifierFailureKind::StubPointing,
                ConcernToken::StubPointing,
            ),
        ];
        for (kind, expected_token) in cases {
            let finding = verifier_failure_to_finding(VerifierFailure {
                annotation: ann.clone(),
                kind,
                evidence: "e".into(),
            })
            .expect("ok");
            assert_eq!(finding.token, expected_token);
            assert_eq!(finding.target.kind(), TargetKind::Annotation);
            assert_eq!(finding.bonds, vec![spec("gate")]);
        }

        let finding = verifier_failure_to_finding(VerifierFailure {
            annotation: ann,
            kind: VerifierFailureKind::MultipleAnnotations {
                count: 2,
                criterion_anchor: "some-anchor".into(),
            },
            evidence: "criterion carries 2 annotations".into(),
        })
        .expect("ok");
        assert_eq!(finding.token, ConcernToken::MultipleAnnotations);
        assert_eq!(finding.target.kind(), TargetKind::Criterion);
        match &finding.target {
            FindingTarget::Criterion { spec: s, anchor } => {
                assert_eq!(s, &spec("gate"));
                assert_eq!(anchor, "some-anchor");
            }
            other => panic!("expected Criterion target, got {other:?}"),
        }
    }
}

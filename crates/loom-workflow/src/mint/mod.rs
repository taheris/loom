//! Driver-side mint pipeline.
//!
//! Consumes [`Finding`] records produced by either the LLM rubric's
//! `LOOM_FINDING:` lines or the deterministic verifier verdict normaliser
//! and runs each through the four-stage state machine pinned in
//! `specs/gate.md` § *Per-finding processing*:
//!
//! 1. **fingerprint** — `loom:mint:<hash>`, identity-only (excludes
//!    `bonds` so bonding shifts across runs do not re-mint).
//! 2. **dedup** — `bd list --label=loom:mint:<fp> --status=open`. Zero
//!    proceeds; one skips with the existing bead id; more than one is a
//!    per-finding [`FindingOutcome::Refused`] surfacing the conflicting
//!    bead ids (the run keeps processing remaining findings).
//! 3. **bonding lead** — walk `bonds` in order; the first whose
//!    [`resolve_open_epic`] returns exactly one open epic wins. None
//!    open ⇒ `lead = bonds[0]` and [`resolve_or_mint_open_epic`] mints a
//!    fresh molecule + epic. The same `>1 open epic` invariant
//!    propagates from [`crate::resolve::ResolveError::InvariantViolation`]
//!    as a per-finding refuse.
//! 4. **mint** — one `bd create --type=task --parent=<lead-epic>
//!    --labels=loom:mint:<fp>,spec:<bonds[0]>,...`. The
//!    [`ConcernToken::InvariantClash`] carve-out tacks on `loom:clarify`
//!    and trusts the rubric to have emitted the canonical `## Options —
//!    …` block in the finding's `evidence` field, which the description
//!    embeds verbatim.
//!
//! The end-of-run [`MintSummary::render`] surface is stdout-only — the
//! mint pipeline performs no other writes outside the four-stage flow.

mod error;

pub use error::MintError;

use loom_driver::bd::{BdClient, CommandRunner, CreateOpts, ListOpts};
use loom_driver::identifier::{BeadId, MoleculeId, SpecLabel};

use crate::resolve::{ResolveError, resolve_open_epic, resolve_or_mint_open_epic};
use crate::review::{ConcernToken, Finding};

/// Bd label prefix the dedup query uses.
///
/// The mint pipeline emits one such label per finding. Spec contract:
/// `bd list --label=loom:mint:<fingerprint> --status=open` is the
/// dedup query, narrow `status=open` so closed-then-not-reopened beads
/// stay closed (operator silence read as decision).
pub const MINT_LABEL_PREFIX: &str = "loom:mint:";

/// Construct the bd label that carries a finding's fingerprint, e.g.
/// `loom:mint:0123456789ab`. The mint pipeline writes this label on every
/// minted fix-up and queries it during dedup.
#[must_use]
pub fn mint_label(fingerprint: &str) -> String {
    format!("{MINT_LABEL_PREFIX}{fingerprint}")
}

/// One processed finding's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindingOutcome {
    /// A fix-up bead was minted; `bead_id` is the new bd id, `lead_spec`
    /// names which `bonds` element supplied the parent epic.
    Minted {
        fingerprint: String,
        bead_id: BeadId,
        lead_spec: SpecLabel,
    },
    /// `--dry-run` mode: the pipeline resolved the bonding lead and
    /// would have created a fix-up, but did not invoke `bd create`.
    /// `lead_spec` names which `bonds` element supplied the parent epic.
    WouldMint {
        fingerprint: String,
        lead_spec: SpecLabel,
    },
    /// An open fix-up already exists for this fingerprint; nothing
    /// minted. `existing_bead` is the dedup query's single hit.
    SkippedDedup {
        fingerprint: String,
        existing_bead: BeadId,
    },
    /// `--spec <X>` filter dropped this finding because its bonding
    /// lead resolved to a different spec.
    SkippedFilter {
        fingerprint: String,
        lead_spec: SpecLabel,
        requested: SpecLabel,
    },
    /// Structural violation — either multiple open beads share the
    /// fingerprint label, or the lead spec has more than one open epic.
    /// `reason` carries the conflicting ids so the operator can resolve
    /// before re-running.
    Refused { fingerprint: String, reason: String },
    /// Unexpected failure (bd CLI failure, parse failure, …) — the
    /// finding could not be processed but the run continued.
    Errored {
        fingerprint: String,
        message: String,
    },
}

impl FindingOutcome {
    /// Stable kebab-case wire name for log/summary surfaces.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Minted { .. } => "minted",
            Self::WouldMint { .. } => "would-mint",
            Self::SkippedDedup { .. } => "skipped-dedup",
            Self::SkippedFilter { .. } => "skipped-filter",
            Self::Refused { .. } => "refused",
            Self::Errored { .. } => "errored",
        }
    }
}

/// Options that gate writes and filter scope on a [`mint_findings`] run.
///
/// Defaults match the production `loom gate mint` invocation with no
/// flags: write to bd, no spec filter.
#[derive(Debug, Clone, Default)]
pub struct MintOptions {
    /// When `true`, the pipeline runs every read-side query (dedup,
    /// lead resolution) but skips `bd create`. The resulting outcome is
    /// [`FindingOutcome::WouldMint`] instead of [`FindingOutcome::Minted`].
    pub dry_run: bool,
    /// When `Some(label)`, findings whose bonding lead resolves to a
    /// different spec are reported as [`FindingOutcome::SkippedFilter`]
    /// and no fix-up is minted. `None` admits every finding.
    pub spec_filter: Option<SpecLabel>,
}

/// End-of-run summary printed to stdout (no bd writes).
///
/// Tallies match `specs/gate.md` § *Per-finding processing* end-of-run
/// shape: `minted M, skipped K (dedup), refused R, errors E`. The
/// `--dry-run` and `--spec` filter pseudo-outcomes carry their own
/// tallies so summaries from those modes remain self-describing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MintSummary {
    pub findings: Vec<FindingOutcome>,
    pub minted: usize,
    pub would_mint: usize,
    pub skipped: usize,
    pub skipped_filter: usize,
    pub refused: usize,
    pub errors: usize,
}

impl MintSummary {
    fn record(&mut self, outcome: FindingOutcome) {
        match &outcome {
            FindingOutcome::Minted { .. } => self.minted += 1,
            FindingOutcome::WouldMint { .. } => self.would_mint += 1,
            FindingOutcome::SkippedDedup { .. } => self.skipped += 1,
            FindingOutcome::SkippedFilter { .. } => self.skipped_filter += 1,
            FindingOutcome::Refused { .. } => self.refused += 1,
            FindingOutcome::Errored { .. } => self.errors += 1,
        }
        self.findings.push(outcome);
    }

    /// Render the summary in the stdout shape spec'd for end-of-run.
    /// One-line header followed by per-finding lines naming the
    /// fingerprint and resulting bead id.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!(
            "minted {}, skipped {} (dedup), refused {}, errors {}",
            self.minted, self.skipped, self.refused, self.errors,
        );
        if self.would_mint > 0 {
            out.push_str(&format!(", would-mint {} (dry-run)", self.would_mint));
        }
        if self.skipped_filter > 0 {
            out.push_str(&format!(
                ", skipped {} (--spec filter)",
                self.skipped_filter
            ));
        }
        out.push('\n');
        for outcome in &self.findings {
            match outcome {
                FindingOutcome::Minted {
                    fingerprint,
                    bead_id,
                    lead_spec,
                } => {
                    out.push_str(&format!(
                        "  minted {fingerprint} → {bead_id} (spec:{lead_spec})\n",
                    ));
                }
                FindingOutcome::WouldMint {
                    fingerprint,
                    lead_spec,
                } => {
                    out.push_str(&format!("  would-mint {fingerprint} (spec:{lead_spec})\n",));
                }
                FindingOutcome::SkippedDedup {
                    fingerprint,
                    existing_bead,
                } => {
                    out.push_str(&format!(
                        "  skipped {fingerprint} (existing {existing_bead})\n",
                    ));
                }
                FindingOutcome::SkippedFilter {
                    fingerprint,
                    lead_spec,
                    requested,
                } => {
                    out.push_str(&format!(
                        "  skipped {fingerprint} (lead spec:{lead_spec} ≠ requested spec:{requested})\n",
                    ));
                }
                FindingOutcome::Refused {
                    fingerprint,
                    reason,
                } => {
                    out.push_str(&format!("  refused {fingerprint}: {reason}\n"));
                }
                FindingOutcome::Errored {
                    fingerprint,
                    message,
                } => {
                    out.push_str(&format!("  error {fingerprint}: {message}\n"));
                }
            }
        }
        out
    }
}

/// Walk a sequence of findings through the mint pipeline with default
/// options (write to bd, no spec filter). Convenience wrapper over
/// [`mint_findings_with_options`].
pub async fn mint_findings<R: CommandRunner>(
    bd: &BdClient<R>,
    findings: &[Finding],
    head_commit: &str,
) -> MintSummary {
    mint_findings_with_options(bd, findings, head_commit, &MintOptions::default()).await
}

/// Walk a sequence of findings through the mint pipeline. Errors that
/// represent per-finding structural violations
/// ([`MintError::DuplicateMintLabel`] and
/// [`ResolveError::InvariantViolation`]) are folded into
/// [`FindingOutcome::Refused`] so the run keeps going; other
/// [`MintError`] variants become [`FindingOutcome::Errored`].
pub async fn mint_findings_with_options<R: CommandRunner>(
    bd: &BdClient<R>,
    findings: &[Finding],
    head_commit: &str,
    opts: &MintOptions,
) -> MintSummary {
    let mut summary = MintSummary::default();
    for finding in findings {
        let fingerprint = finding.fingerprint();
        let outcome = match mint_finding_with_options(bd, finding, head_commit, opts).await {
            Ok(outcome) => outcome,
            Err(MintError::DuplicateMintLabel { ids, count, .. }) => FindingOutcome::Refused {
                fingerprint,
                reason: format!(
                    "{count} open beads share mint label — close all but one before re-running (ids: {ids})",
                ),
            },
            Err(MintError::Resolve(ResolveError::InvariantViolation { label, ids })) => {
                FindingOutcome::Refused {
                    fingerprint,
                    reason: format!(
                        "more than one open epic for spec `{label}` — close all but one before re-running (ids: {ids})",
                    ),
                }
            }
            Err(err) => FindingOutcome::Errored {
                fingerprint,
                message: err.to_string(),
            },
        };
        summary.record(outcome);
    }
    summary
}

/// Process one finding through the dedup → lead → mint pipeline with
/// default options. Convenience wrapper over
/// [`mint_finding_with_options`].
pub async fn mint_finding<R: CommandRunner>(
    bd: &BdClient<R>,
    finding: &Finding,
    head_commit: &str,
) -> Result<FindingOutcome, MintError> {
    mint_finding_with_options(bd, finding, head_commit, &MintOptions::default()).await
}

/// Process one finding through the dedup → lead → mint pipeline.
///
/// With `opts.dry_run == true`, the read-side queries still run but
/// `bd create` is suppressed and the result is
/// [`FindingOutcome::WouldMint`].
///
/// With `opts.spec_filter == Some(label)`, the lead is resolved as
/// usual; if the resolved lead is not `label`, the finding is reported
/// as [`FindingOutcome::SkippedFilter`] and `bd create` is suppressed.
pub async fn mint_finding_with_options<R: CommandRunner>(
    bd: &BdClient<R>,
    finding: &Finding,
    head_commit: &str,
    opts: &MintOptions,
) -> Result<FindingOutcome, MintError> {
    let fingerprint = finding.fingerprint();
    let label = mint_label(&fingerprint);

    let open_with_label = bd
        .list(ListOpts {
            status: Some("open".to_string()),
            label: Some(label.clone()),
            ..ListOpts::default()
        })
        .await?;
    match open_with_label.len() {
        0 => {}
        1 => {
            return Ok(FindingOutcome::SkippedDedup {
                fingerprint,
                existing_bead: open_with_label[0].id.clone(),
            });
        }
        n => {
            let ids = open_with_label
                .iter()
                .map(|b| b.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(MintError::DuplicateMintLabel {
                fingerprint,
                count: n,
                ids,
            });
        }
    }

    let (lead_spec, lead_epic) = resolve_lead(bd, &finding.bonds, head_commit).await?;

    if let Some(requested) = &opts.spec_filter
        && &lead_spec != requested
    {
        return Ok(FindingOutcome::SkippedFilter {
            fingerprint,
            lead_spec,
            requested: requested.clone(),
        });
    }

    if opts.dry_run {
        return Ok(FindingOutcome::WouldMint {
            fingerprint,
            lead_spec,
        });
    }

    let labels = mint_labels(finding, &label);
    let title = mint_title(finding);
    let description = mint_description(finding, &fingerprint);
    let parent = BeadId::new(lead_epic.as_str()).map_err(|source| MintError::InvalidParentId {
        molecule: lead_epic.to_string(),
        source,
    })?;
    let bead_id = bd
        .create(CreateOpts {
            title,
            description,
            issue_type: Some("task".to_string()),
            labels,
            parent: Some(parent),
            ..CreateOpts::default()
        })
        .await?;
    Ok(FindingOutcome::Minted {
        fingerprint,
        bead_id,
        lead_spec,
    })
}

/// Walk `bonds` in order; the first whose `bd find --type=epic
/// --label=spec:<X> --status=open` returns exactly one open epic wins.
/// If none has an open epic, `lead = bonds[0]` and a fresh molecule +
/// epic is minted for it (single-tier resolution per
/// `specs/harness.md` § *Molecule lifecycle*).
async fn resolve_lead<R: CommandRunner>(
    bd: &BdClient<R>,
    bonds: &[SpecLabel],
    head_commit: &str,
) -> Result<(SpecLabel, MoleculeId), MintError> {
    for spec in bonds {
        if let Some(mol) = resolve_open_epic(bd, spec).await? {
            return Ok((spec.clone(), mol));
        }
    }
    let lead = bonds[0].clone();
    let resolved = resolve_or_mint_open_epic(bd, &lead, head_commit).await?;
    Ok((lead, resolved.molecule_id))
}

/// Compose the bd-label list for the minted fix-up:
/// `loom:mint:<fp>`, one `spec:<X>` per bonds entry, and the
/// `invariant-clash` carve-out's `loom:clarify`.
fn mint_labels(finding: &Finding, mint_label: &str) -> Vec<String> {
    let mut labels = Vec::with_capacity(finding.bonds.len() + 2);
    labels.push(mint_label.to_string());
    for spec in &finding.bonds {
        labels.push(format!("spec:{spec}"));
    }
    if finding.token == ConcernToken::InvariantClash {
        labels.push("loom:clarify".to_string());
    }
    labels
}

/// Deterministic title — same finding always mints with the same title
/// across runs, so a closed-then-reopened bead's title still matches
/// the next walk's perceived shape.
fn mint_title(finding: &Finding) -> String {
    format!(
        "{token}: {target}",
        token = finding.token.as_wire(),
        target = finding.target.canonical_form(),
    )
}

/// Description embeds the rubric's evidence verbatim and the fingerprint
/// label, so a reader of the bead can correlate it back to the walk's
/// emit and the dedup machinery. For `invariant-clash`, the evidence is
/// expected to already contain the canonical `## Options — …` block
/// (the rubric is responsible for that — see `specs/gate.md` §
/// "Invariant-clash carve-out"), so this function preserves it verbatim.
fn mint_description(finding: &Finding, fingerprint: &str) -> String {
    let mut out = String::with_capacity(finding.evidence.len() + 96);
    out.push_str(finding.evidence.trim_end());
    out.push_str("\n\n---\n\n");
    out.push_str(&format!(
        "Fingerprint: `{MINT_LABEL_PREFIX}{fingerprint}`\n"
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::bd::{BdError, RunOutput};
    use std::ffi::OsString;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use crate::review::FindingTarget;

    fn spec(s: &str) -> SpecLabel {
        s.parse().expect("valid spec label")
    }

    fn coherence_finding(bonds: Vec<SpecLabel>, anchor: &str, evidence: &str) -> Finding {
        Finding {
            token: ConcernToken::SpecCoherenceFail,
            target: FindingTarget::Criterion {
                spec: bonds[0].clone(),
                anchor: anchor.to_owned(),
            },
            bonds,
            evidence: evidence.to_owned(),
        }
    }

    fn contract_finding(bonds: Vec<SpecLabel>, id: &str, evidence: &str) -> Finding {
        Finding {
            token: ConcernToken::OrphanIntegration,
            target: FindingTarget::Contract { id: id.to_owned() },
            bonds,
            evidence: evidence.to_owned(),
        }
    }

    fn invariant_clash_finding(
        bonds: Vec<SpecLabel>,
        spec: SpecLabel,
        section: &str,
        tag: &str,
        evidence: &str,
    ) -> Finding {
        Finding {
            token: ConcernToken::InvariantClash,
            target: FindingTarget::Invariant {
                spec,
                section: section.to_owned(),
                tag: tag.to_owned(),
            },
            bonds,
            evidence: evidence.to_owned(),
        }
    }

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

    fn rendered_calls(invocations: &Arc<Mutex<Vec<Vec<OsString>>>>) -> Vec<Vec<String>> {
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

    impl CommandRunner for ScriptedRunner {
        async fn run(&self, args: Vec<OsString>, _t: Duration) -> Result<RunOutput, BdError> {
            self.invocations.lock().expect("not poisoned").push(args);
            let response = {
                let mut responses = self.responses.lock().expect("not poisoned");
                assert!(
                    !responses.is_empty(),
                    "ScriptedRunner: no more responses queued",
                );
                responses.remove(0)
            };
            Ok(response)
        }
    }

    fn ok_stdout(body: &str) -> RunOutput {
        RunOutput {
            status: 0,
            stdout: body.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    fn epic_row(id: &str, label: &str) -> String {
        format!(
            r#"{{
                "id": "{id}",
                "title": "{label}: epic",
                "status": "open",
                "priority": 2,
                "issue_type": "epic",
                "labels": ["spec:{label}"]
            }}"#,
        )
    }

    fn epic_list(id: &str, label: &str) -> String {
        format!("[{}]", epic_row(id, label))
    }

    fn fixup_row(id: &str, fingerprint: &str) -> String {
        format!(
            r#"{{
                "id": "{id}",
                "title": "existing fix-up",
                "status": "open",
                "priority": 2,
                "issue_type": "task",
                "labels": ["{MINT_LABEL_PREFIX}{fingerprint}"]
            }}"#,
        )
    }

    /// Spec contract `specs/gate.md` § *Findings and Minting* (criterion
    /// `mint_dedup_query_one_open_result_skips_finding`): the dedup
    /// query returning exactly one open hit causes the finding to be
    /// skipped (no `bd create` fires), and the outcome carries the
    /// existing bead id.
    #[tokio::test]
    async fn mint_dedup_query_one_open_result_skips_finding() {
        let finding = coherence_finding(vec![spec("gate")], "verifier-honesty", "evidence");
        let fp = finding.fingerprint();
        let runner = ScriptedRunner::new(vec![ok_stdout(&format!(
            "[{}]",
            fixup_row("lm-existing.1", &fp),
        ))]);
        let bd = BdClient::with_runner(runner);
        let outcome = mint_finding(&bd, &finding, "head-sha")
            .await
            .expect("dedup skip is not an error");
        match outcome {
            FindingOutcome::SkippedDedup {
                fingerprint,
                existing_bead,
            } => {
                assert_eq!(fingerprint, fp);
                assert_eq!(existing_bead.as_str(), "lm-existing.1");
            }
            other => panic!("expected SkippedDedup, got {other:?}"),
        }
    }

    /// Spec contract: the dedup query returning zero results proceeds
    /// to mint. Pins that the pipeline does NOT mint when one open
    /// result is present (already covered above) but DOES proceed when
    /// none is.
    #[tokio::test]
    async fn mint_dedup_query_zero_results_proceeds_to_mint() {
        let finding = coherence_finding(vec![spec("gate")], "verifier-honesty", "evidence");
        let fp = finding.fingerprint();
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout("lm-newfix.1\n"),
        ]);
        let bd = BdClient::with_runner(runner);
        let outcome = mint_finding(&bd, &finding, "head-sha")
            .await
            .expect("mint succeeds");
        match outcome {
            FindingOutcome::Minted {
                fingerprint,
                bead_id,
                lead_spec,
            } => {
                assert_eq!(fingerprint, fp);
                assert_eq!(bead_id.as_str(), "lm-newfix.1");
                assert_eq!(lead_spec.as_str(), "gate");
            }
            other => panic!("expected Minted, got {other:?}"),
        }
    }

    /// Spec contract: more than one open result on the dedup query is a
    /// structural violation; the per-finding pipeline refuses (no mint)
    /// and surfaces both conflicting bead ids in the error.
    #[tokio::test]
    async fn mint_dedup_query_multiple_open_results_refuses_as_structural_violation() {
        let finding = coherence_finding(vec![spec("gate")], "verifier-honesty", "evidence");
        let fp = finding.fingerprint();
        let dedup_response = format!(
            "[{},{}]",
            fixup_row("lm-dup.1", &fp),
            fixup_row("lm-dup.2", &fp),
        );
        let runner = ScriptedRunner::new(vec![ok_stdout(&dedup_response)]);
        let bd = BdClient::with_runner(runner);
        let err = mint_finding(&bd, &finding, "head-sha")
            .await
            .expect_err("must refuse on dup");
        match err {
            MintError::DuplicateMintLabel {
                fingerprint,
                count,
                ids,
            } => {
                assert_eq!(fingerprint, fp);
                assert_eq!(count, 2);
                assert!(ids.contains("lm-dup.1"), "ids={ids}");
                assert!(ids.contains("lm-dup.2"), "ids={ids}");
            }
            other => panic!("expected DuplicateMintLabel, got {other:?}"),
        }
    }

    /// Spec contract: a closed bead carrying the same fingerprint
    /// label is NOT re-minted on subsequent runs. The dedup query
    /// filters by `status=open`, so a closed bead is not surfaced and
    /// the operator's silence (close-and-leave-it) sticks.
    #[tokio::test]
    async fn mint_dedup_does_not_re_mint_closed_bead_with_same_fingerprint() {
        let finding = coherence_finding(vec![spec("gate")], "verifier-honesty", "evidence");
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout("lm-newfix.7\n"),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let outcome = mint_finding(&bd, &finding, "head-sha").await.unwrap();
        assert!(matches!(outcome, FindingOutcome::Minted { .. }));
        let calls = rendered_calls(&invocations);
        let dedup_call = &calls[0];
        assert!(
            dedup_call.iter().any(|a| a == "--status=open"),
            "dedup query must filter status=open so closed beads stay closed: {dedup_call:?}",
        );
        assert!(
            !dedup_call.iter().any(|a| a == "--status=closed"),
            "dedup query MUST NOT broaden to closed beads: {dedup_call:?}",
        );
    }

    /// Spec contract: reopening a closed fingerprint-labelled bead does
    /// NOT force re-mint — the reopened bead still carries the mint
    /// label and so the next dedup query matches it (one open hit ⇒
    /// skip). Same dedup machinery; what proves the contract is that the
    /// outcome is `SkippedDedup` naming the reopened bead.
    #[tokio::test]
    async fn mint_dedup_skips_reopened_bead_still_carrying_fingerprint_label() {
        let finding = contract_finding(vec![spec("gate")], "molecule-lifecycle", "evidence");
        let fp = finding.fingerprint();
        let dedup_response = format!("[{}]", fixup_row("lm-reopened.3", &fp));
        let runner = ScriptedRunner::new(vec![ok_stdout(&dedup_response)]);
        let bd = BdClient::with_runner(runner);
        let outcome = mint_finding(&bd, &finding, "head-sha").await.unwrap();
        match outcome {
            FindingOutcome::SkippedDedup { existing_bead, .. } => {
                assert_eq!(existing_bead.as_str(), "lm-reopened.3");
            }
            other => panic!("expected SkippedDedup, got {other:?}"),
        }
    }

    /// Spec contract: the bonding lead is the first element of the
    /// finding's `bonds` array whose spec has an open epic. The pipeline
    /// walks bonds in order, queries each spec's open-epic, and stops at
    /// the first hit — earlier-listed specs with NO open epic are
    /// skipped over rather than treated as the lead.
    #[tokio::test]
    async fn mint_bonding_lead_is_first_bonds_element_with_open_epic() {
        let finding = contract_finding(
            vec![spec("harness"), spec("gate")],
            "molecule-lifecycle",
            "contract closure broken across two specs",
        );
        // Responses: dedup (no existing fix-up), harness has no epic,
        // gate has one open epic, then bd create.
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout("lm-fix.42\n"),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let outcome = mint_finding(&bd, &finding, "head-sha")
            .await
            .expect("mint ok");
        match outcome {
            FindingOutcome::Minted {
                lead_spec, bead_id, ..
            } => {
                assert_eq!(
                    lead_spec.as_str(),
                    "gate",
                    "lead must be the first bonds element with an open epic, not bonds[0]",
                );
                assert_eq!(bead_id.as_str(), "lm-fix.42");
            }
            other => panic!("expected Minted, got {other:?}"),
        }
        let calls = rendered_calls(&invocations);
        let harness_query = &calls[1];
        let gate_query = &calls[2];
        assert!(
            harness_query.iter().any(|a| a == "--label=spec:harness"),
            "first bonds element queried first: {harness_query:?}",
        );
        assert!(
            gate_query.iter().any(|a| a == "--label=spec:gate"),
            "second bonds element queried after first miss: {gate_query:?}",
        );
    }

    /// Spec contract `specs/gate.md` § *Standing-safety-net bonding*:
    /// `loom gate mint --tree --spec <X>` resolves the bonding target
    /// via a single bd query. The pipeline's per-finding resolution
    /// must walk through `resolve_open_epic` / `resolve_or_mint_open_epic`
    /// — one query, no tier walk, no pointer table. We pin that by
    /// reading the recorded argv for the single bd list and asserting
    /// it carries `--type=epic`, `--label=spec:<X>`, `--status=open`.
    #[tokio::test]
    async fn mint_tree_scope_resolves_lead_spec_via_single_tier_query() {
        let finding = coherence_finding(vec![spec("alpha")], "x", "evidence");
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-alphaepic", "alpha")),
            ok_stdout("lm-fix.1\n"),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let _ = mint_finding(&bd, &finding, "deadbeef").await.unwrap();
        let calls = rendered_calls(&invocations);
        let lead_query = &calls[1];
        assert!(
            lead_query.iter().any(|a| a == "list"),
            "lead resolution must be a single `bd list` query: {lead_query:?}",
        );
        assert!(
            lead_query.iter().any(|a| a == "--type=epic"),
            "lead query must restrict to type=epic: {lead_query:?}",
        );
        assert!(
            lead_query.iter().any(|a| a == "--label=spec:alpha"),
            "lead query must carry spec label: {lead_query:?}",
        );
        assert!(
            lead_query.iter().any(|a| a == "--status=open"),
            "lead query must restrict to status=open: {lead_query:?}",
        );
    }

    /// Spec contract: when the lead query returns one open epic, the
    /// pipeline bonds the fix-up to that existing epic — it MUST NOT
    /// mint a duplicate molecule. This pins that the resolve.rs `was_minted`
    /// branch never fires when an epic is already open.
    #[tokio::test]
    async fn mint_tree_scope_per_spec_resolution_does_not_clobber_existing_epics() {
        let finding = coherence_finding(vec![spec("alpha")], "x", "evidence");
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-alphaexist", "alpha")),
            ok_stdout("lm-fix.1\n"),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let outcome = mint_finding(&bd, &finding, "head-sha").await.unwrap();
        let calls = rendered_calls(&invocations);
        let create_calls = calls
            .iter()
            .filter(|c| c.iter().any(|a| a == "create"))
            .collect::<Vec<_>>();
        assert_eq!(
            create_calls.len(),
            1,
            "exactly one bd create (the fix-up bead) — no duplicate epic creation: {create_calls:?}",
        );
        let create = create_calls[0];
        let type_idx = create
            .iter()
            .position(|a| a == "--type")
            .expect("--type flag");
        assert_eq!(
            create[type_idx + 1],
            "task",
            "the one bd create is the fix-up task, not a duplicate epic: {create:?}",
        );
        assert!(
            matches!(outcome, FindingOutcome::Minted { ref lead_spec, .. } if lead_spec.as_str() == "alpha"),
        );
    }

    /// Spec contract `specs/gate.md` § *Per-finding processing* step 6:
    /// the minted fix-up bead is `--parent`-ed to the lead spec's open
    /// epic and carries the `loom:mint:<fingerprint>` label, plus one
    /// `spec:<X>` label per `bonds` element so cross-spec searches
    /// surface it from every owner's perspective.
    #[tokio::test]
    async fn mint_creates_fixup_with_parent_epic_and_fingerprint_label() {
        let finding = contract_finding(
            vec![spec("gate"), spec("harness")],
            "molecule-lifecycle",
            "evidence",
        );
        let fp = finding.fingerprint();
        // bonds = [gate, harness] — gate has an open epic ⇒ resolution
        // stops at the first one; second bond doesn't get queried.
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout("lm-fix.7\n"),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let _ = mint_finding(&bd, &finding, "head-sha").await.unwrap();
        let calls = rendered_calls(&invocations);
        let create = calls
            .iter()
            .find(|c| c.iter().any(|a| a == "create"))
            .expect("bd create call recorded");

        let parent_idx = create
            .iter()
            .position(|a| a == "--parent")
            .expect("--parent flag");
        assert_eq!(create[parent_idx + 1], "lm-gateepic", "calls={create:?}");

        let labels_idx = create
            .iter()
            .position(|a| a == "--labels")
            .expect("--labels flag");
        let labels = &create[labels_idx + 1];
        assert!(
            labels.contains(&format!("{MINT_LABEL_PREFIX}{fp}")),
            "labels missing fingerprint: {labels}",
        );
        assert!(
            labels.contains("spec:gate"),
            "labels missing spec:gate: {labels}",
        );
        assert!(
            labels.contains("spec:harness"),
            "labels missing spec:harness for cross-spec search: {labels}",
        );

        let type_idx = create.iter().position(|a| a == "--type").expect("--type");
        assert_eq!(
            create[type_idx + 1],
            "task",
            "fix-up is a task, not an epic"
        );
    }

    /// Spec contract: `invariant-clash` findings mint a fix-up carrying
    /// both `loom:mint:<fp>` and `loom:clarify`, with the description
    /// embedding a canonical `## Options — …` block per the *Options
    /// Format Contract*. The rubric writes the options block into the
    /// finding's `evidence`; the driver lifts it verbatim into the
    /// description.
    #[tokio::test]
    async fn mint_invariant_clash_finding_creates_fixup_with_clarify_label_and_options_block() {
        let options_block = "## Options — keep loom out of podman\n\n\
                             ### Option 1 — Preserve the invariant\n\
                             rework podman call to delegate to wrapix.\n\n\
                             ### Option 2 — Carry the contradiction\n\
                             record exception in specs/harness.md.\n\n\
                             ### Option 3 — Change the invariant\n\
                             remove the no-podman invariant from specs/harness.md.\n";
        let finding = invariant_clash_finding(
            vec![spec("harness")],
            spec("harness"),
            "Out of Scope",
            "loom-runs-podman",
            options_block,
        );
        let fp = finding.fingerprint();
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-harnessepic", "harness")),
            ok_stdout("lm-clarify.1\n"),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let outcome = mint_finding(&bd, &finding, "head-sha").await.unwrap();
        assert!(matches!(outcome, FindingOutcome::Minted { .. }));
        let calls = rendered_calls(&invocations);
        let create = calls
            .iter()
            .find(|c| c.iter().any(|a| a == "create"))
            .expect("create");

        let labels_idx = create
            .iter()
            .position(|a| a == "--labels")
            .expect("--labels");
        let labels = &create[labels_idx + 1];
        assert!(
            labels.contains(&format!("{MINT_LABEL_PREFIX}{fp}")),
            "labels missing fingerprint: {labels}",
        );
        assert!(
            labels.contains("loom:clarify"),
            "invariant-clash must carry loom:clarify: {labels}",
        );

        let desc_idx = create
            .iter()
            .position(|a| a == "--description")
            .expect("--description");
        let description = &create[desc_idx + 1];
        assert!(
            description.contains("## Options"),
            "description must embed the Options block: {description}",
        );
        assert!(
            description.contains("### Option 1"),
            "description must preserve the Options block headings verbatim: {description}",
        );
        assert!(
            description.contains(&format!("{MINT_LABEL_PREFIX}{fp}")),
            "description must cite the fingerprint: {description}",
        );
    }

    /// Spec contract: the end-of-run summary lists minted, skipped-dedup,
    /// refused, and errored counts with per-finding fingerprint +
    /// resulting bead id. Drives one finding through each outcome
    /// category (mint, dedup-skip, refuse on dup labels) and asserts the
    /// tallies + per-finding lines.
    #[tokio::test]
    async fn mint_end_of_run_summary_reports_per_finding_outcomes() {
        let f_mint = coherence_finding(vec![spec("gate")], "verifier-honesty", "evidence-a");
        let f_skip = contract_finding(vec![spec("harness")], "contract-x", "evidence-b");
        let f_refuse = contract_finding(vec![spec("gate")], "contract-y", "evidence-c");
        let fp_mint = f_mint.fingerprint();
        let fp_skip = f_skip.fingerprint();
        let fp_refuse = f_refuse.fingerprint();

        let responses = vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout("lm-newfix.1\n"),
            ok_stdout(&format!("[{}]", fixup_row("lm-existing.2", &fp_skip))),
            ok_stdout(&format!(
                "[{},{}]",
                fixup_row("lm-dup.1", &fp_refuse),
                fixup_row("lm-dup.2", &fp_refuse),
            )),
        ];
        let runner = ScriptedRunner::new(responses);
        let bd = BdClient::with_runner(runner);
        let summary = mint_findings(&bd, &[f_mint, f_skip, f_refuse], "head-sha").await;
        assert_eq!(summary.minted, 1);
        assert_eq!(summary.skipped, 1);
        assert_eq!(summary.refused, 1);
        assert_eq!(summary.errors, 0);
        let render = summary.render();
        assert!(
            render.starts_with("minted 1, skipped 1 (dedup), refused 1, errors 0\n"),
            "header line shape: {render}",
        );
        assert!(
            render.contains(&fp_mint) && render.contains("lm-newfix.1"),
            "minted line must name fingerprint + new bead id: {render}",
        );
        assert!(
            render.contains(&fp_skip) && render.contains("lm-existing.2"),
            "skip line must name fingerprint + existing bead id: {render}",
        );
        assert!(
            render.contains(&fp_refuse)
                && render.contains("lm-dup.1")
                && render.contains("lm-dup.2"),
            "refuse line must name fingerprint + conflicting ids: {render}",
        );
    }

    /// Spec contract `specs/gate.md` § *Findings and Minting* (criterion
    /// `mint_dry_run_makes_no_bd_writes`): with `--dry-run`, the
    /// pipeline runs every read-side query and resolves the bonding
    /// lead, but suppresses `bd create`. The outcome is `WouldMint`
    /// instead of `Minted`, and the only argv recorded by the runner
    /// are `list` calls — never `create`.
    #[tokio::test]
    async fn mint_dry_run_makes_no_bd_writes() {
        let finding = coherence_finding(vec![spec("gate")], "verifier-honesty", "evidence");
        let fp = finding.fingerprint();
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let opts = MintOptions {
            dry_run: true,
            spec_filter: None,
        };
        let summary = mint_findings_with_options(&bd, &[finding], "head-sha", &opts).await;
        assert_eq!(summary.minted, 0, "no bead minted under dry-run");
        assert_eq!(summary.would_mint, 1);
        match summary.findings.first().expect("one outcome recorded") {
            FindingOutcome::WouldMint {
                fingerprint,
                lead_spec,
            } => {
                assert_eq!(fingerprint, &fp);
                assert_eq!(lead_spec.as_str(), "gate");
            }
            other => panic!("expected WouldMint, got {other:?}"),
        }
        let calls = rendered_calls(&invocations);
        for call in &calls {
            assert!(
                !call.iter().any(|a| a == "create"),
                "dry-run MUST NOT invoke bd create: {call:?}",
            );
        }
    }

    /// Spec contract `specs/gate.md` § *Findings and Minting* (criterion
    /// `mint_spec_filter_drops_findings_routing_to_other_specs`):
    /// `--spec <X>` filters findings to those whose bonding lead
    /// resolves to `<X>`. Findings routing elsewhere are reported as
    /// `SkippedFilter` and no `bd create` fires for them. The dedup
    /// query still runs (the filter is post-lead-selection, per spec).
    #[tokio::test]
    async fn mint_spec_filter_drops_findings_routing_to_other_specs() {
        let f_kept = coherence_finding(vec![spec("gate")], "verifier-honesty", "evidence-a");
        let f_dropped = contract_finding(vec![spec("harness")], "molecule-lifecycle", "evidence-b");
        let runner = ScriptedRunner::new(vec![
            // kept: dedup empty, gate epic, bd create.
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout("lm-fix.1\n"),
            // dropped: dedup empty, harness epic exists, filter drops before create.
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-harnessepic", "harness")),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let opts = MintOptions {
            dry_run: false,
            spec_filter: Some(spec("gate")),
        };
        let summary =
            mint_findings_with_options(&bd, &[f_kept, f_dropped], "head-sha", &opts).await;
        assert_eq!(summary.minted, 1, "kept finding mints");
        assert_eq!(
            summary.skipped_filter, 1,
            "dropped finding reported as skipped-filter",
        );
        let dropped_outcome = summary
            .findings
            .iter()
            .find(|o| matches!(o, FindingOutcome::SkippedFilter { .. }))
            .expect("one SkippedFilter outcome recorded");
        match dropped_outcome {
            FindingOutcome::SkippedFilter {
                lead_spec,
                requested,
                ..
            } => {
                assert_eq!(lead_spec.as_str(), "harness");
                assert_eq!(requested.as_str(), "gate");
            }
            other => panic!("expected SkippedFilter, got {other:?}"),
        }
        let calls = rendered_calls(&invocations);
        let create_calls = calls
            .iter()
            .filter(|c| c.iter().any(|a| a == "create"))
            .count();
        assert_eq!(create_calls, 1, "exactly one bd create — the kept finding");
    }
}

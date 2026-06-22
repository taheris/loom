//! Driver-side mint pipeline.
//!
//! Consumes [`Finding`] records produced by either the LLM rubric's
//! `LOOM_FINDING:` lines or the deterministic verifier verdict normaliser
//! and processes them as a per-batch pipeline pinned in `specs/gate.md`
//! § *Per-batch processing*:
//!
//! 1. **Group findings by lead-spec.** For each well-formed finding,
//!    ensure each bonded spec has exactly one resolvable epic, then pick
//!    the first `bonds` element as the lead. Findings with the same lead
//!    group into the same per-spec candidate.
//! 2. **Partition by routing within each group.** Each lead-spec group
//!    yields at most one fix-up batch (non-clarify-bound findings) plus
//!    N single-finding clarify batches (one per clarify-bound finding,
//!    since each carries its own `## Options — …` block).
//! 3. **Dedup per finding** — each finding queries live beads by its
//!    `finding:<hash>` label. Zero proceeds; one skips that finding;
//!    more than one refuses as a structural violation.
//! 4. **Compute the optional batch receipt** via [`batch_fingerprint`]
//!    from the sorted set of contained finding hashes. The receipt is
//!    emitted as `loom:fixup:<fp>` for traceability only.
//! 5. **Mint the batch bead** — one `bd create --type=task
//!    --parent=<lead-epic> --labels=finding:<hash>,loom:fixup:<fp>,spec:<X>,...`.
//!    Spec labels are the union of `bonds` over every finding in the batch.
//!    The [`FindingRouting::Clarify`] carve-out for single-finding
//!    clarify batches tacks on `loom:clarify` and trusts the rubric to
//!    have emitted the canonical `## Options — …` block in the
//!    finding's `evidence` field. Clarify-bound findings whose evidence
//!    is malformed downgrade to `loom:blocked`.
//!
//! The end-of-run [`MintSummary::render`] surface is stdout-only — the
//! mint pipeline performs no other writes outside the dedup + mint flow.

mod error;
pub mod walk;

pub use error::MintError;
pub use walk::{
    MintScope, MintWalker, ProductionMintWalker, VerifierFailure, VerifierFailureKind, WalkError,
    walk,
};

use std::collections::{BTreeMap, HashMap, HashSet};

use loom_driver::bd::{BdClient, Bead, CommandRunner, CreateOpts, ListOpts, UpdateOpts};
use loom_driver::config::SuppressionConfig;
use loom_driver::identifier::{BeadId, MoleculeId, SpecLabel};
use loom_gate::IntegrityFinding;
use loom_protocol::gate::options::has_well_formed_block;
use serde::Serialize;

use crate::gate_clarify::CLARIFY_WITHOUT_OPTIONS_CAUSE;
use crate::resolve::{ResolveError, resolve_open_epic, resolve_or_mint_open_epic};
use crate::review::{ConcernToken, Finding, FindingRoute, FindingTarget};
use crate::suppression::{has_ineffective_suppression_match, suppresses_rubric_finding};

/// How a batch routes to a label class. Single-finding clarify batches
/// whose evidence is well-formed mint with `loom:clarify`; the same
/// token with malformed evidence downgrades to `loom:blocked` carrying
/// [`CLARIFY_WITHOUT_OPTIONS_CAUSE`] so a downstream `loom inbox` consumer
/// is not handed an empty options block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FindingRouting {
    /// Plain fix-up batch — no clarify or blocked label beyond `loom:fixup:<fp>`.
    Fixup,
    /// Single-finding clarify batch with a well-formed options block in evidence.
    Clarify,
    /// Single-finding clarify batch whose evidence omits or malforms the
    /// canonical options block; mints `loom:blocked` instead.
    BlockedClarifyWithoutOptions,
}

/// Classify a single finding for routing — fix-up vs clarify vs
/// blocked-clarify.
fn classify_routing(finding: &Finding) -> FindingRouting {
    if finding.route != FindingRoute::Clarify {
        return FindingRouting::Fixup;
    }
    if has_well_formed_block(&finding.evidence) {
        FindingRouting::Clarify
    } else {
        FindingRouting::BlockedClarifyWithoutOptions
    }
}

/// Bd label prefix for the optional batch receipt.
pub const MINT_LABEL_PREFIX: &str = "loom:fixup:";

/// Bd label prefix for the per-finding dedup key.
pub const FINDING_LABEL_PREFIX: &str = "finding:";

const DEFERRED_LABEL: &str = "loom:deferred";

/// Live bd statuses that count as a per-finding dedup hit.
const DEDUP_STATUSES: &str = "open,in_progress,blocked,deferred";

/// Construct the bd label that carries a batch receipt, e.g.
/// `loom:fixup:0123456789ab`.
#[must_use]
pub fn mint_label(fingerprint: &str) -> String {
    format!("{MINT_LABEL_PREFIX}{fingerprint}")
}

/// Construct the bd label that carries a finding hash.
#[must_use]
pub fn finding_label(finding: &Finding) -> String {
    format!("{FINDING_LABEL_PREFIX}{}", finding.hash())
}

/// Compute the optional batch receipt from a batch's finding hashes.
#[must_use]
pub fn batch_fingerprint(findings: &[Finding]) -> String {
    let mut hashes: Vec<String> = findings.iter().map(Finding::hash).collect();
    hashes.sort();
    let mut input = String::new();
    for (i, hash) in hashes.iter().enumerate() {
        if i > 0 {
            input.push('\u{001E}');
        }
        input.push_str(hash);
    }
    let hash = blake3::hash(input.as_bytes());
    let hex = hash.to_hex();
    hex.as_str()[..BATCH_FINGERPRINT_HEX_LEN].to_owned()
}

const BATCH_FINGERPRINT_HEX_LEN: usize = 12;
const BD_TITLE_MAX_BYTES: usize = 500;
const TRUNCATED_TITLE_SUFFIX: &str = "...";

fn mint_error_message(err: &MintError) -> String {
    let mut rendered = err.to_string();
    let mut source = std::error::Error::source(err);
    while let Some(err) = source {
        rendered.push_str(": ");
        rendered.push_str(&err.to_string());
        source = err.source();
    }
    rendered
}

/// One processed batch's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchOutcome {
    /// A fix-up batch bead was minted; `bead_id` is the new bd id,
    /// `lead_spec` names the bonding lead that supplied the parent epic,
    /// `findings_count` is the number of findings the batch carries.
    Minted {
        fingerprint: String,
        bead_id: BeadId,
        lead_spec: SpecLabel,
        findings_count: usize,
    },
    /// `--dry-run` mode: the pipeline resolved the bonding lead and
    /// would have created a fix-up batch, but did not invoke `bd create`.
    WouldMint {
        fingerprint: String,
        lead_spec: SpecLabel,
        findings_count: usize,
    },
    /// A live bead already tracks this finding hash; nothing minted.
    /// `existing_bead` is the dedup query's single hit.
    SkippedDedup {
        fingerprint: String,
        existing_bead: BeadId,
        findings_count: usize,
    },
    /// `--spec <X>` filter dropped this batch because its bonding lead
    /// resolved to a different spec.
    SkippedFilter {
        fingerprint: String,
        lead_spec: SpecLabel,
        requested: SpecLabel,
        findings_count: usize,
    },
    /// Structural violation — either multiple live beads share the
    /// finding label, or the lead spec has more than one open epic.
    /// `reason` carries the conflicting ids so the operator can resolve
    /// before re-running.
    Refused { fingerprint: String, reason: String },
    /// A molecule-local deferred bead was promoted to ready work.
    PromotedDeferred {
        bead_id: BeadId,
        findings_count: usize,
    },
    /// `--dry-run` mode: a deferred bead would have been promoted.
    WouldPromoteDeferred {
        bead_id: BeadId,
        findings_count: usize,
    },
    /// A closed bead in the same molecule already processed this finding.
    SkippedClosed {
        fingerprint: String,
        existing_bead: BeadId,
        findings_count: usize,
    },
    /// A live remediation bead has no finding labels in the current tree
    /// finding set.
    StaleCandidate {
        bead_id: BeadId,
        absent_hashes: Vec<String>,
    },
    /// A live remediation bead carries both current and absent finding labels.
    PartialStaleCandidate {
        bead_id: BeadId,
        current_hashes: Vec<String>,
        absent_hashes: Vec<String>,
    },
    /// Unexpected failure (bd CLI failure, parse failure, …) — the
    /// batch could not be processed but the run continued.
    Errored {
        fingerprint: String,
        message: String,
    },
}

impl BatchOutcome {
    /// Stable kebab-case wire name for log/summary surfaces.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Minted { .. } => "minted",
            Self::WouldMint { .. } => "would-mint",
            Self::SkippedDedup { .. } => "skipped-dedup",
            Self::SkippedFilter { .. } => "skipped-filter",
            Self::Refused { .. } => "refused",
            Self::PromotedDeferred { .. } => "promoted-deferred",
            Self::WouldPromoteDeferred { .. } => "would-promote-deferred",
            Self::SkippedClosed { .. } => "skipped-closed",
            Self::StaleCandidate { .. } => "stale-candidate",
            Self::PartialStaleCandidate { .. } => "partial-stale-candidate",
            Self::Errored { .. } => "errored",
        }
    }
}

const LOOM_FINDING_STATUS_PREFIX: &str = "LOOM_FINDING_STATUS:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FindingStatusAction {
    Reported,
    Minted,
    SkippedLive,
    Suppressed,
    StaleCandidate,
    PartialStaleCandidate,
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FindingStatusRecord {
    pub id: String,
    pub hash: String,
    pub label: String,
    pub token: ConcernToken,
    pub target: FindingTarget,
    pub action: FindingStatusAction,
}

impl FindingStatusRecord {
    #[must_use]
    pub fn new(finding: &Finding, action: FindingStatusAction) -> Self {
        Self {
            id: finding.id(),
            hash: finding.hash(),
            label: finding_label(finding),
            token: finding.token,
            target: finding.target.clone(),
            action,
        }
    }

    pub fn render(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self).map(|json| format!("{LOOM_FINDING_STATUS_PREFIX} {json}"))
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
    /// [`BatchOutcome::WouldMint`] instead of [`BatchOutcome::Minted`].
    pub dry_run: bool,
    /// When `Some(label)`, batches whose bonding lead resolves to a
    /// different spec are reported as [`BatchOutcome::SkippedFilter`]
    /// and no fix-up is minted. `None` admits every batch.
    pub spec_filter: Option<SpecLabel>,
    /// Top-level `[[suppress]]` entries from `loom.toml`. Matching
    /// rubric-origin findings are reported and removed before dedup.
    pub suppressions: Vec<SuppressionConfig>,
    /// Treat closed beads under the resolved molecule epic as already
    /// processed findings instead of automatically reminting them.
    pub suppress_closed_same_molecule: bool,
    /// Report live remediation beads whose finding labels are absent or
    /// partially absent from the current tree-scope finding set.
    pub report_stale: bool,
}

/// End-of-run summary printed to stdout (no bd writes).
///
/// Tallies match `specs/gate.md` § *Per-batch processing* end-of-run
/// shape: `minted M batches (F findings across S specs), skipped K
/// (dedup), suppressed S, ineffective suppressions I, refused R, errors E`.
/// The `--dry-run` and `--spec` filter
/// pseudo-outcomes carry their own tallies so summaries from those
/// modes remain self-describing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MintSummary {
    pub batches: Vec<BatchOutcome>,
    pub statuses: Vec<FindingStatusRecord>,
    pub minted: usize,
    pub would_mint: usize,
    pub promoted_deferred: usize,
    pub would_promote_deferred: usize,
    pub skipped: usize,
    pub skipped_filter: usize,
    pub suppressed: usize,
    pub ineffective_suppressions: usize,
    pub stale_candidates: usize,
    pub partial_stale_candidates: usize,
    pub refused: usize,
    pub errors: usize,
    pub findings_across_minted: usize,
    pub specs_across_minted: usize,
}

impl MintSummary {
    fn record_status(&mut self, finding: &Finding, action: FindingStatusAction) {
        match action {
            FindingStatusAction::Suppressed => self.suppressed += 1,
            FindingStatusAction::Reported
            | FindingStatusAction::Minted
            | FindingStatusAction::SkippedLive
            | FindingStatusAction::StaleCandidate
            | FindingStatusAction::PartialStaleCandidate
            | FindingStatusAction::Refused => {}
        }
        self.statuses
            .push(FindingStatusRecord::new(finding, action));
    }

    fn record(&mut self, outcome: BatchOutcome) {
        match &outcome {
            BatchOutcome::Minted { .. } => self.minted += 1,
            BatchOutcome::WouldMint { .. } => self.would_mint += 1,
            BatchOutcome::PromotedDeferred { .. } => self.promoted_deferred += 1,
            BatchOutcome::WouldPromoteDeferred { .. } => self.would_promote_deferred += 1,
            BatchOutcome::SkippedDedup { .. } => self.skipped += 1,
            BatchOutcome::SkippedFilter { .. } => self.skipped_filter += 1,
            BatchOutcome::Refused { .. } => self.refused += 1,
            BatchOutcome::Errored { .. } => self.errors += 1,
            BatchOutcome::StaleCandidate { .. } => self.stale_candidates += 1,
            BatchOutcome::PartialStaleCandidate { .. } => self.partial_stale_candidates += 1,
            BatchOutcome::SkippedClosed { .. } => self.skipped += 1,
        }
        self.batches.push(outcome);
    }

    /// Render the summary in the stdout shape spec'd for end-of-run.
    /// One-line header followed by per-batch lines naming the
    /// fingerprint and resulting bead id.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!(
            "minted {} batches ({} findings across {} specs), promoted {} deferred, skipped {} (dedup), suppressed {}, ineffective suppressions {}, refused {}, errors {}",
            self.minted,
            self.findings_across_minted,
            self.specs_across_minted,
            self.promoted_deferred,
            self.skipped,
            self.suppressed,
            self.ineffective_suppressions,
            self.refused,
            self.errors,
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
        if self.would_promote_deferred > 0 {
            out.push_str(&format!(
                ", would-promote {} deferred (dry-run)",
                self.would_promote_deferred
            ));
        }
        if self.stale_candidates > 0 {
            out.push_str(&format!(", stale candidates {}", self.stale_candidates));
        }
        if self.partial_stale_candidates > 0 {
            out.push_str(&format!(
                ", partial-stale candidates {}",
                self.partial_stale_candidates
            ));
        }
        out.push('\n');
        for status in &self.statuses {
            match status.render() {
                Ok(line) => {
                    out.push_str(&line);
                    out.push('\n');
                }
                Err(err) => {
                    out.push_str(&format!("  status serialization error: {err}\n"));
                }
            }
        }
        for outcome in &self.batches {
            match outcome {
                BatchOutcome::Minted {
                    fingerprint,
                    bead_id,
                    lead_spec,
                    findings_count,
                } => {
                    out.push_str(&format!(
                        "  minted {fingerprint} → {bead_id} (spec:{lead_spec}, {findings_count} findings)\n",
                    ));
                }
                BatchOutcome::WouldMint {
                    fingerprint,
                    lead_spec,
                    findings_count,
                } => {
                    out.push_str(&format!(
                        "  would-mint {fingerprint} (spec:{lead_spec}, {findings_count} findings)\n",
                    ));
                }
                BatchOutcome::SkippedDedup {
                    fingerprint,
                    existing_bead,
                    findings_count,
                } => {
                    out.push_str(&format!(
                        "  skipped {fingerprint} (existing {existing_bead}, {findings_count} findings)\n",
                    ));
                }
                BatchOutcome::SkippedFilter {
                    fingerprint,
                    lead_spec,
                    requested,
                    findings_count,
                } => {
                    out.push_str(&format!(
                        "  skipped {fingerprint} (lead spec:{lead_spec} ≠ requested spec:{requested}, {findings_count} findings)\n",
                    ));
                }
                BatchOutcome::Refused {
                    fingerprint,
                    reason,
                } => {
                    out.push_str(&format!("  refused {fingerprint}: {reason}\n"));
                }
                BatchOutcome::PromotedDeferred {
                    bead_id,
                    findings_count,
                } => {
                    out.push_str(&format!(
                        "  promoted deferred {bead_id} ({findings_count} findings)\n",
                    ));
                }
                BatchOutcome::WouldPromoteDeferred {
                    bead_id,
                    findings_count,
                } => {
                    out.push_str(&format!(
                        "  would-promote deferred {bead_id} ({findings_count} findings)\n",
                    ));
                }
                BatchOutcome::Errored {
                    fingerprint,
                    message,
                } => {
                    out.push_str(&format!("  error {fingerprint}: {message}\n"));
                }
                BatchOutcome::SkippedClosed {
                    fingerprint,
                    existing_bead,
                    findings_count,
                } => {
                    out.push_str(&format!(
                        "  skipped {fingerprint} (closed same-molecule {existing_bead}, {findings_count} findings)\n",
                    ));
                }
                BatchOutcome::StaleCandidate {
                    bead_id,
                    absent_hashes,
                } => {
                    out.push_str(&format!(
                        "  stale-candidate {bead_id} (absent findings: {})\n",
                        absent_hashes.join(", ")
                    ));
                }
                BatchOutcome::PartialStaleCandidate {
                    bead_id,
                    current_hashes,
                    absent_hashes,
                } => {
                    out.push_str(&format!(
                        "  partial-stale-candidate {bead_id} (current: {}; absent: {})\n",
                        current_hashes.join(", "),
                        absent_hashes.join(", ")
                    ));
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

/// Dispatch a molecule's push-gate-terminal integrity findings through
/// the standard mint pipeline, per `specs/gate.md` § *Integrity gate*
/// (recovery branch). Each finding is normalized into a typed [`Finding`]
/// via [`IntegrityFinding::to_finding`] (non-terminal variants drop out)
/// and the batch is minted against `head_commit`. The review push-gate
/// reaches mint only through this seam so the `mint_findings_with_options`
/// call stays inside the mint module.
pub async fn mint_integrity_recovery<R: CommandRunner>(
    bd: &BdClient<R>,
    findings: &[IntegrityFinding],
    head_commit: &str,
) -> MintSummary {
    let typed: Vec<Finding> = findings
        .iter()
        .filter_map(IntegrityFinding::to_finding)
        .collect();
    mint_findings_with_options(bd, &typed, head_commit, &MintOptions::default()).await
}

/// Promote one molecule's deferred remediation beads to ready work.
pub async fn promote_deferred<R: CommandRunner>(
    bd: &BdClient<R>,
    molecule: &MoleculeId,
    dry_run: bool,
) -> MintSummary {
    let mut summary = MintSummary::default();
    let molecule_bead = match BeadId::new(molecule.as_str()) {
        Ok(id) => id,
        Err(err) => {
            summary.record(BatchOutcome::Errored {
                fingerprint: molecule.to_string(),
                message: format!("molecule id `{molecule}` is not a bead id: {err}"),
            });
            return summary;
        }
    };
    if let Err(err) = bd.show(&molecule_bead).await {
        summary.record(BatchOutcome::Errored {
            fingerprint: molecule.to_string(),
            message: format!("missing molecule epic `{molecule}` or bd read failed: {err}"),
        });
        return summary;
    }
    let children = match bd
        .list(ListOpts {
            parent: Some(molecule_bead),
            ..ListOpts::default()
        })
        .await
    {
        Ok(children) => children,
        Err(err) => {
            summary.record(BatchOutcome::Errored {
                fingerprint: molecule.to_string(),
                message: format!("bd list failed for molecule `{molecule}`: {err}"),
            });
            return summary;
        }
    };
    if let Some(reason) = duplicate_live_finding_reason(&children) {
        summary.record(BatchOutcome::Refused {
            fingerprint: molecule.to_string(),
            reason,
        });
        return summary;
    }
    for bead in children.into_iter().filter(is_deferred_remediation) {
        let findings_count = finding_label_count(&bead);
        if dry_run {
            summary.record(BatchOutcome::WouldPromoteDeferred {
                bead_id: bead.id,
                findings_count,
            });
            continue;
        }
        match bd
            .update(
                &bead.id,
                UpdateOpts {
                    status: Some("open".to_string()),
                    remove_labels: vec![DEFERRED_LABEL.to_string()],
                    description: Some(bead.description.clone()),
                    ..UpdateOpts::default()
                },
            )
            .await
        {
            Ok(()) => summary.record(BatchOutcome::PromotedDeferred {
                bead_id: bead.id,
                findings_count,
            }),
            Err(err) => summary.record(BatchOutcome::Errored {
                fingerprint: bead.id.to_string(),
                message: format!("bd update failed while promoting deferred bead: {err}"),
            }),
        }
    }
    summary
}

fn duplicate_live_finding_reason(children: &[Bead]) -> Option<String> {
    let mut seen: HashMap<&str, &BeadId> = HashMap::new();
    for bead in children.iter().filter(|bead| bead.status != "closed") {
        for label in bead
            .labels
            .iter()
            .filter_map(|label| label.as_str().strip_prefix(FINDING_LABEL_PREFIX))
        {
            if let Some(existing) = seen.insert(label, &bead.id) {
                return Some(format!(
                    "duplicate live finding hash `{label}` on beads `{existing}` and `{}`",
                    bead.id,
                ));
            }
        }
    }
    None
}

fn is_deferred_remediation(bead: &Bead) -> bool {
    bead.status == "deferred"
        && bead
            .labels
            .iter()
            .any(|label| label.as_str() == DEFERRED_LABEL)
}

fn finding_label_count(bead: &Bead) -> usize {
    bead.labels
        .iter()
        .filter(|label| label.as_str().starts_with(FINDING_LABEL_PREFIX))
        .count()
}

/// Walk a sequence of findings through the per-batch mint pipeline.
///
/// Findings are first deduped by their live `finding:<hash>` labels,
/// then grouped by resolved lead-spec (first bond with an open epic,
/// else `bonds[0]`); each lead-spec group becomes at most one fix-up
/// batch plus N single-finding clarify batches. Multi-open structural
/// violations (dedup query returning >1 hit or
/// [`ResolveError::InvariantViolation`]) become per-batch
/// [`BatchOutcome::Refused`] so the run keeps going; other
/// [`MintError`] variants become [`BatchOutcome::Errored`].
pub async fn mint_findings_with_options<R: CommandRunner>(
    bd: &BdClient<R>,
    findings: &[Finding],
    head_commit: &str,
    opts: &MintOptions,
) -> MintSummary {
    let mut summary = MintSummary::default();
    let mut resolver = LeadResolver::new(bd, head_commit);

    let mut fingerprints = Vec::with_capacity(findings.len());
    let mut ids_by_hash: HashMap<String, String> = HashMap::new();
    for finding in findings {
        let finding_id = finding.id();
        let finding_hash = finding.hash();
        if let Some(existing_id) = ids_by_hash.get(&finding_hash) {
            if existing_id != &finding_id {
                summary.record_status(finding, FindingStatusAction::Refused);
                summary.record(BatchOutcome::Refused {
                    fingerprint: finding_hash,
                    reason: format!(
                        "finding hash collision: `{existing_id}` and `{finding_id}` share one hash",
                    ),
                });
                return summary;
            }
        } else {
            ids_by_hash.insert(finding_hash.clone(), finding_id);
        }
        fingerprints.push((finding, finding_hash));
    }

    let mut survivors: Vec<(Finding, SpecLabel, Option<MoleculeId>)> = Vec::new();
    let mut current_hashes: HashSet<String> = HashSet::new();
    for (finding, finding_hash) in fingerprints {
        if suppresses_rubric_finding(&opts.suppressions, finding) {
            summary.record_status(finding, FindingStatusAction::Suppressed);
            continue;
        }
        current_hashes.insert(finding_hash.clone());
        if has_ineffective_suppression_match(&opts.suppressions, finding) {
            summary.ineffective_suppressions += 1;
        }
        match dedup_live_finding(bd, finding).await {
            FindingDedup::Untracked => {}
            FindingDedup::Tracked(existing_bead) => {
                summary.record_status(finding, FindingStatusAction::SkippedLive);
                summary.record(BatchOutcome::SkippedDedup {
                    fingerprint: finding_hash,
                    existing_bead,
                    findings_count: 1,
                });
                continue;
            }
            FindingDedup::Duplicate { reason } => {
                summary.record_status(finding, FindingStatusAction::Refused);
                summary.record(BatchOutcome::Refused {
                    fingerprint: finding_hash,
                    reason,
                });
                continue;
            }
            FindingDedup::Errored { message } => {
                summary.record_status(finding, FindingStatusAction::Refused);
                summary.record(BatchOutcome::Errored {
                    fingerprint: finding_hash,
                    message,
                });
                continue;
            }
            FindingDedup::Closed { .. } => {}
        }
        let (lead_spec, lead_epic) = match resolver.resolve(&finding.bonds, opts.dry_run).await {
            Ok(lead) => lead,
            Err(MintError::Resolve(ResolveError::InvariantViolation { label, ids })) => {
                let fingerprint = batch_fingerprint(std::slice::from_ref(finding));
                summary.record_status(finding, FindingStatusAction::Refused);
                summary.record(BatchOutcome::Refused {
                    fingerprint,
                    reason: format!(
                        "more than one open epic for spec `{label}` — close all but one before re-running (ids: {ids})",
                    ),
                });
                continue;
            }
            Err(err) => {
                let fingerprint = batch_fingerprint(std::slice::from_ref(finding));
                summary.record_status(finding, FindingStatusAction::Refused);
                summary.record(BatchOutcome::Errored {
                    fingerprint,
                    message: mint_error_message(&err),
                });
                continue;
            }
        };
        if let Some(requested) = &opts.spec_filter
            && &lead_spec != requested
        {
            let fingerprint = batch_fingerprint(std::slice::from_ref(finding));
            summary.record_status(finding, FindingStatusAction::Reported);
            summary.record(BatchOutcome::SkippedFilter {
                fingerprint,
                lead_spec,
                requested: requested.clone(),
                findings_count: 1,
            });
            continue;
        }
        if opts.suppress_closed_same_molecule
            && let Some(epic) = lead_epic.as_ref()
        {
            match dedup_closed_same_molecule(bd, finding, epic).await {
                FindingDedup::Untracked => {}
                FindingDedup::Closed(existing_bead) => {
                    summary.record_status(finding, FindingStatusAction::Reported);
                    summary.record(BatchOutcome::SkippedClosed {
                        fingerprint: finding_hash,
                        existing_bead,
                        findings_count: 1,
                    });
                    continue;
                }
                FindingDedup::Duplicate { reason } => {
                    summary.record_status(finding, FindingStatusAction::Refused);
                    summary.record(BatchOutcome::Refused {
                        fingerprint: finding_hash,
                        reason,
                    });
                    continue;
                }
                FindingDedup::Errored { message } => {
                    summary.record_status(finding, FindingStatusAction::Refused);
                    summary.record(BatchOutcome::Errored {
                        fingerprint: finding_hash,
                        message,
                    });
                    continue;
                }
                FindingDedup::Tracked(_) => {}
            }
        }
        survivors.push((finding.clone(), lead_spec, lead_epic));
    }

    let by_spec = group_survivors_by_lead_spec(survivors);

    let mut minted_specs: HashSet<String> = HashSet::new();
    for (lead_spec, lead_epic, group) in by_spec {
        let (fix_up, clarifies) = partition_group(group);
        if !fix_up.is_empty() {
            let outcome = process_batch(
                bd,
                &fix_up,
                &lead_spec,
                lead_epic.as_ref(),
                FindingRouting::Fixup,
                opts,
            )
            .await;
            record_batch_status(&mut summary, &fix_up, &outcome);
            track_minted(&outcome, &lead_spec, &mut summary, &mut minted_specs);
            summary.record(outcome);
        }
        for (finding, routing) in clarifies {
            let outcome = process_batch(
                bd,
                std::slice::from_ref(&finding),
                &lead_spec,
                lead_epic.as_ref(),
                routing,
                opts,
            )
            .await;
            record_batch_status(&mut summary, std::slice::from_ref(&finding), &outcome);
            track_minted(&outcome, &lead_spec, &mut summary, &mut minted_specs);
            summary.record(outcome);
        }
    }
    summary.specs_across_minted = minted_specs.len();
    if opts.report_stale {
        report_stale_candidates(bd, &mut summary, &current_hashes, opts.spec_filter.as_ref()).await;
    }
    summary
}

/// Group mint survivors by lead spec with stable alphabetical output order.
fn group_survivors_by_lead_spec(
    survivors: Vec<(Finding, SpecLabel, Option<MoleculeId>)>,
) -> Vec<(SpecLabel, Option<MoleculeId>, Vec<Finding>)> {
    let mut by_spec: Vec<(SpecLabel, Option<MoleculeId>, Vec<Finding>)> = Vec::new();
    for (finding, lead_spec, lead_epic) in survivors {
        if let Some(slot) = by_spec.iter_mut().find(|(s, _, _)| *s == lead_spec) {
            slot.2.push(finding);
        } else {
            by_spec.push((lead_spec, lead_epic, vec![finding]));
        }
    }
    by_spec.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    by_spec
}

async fn report_stale_candidates<R: CommandRunner>(
    bd: &BdClient<R>,
    summary: &mut MintSummary,
    current_hashes: &HashSet<String>,
    spec_filter: Option<&SpecLabel>,
) {
    let beads = match bd
        .list(ListOpts {
            status: Some(DEDUP_STATUSES.to_string()),
            label: spec_filter.map(|spec| format!("spec:{spec}")),
            ..ListOpts::default()
        })
        .await
    {
        Ok(beads) => beads,
        Err(err) => {
            let fingerprint =
                spec_filter.map_or_else(|| "stale:tree".to_owned(), |spec| format!("stale:{spec}"));
            let err = MintError::from(err);
            summary.record(BatchOutcome::Errored {
                fingerprint,
                message: mint_error_message(&err),
            });
            return;
        }
    };
    for bead in beads {
        let labels = finding_hash_labels(&bead);
        if labels.is_empty() {
            continue;
        }
        let (current, absent): (Vec<String>, Vec<String>) = labels
            .into_iter()
            .partition(|hash| current_hashes.contains(hash));
        if current.is_empty() {
            summary.record(BatchOutcome::StaleCandidate {
                bead_id: bead.id,
                absent_hashes: absent,
            });
        } else if !absent.is_empty() {
            summary.record(BatchOutcome::PartialStaleCandidate {
                bead_id: bead.id,
                current_hashes: current,
                absent_hashes: absent,
            });
        }
    }
}

fn finding_hash_labels(bead: &Bead) -> Vec<String> {
    let mut hashes = bead
        .labels
        .iter()
        .filter_map(|label| label.as_str().strip_prefix(FINDING_LABEL_PREFIX))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    hashes.sort();
    hashes
}

fn track_minted(
    outcome: &BatchOutcome,
    lead_spec: &SpecLabel,
    summary: &mut MintSummary,
    minted_specs: &mut HashSet<String>,
) {
    if let BatchOutcome::Minted { findings_count, .. } = outcome {
        summary.findings_across_minted += findings_count;
        minted_specs.insert(lead_spec.as_str().to_owned());
    }
}

fn record_batch_status(summary: &mut MintSummary, findings: &[Finding], outcome: &BatchOutcome) {
    let action = match outcome {
        BatchOutcome::Minted { .. } => FindingStatusAction::Minted,
        BatchOutcome::WouldMint { .. } | BatchOutcome::SkippedFilter { .. } => {
            FindingStatusAction::Reported
        }
        BatchOutcome::Refused { .. } | BatchOutcome::Errored { .. } => FindingStatusAction::Refused,
        BatchOutcome::SkippedDedup { .. } => FindingStatusAction::SkippedLive,
        BatchOutcome::SkippedClosed { .. }
        | BatchOutcome::PromotedDeferred { .. }
        | BatchOutcome::WouldPromoteDeferred { .. }
        | BatchOutcome::StaleCandidate { .. }
        | BatchOutcome::PartialStaleCandidate { .. } => FindingStatusAction::Reported,
    };
    for finding in findings {
        summary.record_status(finding, action);
    }
}

/// Partition one lead-spec group into a single fix-up batch (non-
/// clarify-bound findings) plus N single-finding clarify batches.
fn partition_group(group: Vec<Finding>) -> (Vec<Finding>, Vec<(Finding, FindingRouting)>) {
    let mut fix_up = Vec::new();
    let mut clarifies = Vec::new();
    for finding in group {
        let routing = classify_routing(&finding);
        match routing {
            FindingRouting::Fixup => fix_up.push(finding),
            FindingRouting::Clarify | FindingRouting::BlockedClarifyWithoutOptions => {
                clarifies.push((finding, routing));
            }
        }
    }
    (fix_up, clarifies)
}

enum FindingDedup {
    Untracked,
    Tracked(BeadId),
    Closed(BeadId),
    Duplicate { reason: String },
    Errored { message: String },
}

async fn dedup_live_finding<R: CommandRunner>(bd: &BdClient<R>, finding: &Finding) -> FindingDedup {
    let label = finding_label(finding);
    let matching_beads = match bd
        .list(ListOpts {
            status: Some(DEDUP_STATUSES.to_string()),
            label: Some(label),
            ..ListOpts::default()
        })
        .await
    {
        Ok(beads) => beads,
        Err(err) => {
            let err = MintError::from(err);
            return FindingDedup::Errored {
                message: mint_error_message(&err),
            };
        }
    };
    match matching_beads.len() {
        0 => FindingDedup::Untracked,
        1 => {
            let bead = &matching_beads[0];
            if let Some(conflicting_id) = live_bead_conflicting_finding_id(bead, finding) {
                return FindingDedup::Duplicate {
                    reason: format!(
                        "live bead `{}` carries `{}` for different finding id `{}` (current id `{}`) — refuse hash collision",
                        bead.id,
                        finding_label(finding),
                        conflicting_id,
                        finding.id(),
                    ),
                };
            }
            FindingDedup::Tracked(bead.id.clone())
        }
        n => {
            let ids = matching_beads
                .iter()
                .map(|b| b.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            FindingDedup::Duplicate {
                reason: format!(
                    "{n} live beads share finding label — remove duplicate labels or close duplicates before re-running (ids: {ids})",
                ),
            }
        }
    }
}

async fn dedup_closed_same_molecule<R: CommandRunner>(
    bd: &BdClient<R>,
    finding: &Finding,
    molecule: &MoleculeId,
) -> FindingDedup {
    let parent = match BeadId::new(molecule.as_str()) {
        Ok(id) => id,
        Err(source) => {
            return FindingDedup::Errored {
                message: format!("molecule id `{molecule}` is not a bead id: {source}"),
            };
        }
    };
    let label = finding_label(finding);
    let matching_beads = match bd
        .list(ListOpts {
            status: Some("closed".to_string()),
            label: Some(label),
            parent: Some(parent),
            ..ListOpts::default()
        })
        .await
    {
        Ok(beads) => beads,
        Err(err) => {
            let err = MintError::from(err);
            return FindingDedup::Errored {
                message: mint_error_message(&err),
            };
        }
    };
    match matching_beads.len() {
        0 => FindingDedup::Untracked,
        1 => FindingDedup::Closed(matching_beads[0].id.clone()),
        n => {
            let ids = matching_beads
                .iter()
                .map(|b| b.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            FindingDedup::Duplicate {
                reason: format!(
                    "{n} closed beads in the same molecule share finding label (ids: {ids})",
                ),
            }
        }
    }
}

fn live_bead_conflicting_finding_id(bead: &Bead, finding: &Finding) -> Option<String> {
    let current_hash = finding.hash();
    let current_id = finding.id();
    finding_identity_pairs(&bead.description)
        .into_iter()
        .find_map(|(id, hash)| (hash == current_hash && id != current_id).then_some(id))
}

fn finding_identity_pairs(description: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut pending_id: Option<String> = None;
    for line in description.lines().map(str::trim) {
        if let Some(id) = markdown_code_value(line, "id: ") {
            pending_id = Some(id);
            continue;
        }
        if let Some(hash) = markdown_code_value(line, "hash: ")
            && let Some(id) = pending_id.take()
        {
            pairs.push((id, hash));
        }
    }
    pairs
}

fn markdown_code_value(line: &str, prefix: &str) -> Option<String> {
    line.strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('`'))
        .and_then(|rest| rest.split_once('`'))
        .map(|(value, _)| value.to_owned())
}

/// Mint one batch after per-finding dedup has selected its findings.
async fn process_batch<R: CommandRunner>(
    bd: &BdClient<R>,
    findings: &[Finding],
    lead_spec: &SpecLabel,
    lead_epic: Option<&MoleculeId>,
    routing: FindingRouting,
    opts: &MintOptions,
) -> BatchOutcome {
    let fingerprint = batch_fingerprint(findings);
    let label = mint_label(&fingerprint);

    if opts.dry_run {
        return BatchOutcome::WouldMint {
            fingerprint,
            lead_spec: lead_spec.clone(),
            findings_count: findings.len(),
        };
    }

    let labels = batch_labels(findings, &label, routing);
    let title = batch_title(findings, lead_spec);
    let description = batch_description(findings, &fingerprint, routing);
    let Some(lead_epic) = lead_epic else {
        return BatchOutcome::Errored {
            fingerprint,
            message: "lead epic missing outside dry-run".to_owned(),
        };
    };
    let parent = match BeadId::new(lead_epic.as_str()) {
        Ok(p) => p,
        Err(source) => {
            let err = MintError::InvalidParentId {
                molecule: lead_epic.to_string(),
                source,
            };
            return BatchOutcome::Errored {
                fingerprint,
                message: mint_error_message(&err),
            };
        }
    };
    match bd
        .create(CreateOpts {
            title,
            description,
            issue_type: Some("task".to_string()),
            labels,
            parent: Some(parent),
            ..CreateOpts::default()
        })
        .await
    {
        Ok(bead_id) => BatchOutcome::Minted {
            fingerprint,
            bead_id,
            lead_spec: lead_spec.clone(),
            findings_count: findings.len(),
        },
        Err(err) => {
            let err = MintError::from(err);
            BatchOutcome::Errored {
                fingerprint,
                message: mint_error_message(&err),
            }
        }
    }
}

/// Caches bonded-spec resolution so two findings naming the same missing
/// spec do not each mint a fresh molecule.
struct LeadResolver<'a, R: CommandRunner> {
    bd: &'a BdClient<R>,
    head_commit: &'a str,
    cache: HashMap<String, (SpecLabel, MoleculeId)>,
    explored: HashSet<String>,
}

impl<'a, R: CommandRunner> LeadResolver<'a, R> {
    fn new(bd: &'a BdClient<R>, head_commit: &'a str) -> Self {
        Self {
            bd,
            head_commit,
            cache: HashMap::new(),
            explored: HashSet::new(),
        }
    }

    async fn resolve(
        &mut self,
        bonds: &[SpecLabel],
        dry_run: bool,
    ) -> Result<(SpecLabel, Option<MoleculeId>), MintError> {
        let Some(lead) = bonds.first().cloned() else {
            return Err(MintError::EmptyBonds);
        };
        let mut lead_epic = None;
        for spec in bonds {
            let resolved = self.resolve_one(spec, dry_run).await?;
            if spec == &lead {
                lead_epic = resolved;
            }
        }
        Ok((lead, lead_epic))
    }

    async fn resolve_one(
        &mut self,
        spec: &SpecLabel,
        dry_run: bool,
    ) -> Result<Option<MoleculeId>, MintError> {
        let key = spec.as_str().to_owned();
        if let Some((_, epic)) = self.cache.get(&key) {
            return Ok(Some(epic.clone()));
        }
        if self.explored.contains(&key) {
            return Ok(None);
        }
        if let Some(epic) = resolve_open_epic(self.bd, spec).await? {
            self.cache.insert(key, (spec.clone(), epic.clone()));
            return Ok(Some(epic));
        }
        self.explored.insert(key.clone());
        if dry_run {
            return Ok(None);
        }
        let resolved = resolve_or_mint_open_epic(self.bd, spec, self.head_commit).await?;
        self.cache
            .insert(key, (spec.clone(), resolved.molecule_id.clone()));
        Ok(Some(resolved.molecule_id))
    }
}

/// Compose the bd-label list for the minted batch.
fn batch_labels(findings: &[Finding], mint_label: &str, routing: FindingRouting) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut ordered: Vec<&SpecLabel> = Vec::new();
    for finding in findings {
        for spec in &finding.bonds {
            if seen.insert(spec.as_str().to_owned()) {
                ordered.push(spec);
            }
        }
    }
    let mut labels = Vec::with_capacity(ordered.len() + findings.len() + 2);
    labels.push(mint_label.to_string());
    let mut finding_labels: Vec<String> = findings.iter().map(finding_label).collect();
    finding_labels.sort();
    labels.extend(finding_labels);
    for spec in ordered {
        labels.push(format!("spec:{spec}"));
    }
    match routing {
        FindingRouting::Fixup => {}
        FindingRouting::Clarify => labels.push("loom:clarify".to_string()),
        FindingRouting::BlockedClarifyWithoutOptions => {
            labels.push("loom:blocked".to_string());
        }
    }
    labels
}

/// Deterministic title — the same batch always mints with the same
/// title across runs, so a closed-then-reopened bead's title still
/// matches the next walk's perceived shape. Multi-finding batches keep
/// titles concise by summarising concern-token counts; full targets and
/// evidence live in the description.
fn batch_title(findings: &[Finding], lead_spec: &SpecLabel) -> String {
    let title = if findings.len() == 1 {
        let f = &findings[0];
        format!(
            "{token}: {target}",
            token = f.token.as_wire(),
            target = f.target.canonical_form(),
        )
    } else {
        format!(
            "fix-up batch: {} findings for {} ({})",
            findings.len(),
            lead_spec,
            concern_token_summary(findings),
        )
    };
    cap_bd_title(title)
}

fn concern_token_summary(findings: &[Finding]) -> String {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for finding in findings {
        *counts.entry(finding.token.as_wire()).or_default() += 1;
    }
    let mut items = counts.into_iter().collect::<Vec<_>>();
    items.sort_by(|(token_a, count_a), (token_b, count_b)| {
        count_b.cmp(count_a).then_with(|| token_a.cmp(token_b))
    });
    items
        .into_iter()
        .map(|(token, count)| {
            if count == 1 {
                token.to_owned()
            } else {
                format!("{token}×{count}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn cap_bd_title(title: String) -> String {
    if title.len() <= BD_TITLE_MAX_BYTES {
        return title;
    }
    let mut keep = BD_TITLE_MAX_BYTES - TRUNCATED_TITLE_SUFFIX.len();
    while !title.is_char_boundary(keep) {
        keep -= 1;
    }
    format!("{}{TRUNCATED_TITLE_SUFFIX}", &title[..keep])
}

/// Description enumerates every finding in the batch and pins the
/// finding labels plus batch receipt at the foot. For single-finding clarify
/// batches whose evidence carries a well-formed `## Options — …` block,
/// the block is preserved verbatim per the *Options Format Contract*;
/// for the blocked-without-options fallback the cause is prepended so
/// the operator sees why the bead minted as `loom:blocked` rather than
/// `loom:clarify`.
fn batch_description(findings: &[Finding], fingerprint: &str, routing: FindingRouting) -> String {
    let mut out = String::new();
    if matches!(routing, FindingRouting::BlockedClarifyWithoutOptions) {
        out.push_str(&format!("Cause: `{CLARIFY_WITHOUT_OPTIONS_CAUSE}`\n\n"));
    }
    if matches!(routing, FindingRouting::Clarify) {
        out.push_str(findings[0].evidence.trim_end());
        out.push_str("\n\n---\n\n");
    }
    append_findings_section(&mut out, findings);
    out.push_str("\n---\n\n");
    let mut finding_labels: Vec<String> = findings
        .iter()
        .map(|finding| format!("`{}`", finding_label(finding)))
        .collect();
    finding_labels.sort();
    out.push_str(&format!("Finding labels: {}\n", finding_labels.join(", ")));
    out.push_str(&format!(
        "Batch receipt: `{MINT_LABEL_PREFIX}{fingerprint}`\n"
    ));
    out
}

fn append_findings_section(out: &mut String, findings: &[Finding]) {
    let mut indexed: Vec<(usize, &Finding)> = findings.iter().enumerate().collect();
    indexed.sort_by(|(_, a), (_, b)| {
        let ka = (a.token.as_wire(), a.target.canonical_form());
        let kb = (b.token.as_wire(), b.target.canonical_form());
        ka.cmp(&kb)
    });
    out.push_str(&format!("Findings ({}):\n\n", findings.len()));
    for (_, finding) in indexed {
        out.push_str(&format!(
            "- **{token}** — `{target}`\n  id: `{id}`\n  hash: `{hash}`\n  evidence: {evidence}\n",
            token = finding.token.as_wire(),
            target = finding.target.canonical_form(),
            id = finding.id(),
            hash = finding.hash(),
            evidence = evidence_excerpt(&finding.evidence),
        ));
    }
}

fn evidence_excerpt(evidence: &str) -> String {
    const LIMIT: usize = 240;
    let flattened = evidence.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = flattened.chars();
    let excerpt: String = chars.by_ref().take(LIMIT).collect();
    if chars.next().is_none() {
        excerpt
    } else {
        format!("{excerpt}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::bd::{BdError, RunOutput};
    use loom_driver::config::LoomConfig;
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
            route: crate::review::FindingRoute::Deferred,
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
            route: crate::review::FindingRoute::Deferred,
            target: FindingTarget::Contract { id: id.to_owned() },
            bonds,
            evidence: evidence.to_owned(),
        }
    }

    fn style_finding(bonds: Vec<SpecLabel>, rule_id: &str, evidence: &str) -> Finding {
        Finding {
            token: ConcernToken::StyleRuleViolation,
            route: crate::review::FindingRoute::Deferred,
            target: FindingTarget::StyleRule {
                rule_id: rule_id.to_owned(),
                subject: "crates/loom-workflow/src/mint/mod.rs".to_owned(),
            },
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
            route: crate::review::FindingRoute::Clarify,
            target: FindingTarget::Invariant {
                spec,
                section: section.to_owned(),
                tag: tag.to_owned(),
            },
            bonds,
            evidence: evidence.to_owned(),
        }
    }

    fn deterministic_finding(bonds: Vec<SpecLabel>, target_string: &str) -> Finding {
        annotation_finding(
            ConcernToken::VerifierFailed,
            bonds,
            target_string,
            "deterministic verifier failed",
        )
    }

    fn verifier_bypass_finding(
        bonds: Vec<SpecLabel>,
        target_string: &str,
        evidence: &str,
    ) -> Finding {
        annotation_finding(ConcernToken::VerifierBypass, bonds, target_string, evidence)
    }

    fn annotation_finding(
        token: ConcernToken,
        bonds: Vec<SpecLabel>,
        target_string: &str,
        evidence: &str,
    ) -> Finding {
        Finding {
            token,
            route: FindingRoute::Deferred,
            target: FindingTarget::Annotation {
                target_string: target_string.to_owned(),
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

    fn flag_arg<'a>(args: &'a [String], flag: &str) -> &'a str {
        let idx = args
            .iter()
            .position(|arg| arg == flag)
            .expect("flag present");
        &args[idx + 1]
    }

    fn labels_arg(args: &[String]) -> Vec<&str> {
        flag_arg(args, "--labels").split(',').collect()
    }

    fn task_create_call(calls: &[Vec<String>]) -> &Vec<String> {
        calls
            .iter()
            .find(|call| {
                call.iter().any(|arg| arg == "create")
                    && call
                        .windows(2)
                        .any(|pair| pair[0] == "--type" && pair[1] == "task")
            })
            .expect("bd task create recorded")
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

    fn fixup_row(id: &str, hash: &str) -> String {
        fixup_row_status(id, hash, "open")
    }

    fn fixup_row_status(id: &str, hash: &str, status: &str) -> String {
        fixup_row_status_description(id, hash, status, "")
    }

    fn fixup_row_status_description(
        id: &str,
        hash: &str,
        status: &str,
        description: &str,
    ) -> String {
        fixup_row_with_labels(
            id,
            status,
            &[&format!("{FINDING_LABEL_PREFIX}{hash}")],
            description,
        )
    }

    fn fixup_row_with_labels(id: &str, status: &str, labels: &[&str], description: &str) -> String {
        let labels_json = labels
            .iter()
            .map(|label| format!("{label:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            r#"{{
                "id": "{id}",
                "title": "existing fix-up",
                "description": {description:?},
                "status": "{status}",
                "priority": 2,
                "issue_type": "task",
                "labels": [{labels_json}]
            }}"#,
        )
    }

    fn deferred_row(id: &str, hash: &str, status: &str, deferred_label: bool) -> String {
        let deferred = if deferred_label {
            format!(", \"{DEFERRED_LABEL}\"")
        } else {
            String::new()
        };
        format!(
            r#"{{
                "id": "{id}",
                "title": "deferred fix-up",
                "description": "deferred evidence",
                "status": "{status}",
                "priority": 2,
                "issue_type": "task",
                "labels": ["{FINDING_LABEL_PREFIX}{hash}"{deferred}]
            }}"#,
        )
    }

    fn err_stderr(body: &str) -> RunOutput {
        RunOutput {
            status: 1,
            stdout: Vec::new(),
            stderr: body.as_bytes().to_vec(),
        }
    }

    #[tokio::test]
    async fn promote_deferred_updates_existing_beads_without_minting_findings() {
        let runner = ScriptedRunner::new(vec![
            ok_stdout(&format!("[{}]", epic_row("lm-mol", "gate"))),
            ok_stdout(&format!(
                "[{}]",
                deferred_row("lm-mol.1", "hash-a", "deferred", true),
            )),
            ok_stdout(""),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);

        let summary = promote_deferred(&bd, &MoleculeId::new("lm-mol"), false).await;

        assert_eq!(summary.promoted_deferred, 1);
        assert_eq!(
            summary.minted, 0,
            "promotion must not create a finding batch"
        );
        assert!(summary.render().contains("promoted 1 deferred"));
        let calls = rendered_calls(&invocations);
        assert!(
            calls
                .iter()
                .any(|call| call == &["show", "lm-mol", "--json"]),
            "promotion verifies the molecule epic exists: {calls:?}",
        );
        assert!(
            calls
                .iter()
                .any(|call| call == &["list", "--json", "--parent=lm-mol"]),
            "promotion lists existing children instead of fabricating findings: {calls:?}",
        );
        assert!(
            calls.iter().any(|call| call
                == &[
                    "update",
                    "lm-mol.1",
                    "--status",
                    "open",
                    "--remove-label",
                    DEFERRED_LABEL,
                    "--description",
                    "deferred evidence",
                ]),
            "promotion opens the deferred bead and removes loom:deferred: {calls:?}",
        );
        assert!(
            calls
                .iter()
                .all(|call| call.first().is_none_or(|arg| arg != "create")),
            "promotion is a state transition, not bead creation: {calls:?}",
        );
    }

    #[tokio::test]
    async fn promote_deferred_reports_duplicate_live_findings_and_write_errors() {
        let duplicate_runner = ScriptedRunner::new(vec![
            ok_stdout(&format!("[{}]", epic_row("lm-mol", "gate"))),
            ok_stdout(&format!(
                "[{},{}]",
                deferred_row("lm-mol.1", "same", "deferred", true),
                deferred_row("lm-mol.2", "same", "open", false),
            )),
        ]);
        let duplicate_bd = BdClient::with_runner(duplicate_runner);
        let duplicate = promote_deferred(&duplicate_bd, &MoleculeId::new("lm-mol"), false).await;
        assert_eq!(duplicate.refused, 1);
        assert!(
            duplicate.render().contains("duplicate live finding hash"),
            "structural conflict is named: {}",
            duplicate.render(),
        );

        let write_runner = ScriptedRunner::new(vec![
            ok_stdout(&format!("[{}]", epic_row("lm-mol", "gate"))),
            ok_stdout(&format!(
                "[{}]",
                deferred_row("lm-mol.3", "hash-b", "deferred", true),
            )),
            err_stderr("permission denied"),
        ]);
        let write_bd = BdClient::with_runner(write_runner);
        let write = promote_deferred(&write_bd, &MoleculeId::new("lm-mol"), false).await;
        assert_eq!(write.errors, 1);
        assert!(
            write.render().contains("bd update failed"),
            "write failure is named: {}",
            write.render(),
        );
    }

    #[tokio::test]
    async fn mint_refuses_finding_hash_collision() {
        let first =
            verifier_bypass_finding(vec![spec("gate")], "collision-target-10837099", "evidence");
        let second =
            verifier_bypass_finding(vec![spec("gate")], "collision-target-21582999", "evidence");
        let first_id = first.id();
        let first_hash = first.hash();
        let second_id = second.id();
        assert_ne!(
            first_id, second_id,
            "fixture must represent distinct findings"
        );
        assert_eq!(
            first_hash,
            second.hash(),
            "fixture must be an actual Finding::hash collision",
        );
        let runner = ScriptedRunner::new(Vec::new());
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);

        let summary =
            mint_findings_with_options(&bd, &[first, second], "head-sha", &MintOptions::default())
                .await;

        assert_eq!(summary.refused, 1);
        assert!(summary.batches.iter().any(|outcome| matches!(
            outcome,
            BatchOutcome::Refused { fingerprint, reason }
                if fingerprint == &first_hash
                    && reason.contains(&first_id)
                    && reason.contains(&second_id)
        )));
        assert!(
            rendered_calls(&invocations).is_empty(),
            "collision refusal happens before dedup or mint bd queries",
        );
    }

    #[tokio::test]
    async fn mint_dedups_per_finding_hash_label_across_live_statuses() {
        let skipped = coherence_finding(vec![spec("gate")], "verifier-honesty", "evidence");
        let minted = contract_finding(vec![spec("gate")], "molecule-lifecycle", "evidence");
        let refused = style_finding(vec![spec("gate")], "RS-19", "evidence");
        let skipped_hash = skipped.hash();
        let minted_receipt = batch_fingerprint(std::slice::from_ref(&minted));
        let refused_hash = refused.hash();
        let dedup_response = format!(
            "[{},{}]",
            fixup_row("lm-dup.1", &refused_hash),
            fixup_row("lm-dup.2", &refused_hash),
        );
        let runner = ScriptedRunner::new(vec![
            ok_stdout(&format!("[{}]", fixup_row("lm-existing.1", &skipped_hash))),
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout(&dedup_response),
            ok_stdout("lm-newfix.1\n"),
        ]);
        let bd = BdClient::with_runner(runner);
        let summary = mint_findings(&bd, &[skipped, minted, refused], "head-sha").await;
        assert_eq!(summary.skipped, 1, "one live finding skipped: {summary:?}");
        assert_eq!(
            summary.minted, 1,
            "one untracked finding minted: {summary:?}"
        );
        assert_eq!(
            summary.refused, 1,
            "duplicate live finding refused: {summary:?}"
        );
        assert!(summary.batches.iter().any(|outcome| matches!(
            outcome,
            BatchOutcome::SkippedDedup { fingerprint, existing_bead, .. }
                if fingerprint == &skipped_hash && existing_bead.as_str() == "lm-existing.1"
        )));
        assert!(summary.batches.iter().any(|outcome| matches!(
            outcome,
            BatchOutcome::Minted { fingerprint, bead_id, .. }
                if fingerprint == &minted_receipt && bead_id.as_str() == "lm-newfix.1"
        )));
        assert!(summary.batches.iter().any(|outcome| matches!(
            outcome,
            BatchOutcome::Refused { fingerprint, reason }
                if fingerprint == &refused_hash && reason.contains("lm-dup.1") && reason.contains("lm-dup.2")
        )));
    }

    #[tokio::test]
    async fn closed_finding_hash_label_suppresses_remint_only_within_same_molecule() {
        let same_molecule = coherence_finding(vec![spec("gate")], "verifier-honesty", "evidence");
        let outside_history =
            contract_finding(vec![spec("gate")], "molecule-lifecycle", "evidence");
        let same_hash = same_molecule.hash();
        let outside_hash = outside_history.hash();
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout(&format!(
                "[{}]",
                fixup_row_status("lm-closed.1", &same_hash, "closed"),
            )),
            ok_stdout("[]"),
            ok_stdout("[]"),
            ok_stdout("lm-newfix.1\n"),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let opts = MintOptions {
            dry_run: false,
            spec_filter: None,
            suppressions: Vec::new(),
            suppress_closed_same_molecule: true,
            report_stale: false,
        };
        let summary =
            mint_findings_with_options(&bd, &[same_molecule, outside_history], "head-sha", &opts)
                .await;

        assert_eq!(
            summary.minted, 1,
            "outside-molecule history remains actionable"
        );
        assert_eq!(
            summary.skipped, 1,
            "closed same-molecule hit records a skip"
        );
        assert!(summary.batches.iter().any(|outcome| matches!(
            outcome,
            BatchOutcome::SkippedClosed { existing_bead, .. }
                if existing_bead.as_str() == "lm-closed.1"
        )));
        assert!(summary.batches.iter().any(|outcome| matches!(
            outcome,
            BatchOutcome::Minted { bead_id, .. } if bead_id.as_str() == "lm-newfix.1"
        )));
        let calls = rendered_calls(&invocations);
        for hash in [same_hash, outside_hash] {
            assert!(
                calls.iter().any(|call| call
                    == &[
                        "list",
                        "--json",
                        "--status=closed",
                        &format!("--label={FINDING_LABEL_PREFIX}{hash}"),
                        "--parent=lm-gateepic",
                    ]),
                "closed suppression query must be scoped to the owning molecule: {calls:?}",
            );
        }
    }

    #[tokio::test]
    async fn mint_refuses_live_finding_hash_collision() {
        let current = coherence_finding(vec![spec("gate")], "verifier-honesty", "evidence");
        let other = contract_finding(vec![spec("gate")], "molecule-lifecycle", "old evidence");
        let conflicting_description = format!(
            "Findings (1):\n\n- **{}** — `{}`\n  id: `{}`\n  hash: `{}`\n  evidence: old\n",
            other.token.as_wire(),
            other.target.canonical_form(),
            other.id(),
            current.hash(),
        );
        let runner = ScriptedRunner::new(vec![ok_stdout(&format!(
            "[{}]",
            fixup_row_status_description(
                "lm-collision.1",
                &current.hash(),
                "open",
                &conflicting_description,
            ),
        ))]);
        let bd = BdClient::with_runner(runner);
        let summary = mint_findings(&bd, &[current], "head-sha").await;
        assert_eq!(summary.refused, 1, "live id/hash collision refuses");
        match &summary.batches[0] {
            BatchOutcome::Refused { reason, .. } => {
                assert!(
                    reason.contains("lm-collision.1"),
                    "reason names bead: {reason}"
                );
                assert!(
                    reason.contains("refuse hash collision"),
                    "reason names collision: {reason}"
                );
            }
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn blocked_clarify_bead_dedups_same_finding_hash() {
        let finding = invariant_clash_finding(
            vec![spec("gate")],
            spec("gate"),
            "Out of Scope",
            "loom-runs-podman",
            "## Options — pick one\n\n### Option 1 — keep it\nkeep.\n",
        );
        let hash = finding.hash();
        let dedup_response = format!("[{}]", fixup_row_status("lm-parked.5", &hash, "blocked"));
        let runner = ScriptedRunner::new(vec![ok_stdout(&dedup_response)]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let summary = mint_findings(&bd, &[finding], "head-sha").await;
        assert_eq!(summary.skipped, 1, "blocked dup skips re-mint: {summary:?}");
        match &summary.batches[0] {
            BatchOutcome::SkippedDedup { existing_bead, .. } => {
                assert_eq!(existing_bead.as_str(), "lm-parked.5");
            }
            other => panic!("expected SkippedDedup, got {other:?}"),
        }
        let calls = rendered_calls(&invocations);
        assert!(
            !calls.iter().any(|c| c.iter().any(|a| a == "create")),
            "no bd create fires when a blocked dup already exists: {calls:?}",
        );
        let dedup_call = calls
            .iter()
            .find(|c| {
                c.iter()
                    .any(|a| a.starts_with(&format!("--label={FINDING_LABEL_PREFIX}")))
            })
            .expect("dedup list call recorded");
        assert!(
            dedup_call
                .iter()
                .any(|a| a == &format!("--status={DEDUP_STATUSES}")),
            "dedup query must include blocked in its status set: {dedup_call:?}",
        );
    }

    #[tokio::test]
    async fn loom_toml_suppress_entries_filter_rubric_findings_by_id_or_hash() {
        let by_id = coherence_finding(vec![spec("gate")], "verifier-honesty", "evidence");
        let by_hash = contract_finding(vec![spec("gate")], "molecule-lifecycle", "evidence");
        let config = LoomConfig::from_toml_str(&format!(
            r#"
[[suppress]]
id = {:?}
reason = "false positive"

[[suppress]]
hash = {:?}
reason = "false positive"
"#,
            by_id.id(),
            by_hash.hash(),
        ))
        .expect("valid loom.toml suppressions");
        let runner = ScriptedRunner::new(Vec::new());
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let opts = MintOptions {
            dry_run: false,
            spec_filter: None,
            suppressions: config.suppress,
            suppress_closed_same_molecule: false,
            report_stale: false,
        };
        let summary = mint_findings_with_options(&bd, &[by_id, by_hash], "head-sha", &opts).await;
        assert_eq!(summary.suppressed, 2, "both rubric findings suppressed");
        assert!(summary.batches.is_empty(), "nothing reaches dedup or mint");
        assert!(
            rendered_calls(&invocations).is_empty(),
            "suppression happens before bd queries",
        );
        assert!(
            summary
                .statuses
                .iter()
                .all(|s| s.action == FindingStatusAction::Suppressed),
            "suppressed status recorded: {summary:?}",
        );
    }

    #[tokio::test]
    async fn suppressions_do_not_filter_deterministic_or_integrity_findings() {
        let finding = deterministic_finding(vec![spec("gate")], "cargo test --lib failing");
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout("lm-newfix.1\n"),
        ]);
        let bd = BdClient::with_runner(runner);
        let opts = MintOptions {
            dry_run: false,
            spec_filter: None,
            suppressions: vec![SuppressionConfig {
                id: Some(finding.id()),
                hash: None,
                reason: "must not suppress deterministic failures".to_owned(),
            }],
            suppress_closed_same_molecule: false,
            report_stale: false,
        };
        let summary = mint_findings_with_options(&bd, &[finding], "head-sha", &opts).await;
        assert_eq!(summary.suppressed, 0, "deterministic finding remains live");
        assert_eq!(summary.ineffective_suppressions, 1);
        assert_eq!(summary.minted, 1, "deterministic finding still mints");
        assert!(
            !summary
                .statuses
                .iter()
                .any(|s| s.action == FindingStatusAction::Suppressed),
            "ignored suppression must not emit a suppressed status: {summary:?}",
        );
        assert!(
            summary
                .statuses
                .iter()
                .any(|s| s.action == FindingStatusAction::Minted),
            "actionable deterministic status recorded: {summary:?}",
        );
    }

    #[tokio::test]
    async fn mint_tree_reports_stale_candidates_without_closing() {
        let finding = coherence_finding(vec![spec("gate")], "verifier-honesty", "evidence");
        let stale_hash = "v1:stalehash000";
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout("lm-newfix.1\n"),
            ok_stdout(&format!(
                "[{}]",
                fixup_row_with_labels(
                    "lm-stale.1",
                    "open",
                    &[&format!("{FINDING_LABEL_PREFIX}{stale_hash}"), "spec:gate"],
                    "stale evidence",
                ),
            )),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let opts = MintOptions {
            dry_run: false,
            spec_filter: Some(spec("gate")),
            suppressions: Vec::new(),
            suppress_closed_same_molecule: false,
            report_stale: true,
        };
        let summary = mint_findings_with_options(&bd, &[finding], "head-sha", &opts).await;

        assert_eq!(summary.stale_candidates, 1);
        assert!(summary.render().contains("stale-candidate lm-stale.1"));
        let calls = rendered_calls(&invocations);
        assert!(
            calls
                .iter()
                .any(|call| call.iter().any(|arg| arg == "--label=spec:gate")
                    && call
                        .iter()
                        .any(|arg| arg == &format!("--status={DEDUP_STATUSES}"))),
            "stale reporting must list live remediation beads for the spec: {calls:?}",
        );
        assert!(
            calls
                .iter()
                .all(|call| call.first().is_none_or(|arg| arg != "close")),
            "stale reporting must not auto-close candidates: {calls:?}",
        );
    }

    #[tokio::test]
    async fn mint_tree_reports_partially_stale_batches_without_superseding() {
        let finding = coherence_finding(vec![spec("gate")], "verifier-honesty", "evidence");
        let current_hash = finding.hash();
        let absent_hash = "v1:absent000000";
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout("lm-newfix.1\n"),
            ok_stdout(&format!(
                "[{}]",
                fixup_row_with_labels(
                    "lm-partial.1",
                    "open",
                    &[
                        &format!("{FINDING_LABEL_PREFIX}{current_hash}"),
                        &format!("{FINDING_LABEL_PREFIX}{absent_hash}"),
                        "spec:gate",
                    ],
                    "partial evidence",
                ),
            )),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let opts = MintOptions {
            dry_run: false,
            spec_filter: Some(spec("gate")),
            suppressions: Vec::new(),
            suppress_closed_same_molecule: false,
            report_stale: true,
        };
        let summary = mint_findings_with_options(&bd, &[finding], "head-sha", &opts).await;

        assert_eq!(summary.partial_stale_candidates, 1);
        assert!(
            summary
                .render()
                .contains("partial-stale-candidate lm-partial.1")
        );
        assert!(summary.render().contains(&current_hash));
        assert!(summary.render().contains(absent_hash));
        let calls = rendered_calls(&invocations);
        assert!(
            calls
                .iter()
                .all(|call| call.first().is_none_or(|arg| arg != "update")),
            "partial-stale reporting must not supersede or split automatically: {calls:?}",
        );
    }

    #[tokio::test]
    async fn mint_tree_reports_stale_candidates_without_current_findings() {
        let stale_hash = "v1:stalehash000";
        let runner = ScriptedRunner::new(vec![ok_stdout(&format!(
            "[{}]",
            fixup_row_with_labels(
                "lm-stale.1",
                "open",
                &[&format!("{FINDING_LABEL_PREFIX}{stale_hash}"), "spec:gate"],
                "stale evidence",
            ),
        ))]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let opts = MintOptions {
            dry_run: false,
            spec_filter: None,
            suppressions: Vec::new(),
            suppress_closed_same_molecule: false,
            report_stale: true,
        };
        let summary = mint_findings_with_options(&bd, &[], "head-sha", &opts).await;

        assert_eq!(summary.stale_candidates, 1);
        assert!(summary.render().contains("stale-candidate lm-stale.1"));
        let calls = rendered_calls(&invocations);
        assert!(
            calls
                .iter()
                .any(|call| call == &["list", "--json", &format!("--status={DEDUP_STATUSES}")]),
            "unfiltered tree stale reporting must list all live candidates: {calls:?}",
        );
    }

    #[tokio::test]
    async fn mint_non_tree_scopes_do_not_report_stale_candidates() {
        let finding = coherence_finding(vec![spec("gate")], "verifier-honesty", "evidence");
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout("lm-newfix.1\n"),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let opts = MintOptions {
            dry_run: false,
            spec_filter: None,
            suppressions: Vec::new(),
            suppress_closed_same_molecule: false,
            report_stale: false,
        };
        let summary = mint_findings_with_options(&bd, &[finding], "head-sha", &opts).await;

        assert_eq!(summary.stale_candidates, 0);
        assert_eq!(summary.partial_stale_candidates, 0);
        let calls = rendered_calls(&invocations);
        assert!(
            calls.iter().all(|call| {
                !call
                    .iter()
                    .any(|arg| arg == &format!("--status={DEDUP_STATUSES}"))
                    || !call.iter().any(|arg| arg.starts_with("--label=spec:"))
            }),
            "non-tree mint must not perform stale-candidate scans: {calls:?}",
        );
    }

    #[tokio::test]
    async fn mint_summary_emits_finding_status_json_with_identity_and_action() {
        let finding = coherence_finding(vec![spec("gate")], "verifier-honesty", "evidence");
        let id = finding.id();
        let hash = finding.hash();
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout("lm-newfix.1\n"),
        ]);
        let bd = BdClient::with_runner(runner);
        let summary = mint_findings(&bd, &[finding], "head-sha").await;
        let render = summary.render();
        let status_line = render
            .lines()
            .find(|line| line.starts_with(LOOM_FINDING_STATUS_PREFIX))
            .expect("status line emitted");
        let payload = status_line
            .strip_prefix(LOOM_FINDING_STATUS_PREFIX)
            .expect("prefix")
            .trim();
        let json: serde_json::Value = serde_json::from_str(payload).expect("status json");
        assert_eq!(json["id"], id);
        assert_eq!(json["hash"], hash);
        assert_eq!(json["label"], format!("{FINDING_LABEL_PREFIX}{hash}"));
        assert_eq!(json["token"], "spec-coherence-fail");
        assert_eq!(json["target"]["kind"], "Criterion");
        assert_eq!(json["action"], "minted");
    }

    #[tokio::test]
    async fn mint_dedup_skips_reopened_batch_still_carrying_finding_hash_label() {
        let finding = contract_finding(vec![spec("gate")], "molecule-lifecycle", "evidence");
        let hash = finding.hash();
        let dedup_response = format!("[{}]", fixup_row("lm-reopened.3", &hash));
        let runner = ScriptedRunner::new(vec![ok_stdout(&dedup_response)]);
        let bd = BdClient::with_runner(runner);
        let summary = mint_findings(&bd, &[finding], "head-sha").await;
        match &summary.batches[0] {
            BatchOutcome::SkippedDedup { existing_bead, .. } => {
                assert_eq!(existing_bead.as_str(), "lm-reopened.3");
            }
            other => panic!("expected SkippedDedup, got {other:?}"),
        }
    }

    #[test]
    fn mint_batch_receipt_is_stable_across_rubric_runs_for_same_finding_set() {
        let a = coherence_finding(vec![spec("gate")], "verifier-honesty", "first prose");
        let b = contract_finding(vec![spec("gate")], "molecule-lifecycle", "first prose");
        let c = style_finding(vec![spec("gate")], "RS-19", "first prose");

        let original_order = batch_fingerprint(&[a.clone(), b.clone(), c.clone()]);
        let reversed_stream_order = batch_fingerprint(&[c.clone(), b.clone(), a.clone()]);
        assert_eq!(
            original_order, reversed_stream_order,
            "stream order MUST NOT shift batch receipt: {original_order} vs {reversed_stream_order}",
        );

        let reordered_bonds = Finding {
            bonds: vec![spec("harness"), spec("gate")],
            ..a.clone()
        };
        let with_reordered_bonds = batch_fingerprint(&[reordered_bonds, b.clone(), c.clone()]);
        assert_eq!(
            original_order, with_reordered_bonds,
            "bonds shifts MUST NOT change batch receipt",
        );

        let tweaked_evidence = Finding {
            evidence: "tweaked prose".into(),
            ..a.clone()
        };
        let with_tweaked_evidence = batch_fingerprint(&[tweaked_evidence, b, c]);
        assert_eq!(
            original_order, with_tweaked_evidence,
            "evidence prose MUST NOT change batch receipt"
        );

        assert_eq!(
            original_order.len(),
            12,
            "12-character batch receipt: {original_order}",
        );
    }

    /// Spec contract `specs/gate.md` § *Findings and Minting*:
    /// the minted batch bead is `--parent`-ed to the lead spec's open
    /// epic and carries one `finding:<hash>` label per finding plus one
    /// `spec:<X>` label per unique entry across the union of `bonds`
    /// over the batch's findings.
    #[tokio::test]
    async fn mint_creates_batch_under_work_epic_with_finding_hash_and_union_spec_labels() {
        let f1 = contract_finding(
            vec![spec("gate"), spec("harness")],
            "molecule-lifecycle",
            "evidence-1",
        );
        let f2 = coherence_finding(vec![spec("gate")], "verifier-honesty", "evidence-2");
        let f3 = style_finding(vec![spec("gate"), spec("templates")], "RS-19", "evidence-3");
        let finding_labels = [finding_label(&f1), finding_label(&f2), finding_label(&f3)];
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout("[]"),
            ok_stdout("[]"),
            ok_stdout("lm-harn\n"),
            ok_stdout("[]"),
            ok_stdout("[]"),
            ok_stdout("[]"),
            ok_stdout("[]"),
            ok_stdout("lm-tmpl\n"),
            ok_stdout("lm-batch.1\n"),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let opts = MintOptions {
            suppress_closed_same_molecule: false,
            report_stale: false,
            ..MintOptions::default()
        };
        let summary = mint_findings_with_options(&bd, &[f1, f2, f3], "head-sha", &opts).await;
        assert_eq!(summary.minted, 1, "one batch minted: {summary:?}");
        let calls = rendered_calls(&invocations);
        let create = task_create_call(&calls);

        assert_eq!(flag_arg(create, "--parent"), "lm-gateepic");

        let labels = labels_arg(create);
        let finding_label_count = labels
            .iter()
            .filter(|label| label.starts_with(FINDING_LABEL_PREFIX))
            .count();
        assert_eq!(
            finding_label_count,
            finding_labels.len(),
            "labels: {labels:?}"
        );
        for finding_label in finding_labels {
            assert!(
                labels.contains(&finding_label.as_str()),
                "labels missing finding hash label {finding_label}: {labels:?}",
            );
        }
        for spec_label in ["spec:gate", "spec:harness", "spec:templates"] {
            assert!(
                labels.contains(&spec_label),
                "labels missing union spec label {spec_label}: {labels:?}",
            );
        }

        assert_eq!(flag_arg(create, "--type"), "task");
    }

    /// Spec contract: lead selection and batching are placement choices;
    /// they do not rewrite the id/hash identity of contained findings.
    #[tokio::test]
    async fn mint_bonding_lead_groups_findings_without_affecting_identity() {
        let f1 = contract_finding(
            vec![spec("harness"), spec("gate")],
            "molecule-lifecycle",
            "first",
        );
        let f2 = coherence_finding(
            vec![spec("harness"), spec("gate")],
            "verifier-honesty",
            "second",
        );
        let expected_identity = [
            (f1.id(), f1.hash(), finding_label(&f1)),
            (f2.id(), f2.hash(), finding_label(&f2)),
        ];
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout("[]"),
            ok_stdout("[]"),
            ok_stdout("lm-harn\n"),
            ok_stdout("[]"),
            ok_stdout("[]"),
            ok_stdout("lm-gateepic\n"),
            ok_stdout("[]"),
            ok_stdout("lm-batch.1\n"),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let opts = MintOptions {
            suppress_closed_same_molecule: false,
            report_stale: false,
            ..MintOptions::default()
        };
        let summary = mint_findings_with_options(&bd, &[f1, f2], "head-sha", &opts).await;
        assert_eq!(summary.minted, 1, "two findings → one batch: {summary:?}");
        match &summary.batches[0] {
            BatchOutcome::Minted {
                lead_spec,
                findings_count,
                ..
            } => {
                assert_eq!(lead_spec.as_str(), "harness");
                assert_eq!(*findings_count, 2);
            }
            other => panic!("expected Minted, got {other:?}"),
        }

        let calls = rendered_calls(&invocations);
        let create = task_create_call(&calls);
        assert_eq!(flag_arg(create, "--parent"), "lm-harn");
        let labels = labels_arg(create);
        let description = flag_arg(create, "--description");
        for (id, hash, label) in expected_identity {
            assert!(labels.contains(&label.as_str()), "labels: {labels:?}");
            assert!(
                description.contains(&format!("id: `{id}`")),
                "{description}"
            );
            assert!(
                description.contains(&format!("hash: `{hash}`")),
                "{description}"
            );
        }
    }

    /// Spec contract `specs/gate.md` § *Standing-safety-net bonding*:
    /// `loom gate mint --tree` resolves the lead from `bonds[0]` after
    /// ensuring every bonded spec has an epic. Missing sibling spec epics
    /// are bootstrapped before the remediation task is parented under the
    /// lead work epic.
    #[tokio::test]
    async fn mint_tree_scope_resolves_lead_spec_and_ensures_spec_epic() {
        let finding = contract_finding(vec![spec("alpha"), spec("beta")], "x", "evidence");
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-alphaepic", "alpha")),
            ok_stdout("[]"),
            ok_stdout("[]"),
            ok_stdout("lm-betaepic\n"),
            ok_stdout("lm-fix.1\n"),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let opts = MintOptions {
            suppress_closed_same_molecule: false,
            report_stale: false,
            ..MintOptions::default()
        };
        let summary = mint_findings_with_options(&bd, &[finding], "deadbeef", &opts).await;
        assert_eq!(summary.minted, 1, "summary: {summary:?}");
        match &summary.batches[0] {
            BatchOutcome::Minted { lead_spec, .. } => assert_eq!(lead_spec.as_str(), "alpha"),
            other => panic!("expected Minted, got {other:?}"),
        }
        let calls = rendered_calls(&invocations);
        assert!(
            calls
                .iter()
                .any(|call| call.iter().any(|arg| arg == "create")
                    && call
                        .windows(2)
                        .any(|pair| pair[0] == "--type" && pair[1] == "epic")
                    && call
                        .windows(2)
                        .any(|pair| pair[0] == "--labels" && pair[1] == "spec:beta")),
            "missing bonded spec must be bootstrapped as an epic: {calls:?}",
        );
        let create = task_create_call(&calls);
        assert_eq!(flag_arg(create, "--parent"), "lm-alphaepic");
    }

    /// Spec contract: when the lead query returns one open epic, the
    /// pipeline bonds the batch to that existing epic — it MUST NOT
    /// mint a duplicate molecule.
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
        let summary = mint_findings(&bd, &[finding], "head-sha").await;
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
        assert_eq!(create[type_idx + 1], "task");
        assert!(
            matches!(summary.batches[0], BatchOutcome::Minted { ref lead_spec, .. } if lead_spec.as_str() == "alpha"),
        );
    }

    /// Spec contract: clarify-bound findings mint as single-finding
    /// beads (not bundled into the spec's fix-up batch) carrying
    /// `finding:<hash>` and `loom:clarify` labels, with the description
    /// embedding the `## Options — …` block from the finding's evidence.
    #[tokio::test]
    async fn mint_clarify_bound_finding_creates_single_bead_with_finding_hash_label_and_options_block()
     {
        let options_block = "## Options — keep loom out of podman\n\n\
                             ### Option 1 — Preserve the invariant\n\
                             rework podman call to delegate to wrix.\n\n\
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
        let finding_hash_label = finding_label(&finding);
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-harn", "harness")),
            ok_stdout("lm-clarify.1\n"),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let summary = mint_findings(&bd, &[finding], "head-sha").await;
        assert_eq!(summary.minted, 1);
        let calls = rendered_calls(&invocations);
        let create = calls
            .iter()
            .find(|c| c.iter().any(|a| a == "create"))
            .expect("create");

        let labels = labels_arg(create);
        assert!(
            labels.contains(&finding_hash_label.as_str()),
            "labels missing finding hash label {finding_hash_label}: {labels:?}",
        );
        assert!(
            labels.contains(&"loom:clarify"),
            "invariant-clash must carry loom:clarify: {labels:?}",
        );

        let description = flag_arg(create, "--description");
        assert!(
            description.contains(options_block.trim_end()),
            "description must preserve the Options block verbatim: {description}",
        );
    }

    /// Spec contract `specs/templates.md` § Options-block requirement on
    /// clarify-bound findings: a clarify-bound finding whose evidence
    /// omits the canonical `## Options — …` block does NOT mint a
    /// `loom:clarify` bead; instead the mint pipeline downgrades it to
    /// `loom:blocked` with cause `clarify-without-options`.
    #[tokio::test]
    async fn mint_clarify_bound_finding_without_options_falls_back_to_blocked() {
        let malformed_evidence = "Some prose without the canonical Options block heading.";
        let finding = invariant_clash_finding(
            vec![spec("harness")],
            spec("harness"),
            "Out of Scope",
            "loom-runs-podman",
            malformed_evidence,
        );
        let fp = batch_fingerprint(std::slice::from_ref(&finding));
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-harn", "harness")),
            ok_stdout("lm-blocked.1\n"),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let summary = mint_findings(&bd, &[finding], "head-sha").await;
        assert_eq!(summary.minted, 1);
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
            "labels missing batch receipt: {labels}",
        );
        assert!(
            labels.contains("loom:blocked"),
            "malformed evidence must downgrade to loom:blocked: {labels}",
        );
        assert!(
            !labels.contains("loom:clarify"),
            "loom:clarify MUST NOT be applied when options block is missing: {labels}",
        );

        let desc_idx = create
            .iter()
            .position(|a| a == "--description")
            .expect("--description");
        let description = &create[desc_idx + 1];
        assert!(
            description.contains(CLARIFY_WITHOUT_OPTIONS_CAUSE),
            "description must cite the cause string: {description}",
        );
    }

    /// Spec contract `specs/gate.md` § *Per-batch processing* step 8:
    /// fix-up batches enumerate every finding in the bead description
    /// (one item per finding: token, target's canonical form, evidence
    /// excerpt); the title is stable across runs for the same batch.
    #[tokio::test]
    async fn mint_batch_description_enumerates_finding_identity_and_title_is_stable() {
        let f1 = coherence_finding(vec![spec("gate")], "verifier-honesty", "evidence-A");
        let f2 = contract_finding(vec![spec("gate")], "molecule-lifecycle", "evidence-B");
        let f3 = style_finding(vec![spec("gate")], "RS-19", "evidence-C");

        // First ordering: [f1, f2, f3].
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout("[]"),
            ok_stdout("[]"),
            ok_stdout("lm-batch.1\n"),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let _ = mint_findings(&bd, &[f1.clone(), f2.clone(), f3.clone()], "head-sha").await;
        let calls = rendered_calls(&invocations);
        let create = calls
            .iter()
            .find(|c| c.iter().any(|a| a == "create"))
            .expect("create");
        let title_idx = create.iter().position(|a| a == "--title").expect("--title");
        let title_first = create[title_idx + 1].clone();
        let desc_idx = create
            .iter()
            .position(|a| a == "--description")
            .expect("--description");
        let description = &create[desc_idx + 1];

        assert!(
            description.contains("spec-coherence-fail"),
            "description enumerates each finding's token: {description}",
        );
        assert!(
            description.contains("orphan-integration"),
            "description enumerates each finding's token: {description}",
        );
        assert!(
            description.contains("style-rule-violation"),
            "description enumerates each finding's token: {description}",
        );
        assert!(
            description.contains("criterion:gate:verifier-honesty"),
            "description names each canonical target form: {description}",
        );
        assert!(
            description.contains("contract:molecule-lifecycle"),
            "description names each canonical target form: {description}",
        );
        assert!(
            description.contains("style:RS-19:crates/loom-workflow/src/mint/mod.rs"),
            "description names each canonical target form: {description}",
        );
        assert!(
            description.contains("evidence-A"),
            "description includes per-finding evidence excerpt: {description}",
        );
        assert!(
            description.contains("evidence-B"),
            "description includes per-finding evidence excerpt: {description}",
        );
        assert!(
            description.contains("evidence-C"),
            "description includes per-finding evidence excerpt: {description}",
        );
        for finding in [&f1, &f2, &f3] {
            assert!(
                description.contains(&format!("id: `{}`", finding.id())),
                "description enumerates finding id: {description}",
            );
            assert!(
                description.contains(&format!("hash: `{}`", finding.hash())),
                "description enumerates finding hash: {description}",
            );
        }

        // Second ordering of the same set: [f3, f1, f2]. The same set
        // produces the same concern-token summary title.
        let runner2 = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout("[]"),
            ok_stdout("[]"),
            ok_stdout("lm-batch.2\n"),
        ]);
        let invocations2 = runner2.invocations_handle();
        let bd2 = BdClient::with_runner(runner2);
        let _ = mint_findings(&bd2, &[f3, f1, f2], "head-sha").await;
        let calls2 = rendered_calls(&invocations2);
        let create2 = calls2
            .iter()
            .find(|c| c.iter().any(|a| a == "create"))
            .expect("create");
        let title_idx2 = create2
            .iter()
            .position(|a| a == "--title")
            .expect("--title");
        let title_second = create2[title_idx2 + 1].clone();
        assert_eq!(
            title_first, title_second,
            "title is deterministic from the finding set regardless of stream order: {title_first:?} vs {title_second:?}",
        );
    }

    #[test]
    fn multi_finding_title_summarizes_tokens_without_truncating_description() {
        let long_target = format!("nix run .#{}", "test".repeat(200));
        let mut findings = (0..12)
            .map(|idx| {
                verifier_bypass_finding(
                    vec![spec("gate")],
                    &format!("{long_target}-{idx}"),
                    "long target evidence",
                )
            })
            .collect::<Vec<_>>();
        findings.push(coherence_finding(
            vec![spec("gate")],
            "functional",
            "coherence evidence",
        ));

        let title = batch_title(&findings, &spec("gate"));
        assert_eq!(
            title,
            "fix-up batch: 13 findings for gate (verifier-bypass×12, spec-coherence-fail)",
        );
        assert!(
            title.len() <= BD_TITLE_MAX_BYTES,
            "bd title must fit the CLI limit: {} > {BD_TITLE_MAX_BYTES}: {title}",
            title.len(),
        );

        let description = batch_description(
            &findings,
            &batch_fingerprint(&findings),
            FindingRouting::Fixup,
        );
        assert!(
            description.contains(&format!("{long_target}-11")),
            "description retains the full finding target even when the title is summarised: {description}",
        );
    }

    #[test]
    fn single_finding_title_is_capped_to_bd_limit() {
        let long_target = format!("nix run .#{}", "test".repeat(200));
        let finding = verifier_bypass_finding(vec![spec("gate")], &long_target, "evidence");

        let title = batch_title(&[finding], &spec("gate"));
        assert!(
            title.len() <= BD_TITLE_MAX_BYTES,
            "bd title must fit the CLI limit: {} > {BD_TITLE_MAX_BYTES}: {title}",
            title.len(),
        );
        assert!(
            title.ends_with(TRUNCATED_TITLE_SUFFIX),
            "over-limit single-finding titles should carry a truncation marker: {title}",
        );
    }

    #[tokio::test]
    async fn mint_error_summary_includes_bd_cli_source() {
        let finding = coherence_finding(vec![spec("gate")], "verifier-honesty", "evidence-A");
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            RunOutput {
                status: 1,
                stdout: Vec::new(),
                stderr: b"Error: validation failed for issue lm-gateepic.1: title must be 500 characters or less (got 972)\n".to_vec(),
            },
        ]);
        let bd = BdClient::with_runner(runner);

        let summary = mint_findings(&bd, &[finding], "head-sha").await;
        assert_eq!(summary.errors, 1, "bd create failure is an error");
        let rendered = summary.render();
        assert!(
            rendered.contains("bd CLI failure while minting findings: `bd` exited with status 1"),
            "summary must include the bd source error: {rendered}",
        );
        assert!(
            rendered.contains("title must be 500 characters or less"),
            "summary must include bd stderr: {rendered}",
        );
    }

    /// Spec contract `specs/gate.md` § *Per-batch processing* end-of-run
    /// shape: the summary lists minted batches (with finding count per
    /// batch), skipped-dedup, refused, and errored counts with identifiers
    /// and resulting bead id.
    #[tokio::test]
    async fn mint_end_of_run_summary_reports_per_batch_outcomes() {
        // Three lead-specs ⇒ three batches: one minted (gate), one
        // skipped-dedup (harness), one refused (alpha, duplicate
        // finding label).
        let f_mint_1 = coherence_finding(vec![spec("gate")], "verifier-honesty", "evidence-A");
        let f_mint_2 = style_finding(vec![spec("gate")], "RS-19", "evidence-B");
        let f_skip = contract_finding(vec![spec("harness")], "contract-x", "evidence-C");
        let f_refuse = contract_finding(vec![spec("alpha")], "contract-y", "evidence-D");
        let fp_mint = batch_fingerprint(&[f_mint_1.clone(), f_mint_2.clone()]);
        let fp_skip = f_skip.hash();
        let fp_refuse = f_refuse.hash();

        let responses = vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout("[]"),
            ok_stdout(&format!("[{}]", fixup_row("lm-existing.2", &fp_skip))),
            ok_stdout(&format!(
                "[{},{}]",
                fixup_row("lm-dup.1", &fp_refuse),
                fixup_row("lm-dup.2", &fp_refuse),
            )),
            ok_stdout("lm-newfix.1\n"),
        ];
        let runner = ScriptedRunner::new(responses);
        let bd = BdClient::with_runner(runner);
        let summary = mint_findings(&bd, &[f_mint_1, f_mint_2, f_skip, f_refuse], "head-sha").await;
        assert_eq!(summary.minted, 1, "one batch minted");
        assert_eq!(summary.skipped, 1, "one batch skipped-dedup");
        assert_eq!(summary.refused, 1, "one batch refused");
        assert_eq!(summary.errors, 0);
        assert_eq!(
            summary.findings_across_minted, 2,
            "two findings in minted batch"
        );
        assert_eq!(summary.specs_across_minted, 1, "one spec in minted batches");
        let render = summary.render();
        assert!(
            render.starts_with(
                "minted 1 batches (2 findings across 1 specs), promoted 0 deferred, skipped 1 (dedup), suppressed 0, ineffective suppressions 0, refused 1, errors 0\n"
            ),
            "header line shape: {render}",
        );
        assert!(
            render.contains(LOOM_FINDING_STATUS_PREFIX),
            "status lines are emitted: {render}",
        );
        assert!(
            render.contains(&fp_mint) && render.contains("lm-newfix.1"),
            "minted line names batch receipt + new bead id: {render}",
        );
        assert!(
            render.contains(&fp_skip) && render.contains("lm-existing.2"),
            "skip line names finding hash + existing bead id: {render}",
        );
        assert!(
            render.contains(&fp_refuse)
                && render.contains("lm-dup.1")
                && render.contains("lm-dup.2"),
            "refuse line names finding hash + conflicting ids: {render}",
        );
    }

    #[tokio::test]
    async fn mint_idempotent_after_partial_failure_retries_only_unfinished_findings() {
        let already_minted = coherence_finding(vec![spec("gate")], "verifier-honesty", "done");
        let unfinished = contract_finding(vec![spec("gate")], "molecule-lifecycle", "retry");
        let already_hash = already_minted.hash();
        let unfinished_label = finding_label(&unfinished);
        let runner = ScriptedRunner::new(vec![
            ok_stdout(&format!("[{}]", fixup_row("lm-finished.1", &already_hash))),
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout("lm-retry.1\n"),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let summary = mint_findings(&bd, &[already_minted, unfinished], "head-sha").await;
        assert_eq!(summary.skipped, 1, "finished finding is skipped");
        assert_eq!(summary.minted, 1, "unfinished finding is retried");
        let calls = rendered_calls(&invocations);
        let create = calls
            .iter()
            .find(|c| c.iter().any(|a| a == "create"))
            .expect("unfinished finding minted");
        let labels_idx = create
            .iter()
            .position(|a| a == "--labels")
            .expect("--labels");
        let labels = &create[labels_idx + 1];
        assert!(
            labels.contains(&unfinished_label),
            "retry batch carries unfinished finding label: {labels}",
        );
        assert!(
            !labels.contains(&format!("{FINDING_LABEL_PREFIX}{already_hash}")),
            "retry batch omits already-minted finding label: {labels}",
        );
    }

    /// Spec contract `specs/gate.md` § *Findings and Minting* (criterion
    /// `mint_dry_run_makes_no_bd_writes`): with `--dry-run`, the
    /// pipeline runs read-side queries to choose the bonding lead, but
    /// performs no bd writes even when no open epic exists. The outcome
    /// is `WouldMint` instead of `Minted`, and the only argv recorded by
    /// the runner are `list` calls.
    #[tokio::test]
    async fn mint_dry_run_makes_no_bd_writes() {
        let finding = coherence_finding(vec![spec("gate")], "verifier-honesty", "evidence");
        let fp = batch_fingerprint(std::slice::from_ref(&finding));
        let runner = ScriptedRunner::new(vec![ok_stdout("[]"), ok_stdout("[]")]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let opts = MintOptions {
            dry_run: true,
            spec_filter: None,
            suppressions: Vec::new(),
            suppress_closed_same_molecule: false,
            report_stale: false,
        };
        let summary = mint_findings_with_options(&bd, &[finding], "head-sha", &opts).await;
        assert_eq!(summary.minted, 0, "no bead minted under dry-run");
        assert_eq!(summary.would_mint, 1);
        match summary.batches.first().expect("one outcome recorded") {
            BatchOutcome::WouldMint {
                fingerprint,
                lead_spec,
                findings_count,
            } => {
                assert_eq!(fingerprint, &fp);
                assert_eq!(lead_spec.as_str(), "gate");
                assert_eq!(*findings_count, 1);
            }
            other => panic!("expected WouldMint, got {other:?}"),
        }
        let calls = rendered_calls(&invocations);
        for call in &calls {
            assert_eq!(
                call.first().map(String::as_str),
                Some("list"),
                "dry-run MUST only invoke read-side bd list calls: {call:?}",
            );
        }
    }

    /// Spec contract `specs/gate.md` § *Findings and Minting* (criterion
    /// `mint_spec_filter_drops_findings_routing_to_other_specs`):
    /// `--spec <X>` filters findings to those whose bonding lead
    /// resolves to `<X>`. Findings routing elsewhere are reported as
    /// `SkippedFilter` and no `bd create` fires for them.
    #[tokio::test]
    async fn mint_spec_filter_drops_findings_routing_to_other_specs() {
        let f_kept = coherence_finding(vec![spec("gate")], "verifier-honesty", "evidence-a");
        let f_dropped = contract_finding(vec![spec("harness")], "molecule-lifecycle", "evidence-b");
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-gateepic", "gate")),
            ok_stdout("[]"),
            ok_stdout(&epic_list("lm-harn", "harness")),
            ok_stdout("lm-fix.1\n"),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let opts = MintOptions {
            dry_run: false,
            spec_filter: Some(spec("gate")),
            suppressions: Vec::new(),
            suppress_closed_same_molecule: false,
            report_stale: false,
        };
        let summary =
            mint_findings_with_options(&bd, &[f_kept, f_dropped], "head-sha", &opts).await;
        assert_eq!(summary.minted, 1, "kept finding mints");
        assert_eq!(
            summary.skipped_filter, 1,
            "dropped finding reported as skipped-filter",
        );
        let dropped_outcome = summary
            .batches
            .iter()
            .find(|o| matches!(o, BatchOutcome::SkippedFilter { .. }))
            .expect("one SkippedFilter outcome recorded");
        match dropped_outcome {
            BatchOutcome::SkippedFilter {
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

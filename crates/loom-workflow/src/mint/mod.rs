//! Driver-side mint pipeline.
//!
//! Consumes [`Finding`] records produced by either the LLM rubric's
//! `LOOM_FINDING:` lines or the deterministic verifier verdict normaliser.
//! Tree-scope mint first builds an actionable plan: suppression and
//! `finding:<hash>` dedup happen before metadata spec-epic validation,
//! and lead specs affect grouping plus `spec:<label>` labels only. The
//! legacy molecule recovery path still materializes batches under a
//! caller-selected work epic; tree-scope mint materializes the actionable
//! plan under one standing remediation work epic.
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

use loom_driver::bd::{BdClient, Bead, CommandRunner, CreateOpts, Label, ListOpts, UpdateOpts};
use loom_driver::config::SuppressionConfig;
use loom_driver::identifier::{BeadId, MoleculeId, SpecLabel};
use loom_events::{DriverEventPayload, DriverKind};
use loom_gate::IntegrityFinding;
use loom_protocol::gate::options::has_well_formed_block;
use serde::Serialize;

use crate::gate_clarify::{
    CLARIFY_WITHOUT_OPTIONS_CAUSE, OptionsParseResult, evidence_excerpt as route_evidence_excerpt,
    evidence_hash,
};
use crate::resolve::{
    ResolveError, ensure_spec_metadata_epic, resolve_open_epic, resolve_or_mint_open_epic,
};
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

const ACTIVE_LABEL: &str = "loom:active";
const DEFERRED_LABEL: &str = "loom:deferred";
pub const GATE_ROUTING_STRUCTURAL_VIOLATION_CAUSE: &str = "gate-routing-structural-violation";
const EMPTY_TREE_EPIC_CLOSE_REASON: &str = "empty tree mint remediation cleanup";

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
    /// Tree planning selected an actionable batch, but materialization is
    /// deferred until a tree work epic exists.
    Planned {
        fingerprint: String,
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
            Self::Planned { .. } => "planned",
            Self::WouldMint { .. } => "would-mint",
            Self::SkippedDedup { .. } => "skipped-dedup",
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

impl FindingStatusAction {
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Reported => "reported",
            Self::Minted => "minted",
            Self::SkippedLive => "skipped-live",
            Self::Suppressed => "suppressed",
            Self::StaleCandidate => "stale-candidate",
            Self::PartialStaleCandidate => "partial-stale-candidate",
            Self::Refused => "refused",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FindingRoutingRecord {
    pub id: String,
    pub hash: String,
    pub token: ConcernToken,
    pub requested_route: FindingRoute,
    pub action: FindingStatusAction,
    pub options_parse_result: Option<OptionsParseResult>,
    pub evidence_hash: String,
    pub evidence_excerpt: String,
}

impl FindingRoutingRecord {
    fn new(finding: &Finding, action: FindingStatusAction) -> Self {
        let options_parse_result = (finding.route == FindingRoute::Clarify).then(|| {
            if has_well_formed_block(&finding.evidence) {
                OptionsParseResult::WellFormed
            } else {
                OptionsParseResult::MissingOrMalformed
            }
        });
        Self {
            id: finding.id(),
            hash: finding.hash(),
            token: finding.token,
            requested_route: finding.route,
            action,
            options_parse_result,
            evidence_hash: evidence_hash(&finding.evidence),
            evidence_excerpt: route_evidence_excerpt(&finding.evidence),
        }
    }

    #[must_use]
    pub fn clarify_downgraded(&self) -> bool {
        self.requested_route == FindingRoute::Clarify
            && self.options_parse_result == Some(OptionsParseResult::MissingOrMalformed)
            && self.action == FindingStatusAction::Minted
    }
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

/// Options that gate writes on a [`mint_findings`] run.
///
/// Defaults write to bd, apply no suppressions, and skip stale reporting;
/// production callers fill config-derived suppressions and tree-sweep flags.
#[derive(Debug, Clone, Default)]
pub struct MintOptions {
    /// When `true`, the pipeline runs every read-side query (dedup,
    /// lead resolution) but skips `bd create`. The resulting outcome is
    /// [`BatchOutcome::WouldMint`] instead of [`BatchOutcome::Minted`].
    pub dry_run: bool,
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
/// The `--dry-run` pseudo-outcome carries its own tally so summaries from
/// that mode remain self-describing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MintSummary {
    pub batches: Vec<BatchOutcome>,
    pub statuses: Vec<FindingStatusRecord>,
    pub routing: Vec<FindingRoutingRecord>,
    pub active_epic: Option<BeadId>,
    pub minted: usize,
    pub planned: usize,
    pub would_mint: usize,
    pub promoted_deferred: usize,
    pub would_promote_deferred: usize,
    pub blocking_findings: usize,
    pub deferred_findings_merged: usize,
    pub clarify_findings_raised: usize,
    pub ready_remediation_batches: usize,
    pub skipped: usize,
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
    /// Build route and Beads-transition events for successful mint actions.
    #[must_use]
    pub fn routing_events(&self) -> Vec<DriverEventPayload> {
        let mut events = Vec::new();
        for routing in &self.routing {
            let action = routing.action.as_wire();
            events.push(DriverEventPayload::new(
                DriverKind::MarkerRouted,
                format!("finding {} routed to {action}", routing.hash),
                serde_json::json!({
                    "source_route": "mint-finding",
                    "identity": routing.id,
                    "finding_hash": routing.hash,
                    "finding_token": routing.token,
                    "requested_route": routing.requested_route.as_wire(),
                    "route": action,
                }),
            ));
            if routing.clarify_downgraded() {
                events.push(DriverEventPayload::new(
                    DriverKind::ClarifyDowngraded,
                    format!("clarify finding {} downgraded to blocked", routing.hash),
                    serde_json::json!({
                        "source_route": "mint-finding",
                        "identity": routing.id,
                        "finding_hash": routing.hash,
                        "finding_token": routing.token,
                        "options_parse_result": routing.options_parse_result,
                        "evidence_hash": routing.evidence_hash,
                        "evidence_excerpt": routing.evidence_excerpt,
                        "cause": CLARIFY_WITHOUT_OPTIONS_CAUSE,
                    }),
                ));
            }
        }
        for outcome in &self.batches {
            match outcome {
                BatchOutcome::Minted { bead_id, .. } => events.push(DriverEventPayload::new(
                    DriverKind::BdStateTransition,
                    format!("Beads item {bead_id} created for routed findings"),
                    serde_json::json!({
                        "source_route": "mint-finding",
                        "bead_id": bead_id,
                        "mutation": "create",
                        "status": "open",
                    }),
                )),
                BatchOutcome::PromotedDeferred { bead_id, .. } => {
                    events.push(DriverEventPayload::new(
                        DriverKind::BdStateTransition,
                        format!("Beads item {bead_id} promoted from deferred"),
                        serde_json::json!({
                            "source_route": "mint-finding",
                            "bead_id": bead_id,
                            "mutation": "update",
                            "status": "open",
                            "removed_labels": ["loom:deferred"],
                        }),
                    ));
                }
                BatchOutcome::Planned { .. }
                | BatchOutcome::WouldMint { .. }
                | BatchOutcome::SkippedDedup { .. }
                | BatchOutcome::Refused { .. }
                | BatchOutcome::WouldPromoteDeferred { .. }
                | BatchOutcome::SkippedClosed { .. }
                | BatchOutcome::StaleCandidate { .. }
                | BatchOutcome::PartialStaleCandidate { .. }
                | BatchOutcome::Errored { .. } => {}
            }
        }
        events
    }

    /// Append a non-finding infrastructure/walk error to the rendered summary.
    ///
    /// Tree-scope mint uses this when a verifier or rubric source fails after
    /// other findings were collected: already-collected findings can still be
    /// materialized, while the process exits non-zero via the `errors` tally.
    pub fn record_error(&mut self, fingerprint: impl Into<String>, message: impl Into<String>) {
        self.record(BatchOutcome::Errored {
            fingerprint: fingerprint.into(),
            message: message.into(),
        });
    }

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
        self.routing
            .push(FindingRoutingRecord::new(finding, action));
    }

    fn record(&mut self, outcome: BatchOutcome) {
        match &outcome {
            BatchOutcome::Minted { .. } => self.minted += 1,
            BatchOutcome::Planned { .. } => self.planned += 1,
            BatchOutcome::WouldMint { .. } => self.would_mint += 1,
            BatchOutcome::PromotedDeferred { .. } => self.promoted_deferred += 1,
            BatchOutcome::WouldPromoteDeferred { .. } => self.would_promote_deferred += 1,
            BatchOutcome::SkippedDedup { .. } => self.skipped += 1,
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
            "minted {} batches ({} findings across {} specs); finding lifecycle: blocking {}, deferred merged {}, deferred promoted {} (promoted {} deferred), ready remediation batches {}, clarify raised {}, skipped live {}, suppressed {}, stale {}, partially stale {}, structural conflicts {}, transient errors {}",
            self.minted,
            self.findings_across_minted,
            self.specs_across_minted,
            self.blocking_findings,
            self.deferred_findings_merged,
            self.promoted_deferred,
            self.promoted_deferred,
            self.ready_remediation_batches,
            self.clarify_findings_raised,
            self.skipped,
            self.suppressed,
            self.stale_candidates,
            self.partial_stale_candidates,
            self.refused,
            self.errors,
        );
        if self.ineffective_suppressions > 0 {
            out.push_str(&format!(
                ", ineffective suppressions {}",
                self.ineffective_suppressions,
            ));
        }
        if self.planned > 0 {
            out.push_str(&format!(", planned {} tree batches", self.planned));
        }
        if self.would_mint > 0 {
            out.push_str(&format!(", would-mint {} (dry-run)", self.would_mint));
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
        if let Some(epic) = &self.active_epic {
            out.push_str(&format!("active remediation epic: {epic}\n"));
            out.push_str("next: loom loop\n");
        }
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
                BatchOutcome::Planned {
                    fingerprint,
                    lead_spec,
                    findings_count,
                } => {
                    out.push_str(&format!(
                        "  planned {fingerprint} (spec:{lead_spec}, {findings_count} findings)\n",
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
/// options (write to bd, no suppressions). Convenience wrapper over
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

#[derive(Debug)]
struct MoleculeMintGroup {
    lead_spec: SpecLabel,
    route: FindingRoute,
    findings: Vec<Finding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MoleculeBatchState {
    Ready,
    Deferred,
    Clarify,
    BlockedClarifyWithoutOptions,
}

/// Materialize one molecule-completion review's findings under the explicit
/// originating molecule. Blocking work becomes ready immediately, deferred
/// work stays parked until stabilization, and clarify work is one bead per
/// finding. Structural conflicts park the molecule for human resolution.
pub async fn route_molecule_findings<R: CommandRunner>(
    bd: &BdClient<R>,
    molecule: &MoleculeId,
    findings: &[Finding],
    opts: &MintOptions,
) -> MintSummary {
    let mut summary = route_molecule_findings_inner(bd, molecule, findings, opts).await;
    if summary.refused > 0 {
        match BeadId::new(molecule.as_str()) {
            Ok(bead) => {
                if let Err(err) = bd
                    .update(
                        &bead,
                        UpdateOpts {
                            status: Some("blocked".to_string()),
                            add_labels: vec!["loom:blocked".to_string()],
                            notes: Some(format!(
                                "{GATE_ROUTING_STRUCTURAL_VIOLATION_CAUSE}: {}",
                                summary.render(),
                            )),
                            ..UpdateOpts::default()
                        },
                    )
                    .await
                {
                    summary.record_error("molecule-routing-block", err.to_string());
                }
            }
            Err(err) => summary.record_error(
                "molecule-routing-block",
                format!("molecule id `{molecule}` is not a bead id: {err}"),
            ),
        }
    }
    summary
}

async fn route_molecule_findings_inner<R: CommandRunner>(
    bd: &BdClient<R>,
    molecule: &MoleculeId,
    findings: &[Finding],
    opts: &MintOptions,
) -> MintSummary {
    let mut summary = MintSummary::default();
    let parent = match BeadId::new(molecule.as_str()) {
        Ok(parent) => parent,
        Err(err) => {
            summary.record(BatchOutcome::Errored {
                fingerprint: molecule.to_string(),
                message: format!("molecule id `{molecule}` is not a bead id: {err}"),
            });
            return summary;
        }
    };
    if let Err(err) = bd.show(&parent).await {
        summary.record(BatchOutcome::Errored {
            fingerprint: molecule.to_string(),
            message: format!("missing molecule epic `{molecule}` or bd read failed: {err}"),
        });
        return summary;
    }
    let children = match bd
        .list(ListOpts {
            parent: Some(parent.clone()),
            status: Some(DEDUP_STATUSES.to_string()),
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
        for finding in findings {
            summary.record_status(finding, FindingStatusAction::Refused);
        }
        summary.record(BatchOutcome::Refused {
            fingerprint: molecule.to_string(),
            reason,
        });
        return summary;
    }

    let mut survivors = Vec::new();
    for finding in findings {
        if suppresses_rubric_finding(&opts.suppressions, finding) {
            summary.record_status(finding, FindingStatusAction::Suppressed);
            continue;
        }
        if has_ineffective_suppression_match(&opts.suppressions, finding) {
            summary.ineffective_suppressions += 1;
        }
        let hash = finding.hash();
        match dedup_live_finding(bd, finding).await {
            FindingDedup::Untracked | FindingDedup::Closed(_) => {}
            FindingDedup::Tracked(existing_bead) => {
                summary.record_status(finding, FindingStatusAction::SkippedLive);
                summary.record(BatchOutcome::SkippedDedup {
                    fingerprint: hash,
                    existing_bead,
                    findings_count: 1,
                });
                continue;
            }
            FindingDedup::Duplicate { reason } => {
                summary.record_status(finding, FindingStatusAction::Refused);
                summary.record(BatchOutcome::Refused {
                    fingerprint: hash,
                    reason,
                });
                continue;
            }
            FindingDedup::Errored { message } => {
                summary.record_status(finding, FindingStatusAction::Refused);
                summary.record(BatchOutcome::Errored {
                    fingerprint: hash,
                    message,
                });
                continue;
            }
        }
        if opts.suppress_closed_same_molecule {
            match dedup_closed_same_molecule(bd, finding, molecule).await {
                FindingDedup::Untracked | FindingDedup::Tracked(_) => {}
                FindingDedup::Closed(existing_bead) => {
                    summary.record_status(finding, FindingStatusAction::Reported);
                    summary.record(BatchOutcome::SkippedClosed {
                        fingerprint: hash,
                        existing_bead,
                        findings_count: 1,
                    });
                    continue;
                }
                FindingDedup::Duplicate { reason } => {
                    summary.record_status(finding, FindingStatusAction::Refused);
                    summary.record(BatchOutcome::Refused {
                        fingerprint: hash,
                        reason,
                    });
                    continue;
                }
                FindingDedup::Errored { message } => {
                    summary.record_status(finding, FindingStatusAction::Refused);
                    summary.record(BatchOutcome::Errored {
                        fingerprint: hash,
                        message,
                    });
                    continue;
                }
            }
        }
        survivors.push(finding.clone());
    }

    let groups = molecule_mint_groups(survivors);
    let mut routed_specs = HashSet::new();
    for group in groups {
        let routing = group
            .findings
            .first()
            .map_or(FindingRouting::Fixup, classify_routing);
        let state = match (group.route, routing) {
            (FindingRoute::Blocking, _) => MoleculeBatchState::Ready,
            (FindingRoute::Deferred, _) => MoleculeBatchState::Deferred,
            (FindingRoute::Clarify, FindingRouting::Clarify) => MoleculeBatchState::Clarify,
            (FindingRoute::Clarify, FindingRouting::BlockedClarifyWithoutOptions) => {
                MoleculeBatchState::BlockedClarifyWithoutOptions
            }
            (FindingRoute::Clarify, FindingRouting::Fixup) => {
                MoleculeBatchState::BlockedClarifyWithoutOptions
            }
        };
        let outcome = if state == MoleculeBatchState::Deferred {
            merge_or_create_deferred_batch(
                bd,
                &children,
                &group.findings,
                &group.lead_spec,
                &parent,
            )
            .await
        } else {
            create_molecule_batch(bd, &group.findings, &group.lead_spec, &parent, state).await
        };
        let succeeded = matches!(outcome, BatchOutcome::Minted { .. });
        record_batch_status(&mut summary, &group.findings, &outcome);
        if succeeded {
            let count = group.findings.len();
            summary.findings_across_minted += count;
            routed_specs.insert(group.lead_spec.as_str().to_owned());
            match state {
                MoleculeBatchState::Ready => {
                    summary.blocking_findings += count;
                    summary.ready_remediation_batches += 1;
                }
                MoleculeBatchState::Deferred => {
                    summary.deferred_findings_merged += count;
                }
                MoleculeBatchState::Clarify => {
                    summary.clarify_findings_raised += count;
                }
                MoleculeBatchState::BlockedClarifyWithoutOptions => {}
            }
        }
        summary.record(outcome);
    }
    summary.specs_across_minted = routed_specs.len();
    summary
}

fn molecule_mint_groups(findings: Vec<Finding>) -> Vec<MoleculeMintGroup> {
    let mut groups: Vec<MoleculeMintGroup> = Vec::new();
    for finding in findings {
        let Some(lead_spec) = finding.bonds.first().cloned() else {
            continue;
        };
        if finding.route == FindingRoute::Clarify {
            groups.push(MoleculeMintGroup {
                lead_spec,
                route: finding.route,
                findings: vec![finding],
            });
            continue;
        }
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.lead_spec == lead_spec && group.route == finding.route)
        {
            group.findings.push(finding);
        } else {
            groups.push(MoleculeMintGroup {
                lead_spec,
                route: finding.route,
                findings: vec![finding],
            });
        }
    }
    groups.sort_by(|left, right| {
        left.lead_spec
            .as_str()
            .cmp(right.lead_spec.as_str())
            .then_with(|| left.route.as_wire().cmp(right.route.as_wire()))
    });
    groups
}

async fn merge_or_create_deferred_batch<R: CommandRunner>(
    bd: &BdClient<R>,
    children: &[Bead],
    findings: &[Finding],
    lead_spec: &SpecLabel,
    parent: &BeadId,
) -> BatchOutcome {
    let spec_label = format!("spec:{lead_spec}");
    let matches = children
        .iter()
        .filter(|bead| {
            is_deferred_remediation(bead)
                && bead.labels.iter().any(|label| label.as_str() == spec_label)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => {
            create_molecule_batch(
                bd,
                findings,
                lead_spec,
                parent,
                MoleculeBatchState::Deferred,
            )
            .await
        }
        [existing] => {
            let fingerprint = batch_fingerprint(findings);
            let mut description = existing.description.trim_end().to_owned();
            if !description.is_empty() {
                description.push_str("\n\n");
            }
            description.push_str(&molecule_batch_description(
                findings,
                MoleculeBatchState::Deferred,
            ));
            let labels = molecule_batch_labels(findings, MoleculeBatchState::Deferred);
            match bd
                .update(
                    &existing.id,
                    UpdateOpts {
                        status: Some("deferred".to_string()),
                        add_labels: labels,
                        description: Some(description),
                        ..UpdateOpts::default()
                    },
                )
                .await
            {
                Ok(()) => BatchOutcome::Minted {
                    fingerprint,
                    bead_id: existing.id.clone(),
                    lead_spec: lead_spec.clone(),
                    findings_count: findings.len(),
                },
                Err(err) => BatchOutcome::Errored {
                    fingerprint,
                    message: format!(
                        "bd update failed while merging deferred findings into `{}`: {err}",
                        existing.id,
                    ),
                },
            }
        }
        duplicates => BatchOutcome::Refused {
            fingerprint: batch_fingerprint(findings),
            reason: format!(
                "multiple deferred remediation beads for spec `{lead_spec}` in molecule (ids: {})",
                duplicates
                    .iter()
                    .map(|bead| bead.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        },
    }
}

async fn create_molecule_batch<R: CommandRunner>(
    bd: &BdClient<R>,
    findings: &[Finding],
    lead_spec: &SpecLabel,
    parent: &BeadId,
    state: MoleculeBatchState,
) -> BatchOutcome {
    let fingerprint = batch_fingerprint(findings);
    let notes = (state == MoleculeBatchState::BlockedClarifyWithoutOptions)
        .then(|| CLARIFY_WITHOUT_OPTIONS_CAUSE.to_string());
    let created = bd
        .create(CreateOpts {
            title: batch_title(findings, lead_spec),
            description: molecule_batch_description(findings, state),
            issue_type: Some("task".to_string()),
            labels: molecule_batch_labels(findings, state),
            parent: Some(parent.clone()),
            notes,
            ..CreateOpts::default()
        })
        .await;
    let bead_id = match created {
        Ok(bead_id) => bead_id,
        Err(err) => {
            return BatchOutcome::Errored {
                fingerprint,
                message: mint_error_message(&MintError::from(err)),
            };
        }
    };
    let molecule = MoleculeId::new(parent.as_str());
    if let Err(err) = bd.mol_bond(molecule.as_str(), bead_id.as_str()).await {
        return BatchOutcome::Errored {
            fingerprint,
            message: format!(
                "created routed bead `{bead_id}` but failed to bond it to molecule `{molecule}`: {err}",
            ),
        };
    }
    let parked_status = match state {
        MoleculeBatchState::Deferred => Some("deferred"),
        MoleculeBatchState::Clarify | MoleculeBatchState::BlockedClarifyWithoutOptions => {
            Some("blocked")
        }
        MoleculeBatchState::Ready => None,
    };
    if let Some(status) = parked_status
        && let Err(err) = bd
            .update(
                &bead_id,
                UpdateOpts {
                    status: Some(status.to_string()),
                    ..UpdateOpts::default()
                },
            )
            .await
    {
        return BatchOutcome::Errored {
            fingerprint,
            message: format!(
                "created routed bead `{bead_id}` but failed to set status `{status}`: {err}",
            ),
        };
    }
    BatchOutcome::Minted {
        fingerprint,
        bead_id,
        lead_spec: lead_spec.clone(),
        findings_count: findings.len(),
    }
}

fn molecule_batch_labels(findings: &[Finding], state: MoleculeBatchState) -> Vec<String> {
    let mut labels = findings.iter().map(finding_label).collect::<Vec<_>>();
    let mut specs = findings
        .iter()
        .flat_map(|finding| finding.bonds.iter())
        .map(|spec| format!("spec:{spec}"))
        .collect::<Vec<_>>();
    specs.sort();
    specs.dedup();
    labels.extend(specs);
    match state {
        MoleculeBatchState::Ready => {}
        MoleculeBatchState::Deferred => labels.push(DEFERRED_LABEL.to_string()),
        MoleculeBatchState::Clarify => labels.push("loom:clarify".to_string()),
        MoleculeBatchState::BlockedClarifyWithoutOptions => {
            labels.push("loom:blocked".to_string());
        }
    }
    labels
}

fn molecule_batch_description(findings: &[Finding], state: MoleculeBatchState) -> String {
    let mut description = String::new();
    if state == MoleculeBatchState::BlockedClarifyWithoutOptions {
        description.push_str(&format!("Cause: `{CLARIFY_WITHOUT_OPTIONS_CAUSE}`\n\n"));
    }
    if state == MoleculeBatchState::Clarify {
        description.push_str(findings[0].evidence.trim_end());
        description.push_str("\n\n---\n\n");
    }
    append_findings_section(&mut description, findings);
    description
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TreeMintPlan {
    batches: Vec<TreeMintBatch>,
}

impl TreeMintPlan {
    #[must_use]
    pub fn batches(&self) -> &[TreeMintBatch] {
        &self.batches
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeMintBatch {
    pub fingerprint: String,
    pub lead_spec: SpecLabel,
    pub findings: Vec<Finding>,
    pub routing: TreeMintBatchRouting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeMintBatchRouting {
    Fixup,
    Clarify,
    BlockedClarifyWithoutOptions,
}

impl From<FindingRouting> for TreeMintBatchRouting {
    fn from(value: FindingRouting) -> Self {
        match value {
            FindingRouting::Fixup => Self::Fixup,
            FindingRouting::Clarify => Self::Clarify,
            FindingRouting::BlockedClarifyWithoutOptions => Self::BlockedClarifyWithoutOptions,
        }
    }
}

impl From<TreeMintBatchRouting> for FindingRouting {
    fn from(value: TreeMintBatchRouting) -> Self {
        match value {
            TreeMintBatchRouting::Fixup => Self::Fixup,
            TreeMintBatchRouting::Clarify => Self::Clarify,
            TreeMintBatchRouting::BlockedClarifyWithoutOptions => {
                Self::BlockedClarifyWithoutOptions
            }
        }
    }
}

pub async fn mint_tree_findings_with_options<R: CommandRunner>(
    bd: &BdClient<R>,
    findings: &[Finding],
    head_commit: &str,
    opts: &MintOptions,
) -> MintSummary {
    let (plan, summary) = plan_tree_mint_with_options(bd, findings, opts).await;
    materialize_tree_plan(bd, plan, head_commit, opts, summary).await
}

async fn materialize_tree_plan<R: CommandRunner>(
    bd: &BdClient<R>,
    plan: TreeMintPlan,
    head_commit: &str,
    opts: &MintOptions,
    mut summary: MintSummary,
) -> MintSummary {
    if plan.is_empty() {
        return summary;
    }
    if opts.dry_run {
        record_dry_run_tree_plan(plan, &mut summary);
        return summary;
    }

    let active_before = match active_work_epics(bd).await {
        Ok(active) => active,
        Err(err) => {
            summary.record(BatchOutcome::Errored {
                fingerprint: "tree-active".to_owned(),
                message: mint_error_message(&err),
            });
            return summary;
        }
    };
    let epic = match create_tree_remediation_epic(bd, &plan, head_commit).await {
        Ok(epic) => epic,
        Err(err) => {
            summary.record(BatchOutcome::Errored {
                fingerprint: "tree-epic".to_owned(),
                message: mint_error_message(&err),
            });
            return summary;
        }
    };

    let mut minted_specs = HashSet::new();
    let mut created_children = 0_usize;
    let mut active_applied = false;
    for batch in plan.batches {
        let routing = FindingRouting::from(batch.routing);
        let outcome =
            create_batch_under_parent(bd, &batch.findings, &batch.lead_spec, &epic, routing).await;
        let child_created = matches!(outcome, BatchOutcome::Minted { .. });
        record_batch_status(&mut summary, &batch.findings, &outcome);
        track_minted(&outcome, &batch.lead_spec, &mut summary, &mut minted_specs);
        summary.record(outcome);

        if child_created {
            created_children += 1;
            if !active_applied {
                match activate_tree_remediation_epic(bd, &epic, &active_before).await {
                    Ok(()) => {
                        summary.active_epic = Some(epic.clone());
                        active_applied = true;
                    }
                    Err(err) => {
                        summary.record(BatchOutcome::Errored {
                            fingerprint: format!("active:{epic}"),
                            message: mint_error_message(&err),
                        });
                        break;
                    }
                }
            }
        } else if created_children == 0 {
            neutralize_empty_tree_epic(bd, &epic, &mut summary).await;
            break;
        }
    }
    summary.specs_across_minted = minted_specs.len();
    summary
}

fn record_dry_run_tree_plan(plan: TreeMintPlan, summary: &mut MintSummary) {
    for batch in plan.batches {
        let outcome = BatchOutcome::WouldMint {
            fingerprint: batch.fingerprint,
            lead_spec: batch.lead_spec,
            findings_count: batch.findings.len(),
        };
        record_batch_status(summary, &batch.findings, &outcome);
        summary.record(outcome);
    }
}

async fn active_work_epics<R: CommandRunner>(bd: &BdClient<R>) -> Result<Vec<Bead>, MintError> {
    let beads = bd
        .list(ListOpts {
            issue_type: Some("epic".to_string()),
            label: Some(ACTIVE_LABEL.to_string()),
            status: Some("open".to_string()),
            ..ListOpts::default()
        })
        .await?;
    Ok(beads
        .into_iter()
        .filter(|bead| bead.labels.iter().any(Label::is_active))
        .collect())
}

async fn create_tree_remediation_epic<R: CommandRunner>(
    bd: &BdClient<R>,
    plan: &TreeMintPlan,
    head_commit: &str,
) -> Result<BeadId, MintError> {
    let batch_count = plan.batches.len();
    let finding_count = plan
        .batches
        .iter()
        .map(|batch| batch.findings.len())
        .sum::<usize>();
    let metadata = serde_json::json!({ "loom.base_commit": head_commit }).to_string();
    Ok(bd
        .create(CreateOpts {
            title: cap_bd_title(format!(
                "tree remediation: {batch_count} batches ({finding_count} findings)",
            )),
            description: tree_remediation_epic_description(batch_count, finding_count),
            issue_type: Some("epic".to_string()),
            priority: Some(2),
            labels: tree_remediation_epic_labels(plan),
            metadata: Some(metadata),
            ..CreateOpts::default()
        })
        .await?)
}

fn tree_remediation_epic_description(batch_count: usize, finding_count: usize) -> String {
    format!(
        "Standing remediation work epic for `loom gate mint --tree`.\n\nChild batches: {batch_count}\nFindings: {finding_count}\n\nRun `loom loop` to process this active remediation epic.\n",
    )
}

fn tree_remediation_epic_labels(plan: &TreeMintPlan) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut labels = Vec::new();
    for batch in &plan.batches {
        for finding in &batch.findings {
            for spec in &finding.bonds {
                if seen.insert(spec.as_str().to_owned()) {
                    labels.push(format!("spec:{spec}"));
                }
            }
        }
    }
    labels
}

async fn activate_tree_remediation_epic<R: CommandRunner>(
    bd: &BdClient<R>,
    epic: &BeadId,
    active_before: &[Bead],
) -> Result<(), MintError> {
    bd.update(
        epic,
        UpdateOpts {
            add_labels: vec![ACTIVE_LABEL.to_string()],
            ..UpdateOpts::default()
        },
    )
    .await?;
    for active in active_before {
        if &active.id == epic {
            continue;
        }
        bd.update(
            &active.id,
            UpdateOpts {
                remove_labels: vec![ACTIVE_LABEL.to_string()],
                ..UpdateOpts::default()
            },
        )
        .await?;
    }
    Ok(())
}

async fn neutralize_empty_tree_epic<R: CommandRunner>(
    bd: &BdClient<R>,
    epic: &BeadId,
    summary: &mut MintSummary,
) {
    if let Err(err) = bd.close(epic, Some(EMPTY_TREE_EPIC_CLOSE_REASON)).await {
        summary.record(BatchOutcome::Errored {
            fingerprint: format!("tree-epic:{epic}"),
            message: format!(
                "failed to close empty tree remediation epic `{epic}`: {}",
                mint_error_message(&MintError::from(err)),
            ),
        });
    }
}

pub async fn plan_tree_mint_with_options<R: CommandRunner>(
    bd: &BdClient<R>,
    findings: &[Finding],
    opts: &MintOptions,
) -> (TreeMintPlan, MintSummary) {
    let mut summary = MintSummary::default();
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
                return (TreeMintPlan::default(), summary);
            }
        } else {
            ids_by_hash.insert(finding_hash.clone(), finding_id);
        }
        fingerprints.push((finding, finding_hash));
    }

    let mut survivors = Vec::new();
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
            FindingDedup::Untracked | FindingDedup::Closed(_) => {}
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
                return (TreeMintPlan::default(), summary);
            }
            FindingDedup::Errored { message } => {
                summary.record_status(finding, FindingStatusAction::Refused);
                summary.record(BatchOutcome::Errored {
                    fingerprint: finding_hash,
                    message,
                });
                return (TreeMintPlan::default(), summary);
            }
        }
        survivors.push(finding.clone());
    }

    if survivors.is_empty() {
        if opts.report_stale {
            report_stale_candidates(bd, &mut summary, &current_hashes).await;
        }
        return (TreeMintPlan::default(), summary);
    }

    if let Err(outcome) = ensure_tree_spec_metadata(bd, &survivors, opts.dry_run).await {
        summary.record(outcome);
        if opts.report_stale {
            report_stale_candidates(bd, &mut summary, &current_hashes).await;
        }
        return (TreeMintPlan::default(), summary);
    }

    let batches = tree_plan_batches(survivors);
    if opts.report_stale {
        report_stale_candidates(bd, &mut summary, &current_hashes).await;
    }
    (TreeMintPlan { batches }, summary)
}

async fn ensure_tree_spec_metadata<R: CommandRunner>(
    bd: &BdClient<R>,
    findings: &[Finding],
    dry_run: bool,
) -> Result<(), BatchOutcome> {
    let mut seen = HashSet::new();
    let mut specs = Vec::new();
    for finding in findings {
        for spec in &finding.bonds {
            if seen.insert(spec.as_str().to_owned()) {
                specs.push(spec.clone());
            }
        }
    }
    for spec in specs {
        match ensure_spec_metadata_epic(bd, &spec, dry_run).await {
            Ok(_) => {}
            Err(ResolveError::DuplicateSpecEpics { label, ids }) => {
                return Err(BatchOutcome::Refused {
                    fingerprint: format!("spec:{label}"),
                    reason: format!(
                        "duplicate loom:spec epics for spec `{label}` — close or relabel all but one before re-running (ids: {ids})",
                    ),
                });
            }
            Err(err) => {
                let err = MintError::Resolve(err);
                return Err(BatchOutcome::Errored {
                    fingerprint: format!("spec:{spec}"),
                    message: mint_error_message(&err),
                });
            }
        }
    }
    Ok(())
}

fn tree_plan_batches(findings: Vec<Finding>) -> Vec<TreeMintBatch> {
    let mut by_spec: Vec<(SpecLabel, Vec<Finding>)> = Vec::new();
    for finding in findings {
        let Some(lead_spec) = finding.bonds.first().cloned() else {
            continue;
        };
        if let Some(slot) = by_spec.iter_mut().find(|(spec, _)| *spec == lead_spec) {
            slot.1.push(finding);
        } else {
            by_spec.push((lead_spec, vec![finding]));
        }
    }
    by_spec.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    let mut batches = Vec::new();
    for (lead_spec, group) in by_spec {
        let (fix_up, clarifies) = partition_group(group);
        if !fix_up.is_empty() {
            batches.push(TreeMintBatch {
                fingerprint: batch_fingerprint(&fix_up),
                lead_spec: lead_spec.clone(),
                findings: fix_up,
                routing: TreeMintBatchRouting::Fixup,
            });
        }
        for (finding, routing) in clarifies {
            batches.push(TreeMintBatch {
                fingerprint: batch_fingerprint(std::slice::from_ref(&finding)),
                lead_spec: lead_spec.clone(),
                findings: vec![finding],
                routing: routing.into(),
            });
        }
    }
    batches
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
            Ok(()) => {
                summary.ready_remediation_batches += 1;
                summary.record(BatchOutcome::PromotedDeferred {
                    bead_id: bead.id,
                    findings_count,
                });
            }
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
        report_stale_candidates(bd, &mut summary, &current_hashes).await;
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
) {
    let beads = match bd
        .list(ListOpts {
            status: Some(DEDUP_STATUSES.to_string()),
            ..ListOpts::default()
        })
        .await
    {
        Ok(beads) => beads,
        Err(err) => {
            let err = MintError::from(err);
            summary.record(BatchOutcome::Errored {
                fingerprint: "stale:tree".to_owned(),
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
        BatchOutcome::Planned { .. } | BatchOutcome::WouldMint { .. } => {
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

    if opts.dry_run {
        return BatchOutcome::WouldMint {
            fingerprint,
            lead_spec: lead_spec.clone(),
            findings_count: findings.len(),
        };
    }

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
    create_batch_under_parent(bd, findings, lead_spec, &parent, routing).await
}

async fn create_batch_under_parent<R: CommandRunner>(
    bd: &BdClient<R>,
    findings: &[Finding],
    lead_spec: &SpecLabel,
    parent: &BeadId,
    routing: FindingRouting,
) -> BatchOutcome {
    let fingerprint = batch_fingerprint(findings);
    let label = mint_label(&fingerprint);
    let labels = batch_labels(findings, &label, routing);
    let title = batch_title(findings, lead_spec);
    let description = batch_description(findings, &fingerprint, routing);
    let notes = matches!(routing, FindingRouting::BlockedClarifyWithoutOptions)
        .then(|| CLARIFY_WITHOUT_OPTIONS_CAUSE.to_string());
    match bd
        .create(CreateOpts {
            title,
            description,
            issue_type: Some("task".to_string()),
            labels,
            parent: Some(parent.clone()),
            notes,
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

    #[derive(Debug)]
    struct StatefulBdState {
        beads: Vec<Bead>,
        bonds: Vec<(MoleculeId, BeadId)>,
        next_child: u32,
        duplicate_on_children_query: Option<Finding>,
    }

    #[derive(Clone)]
    struct StatefulBdRunner {
        state: Arc<Mutex<StatefulBdState>>,
    }

    impl StatefulBdRunner {
        fn molecule() -> Self {
            let beads = vec![Bead {
                id: BeadId::new("lm-mol").expect("molecule id"),
                title: "molecule".to_string(),
                description: String::new(),
                status: "open".to_string(),
                priority: 2,
                issue_type: "epic".to_string(),
                labels: vec![Label::new("spec:agent")],
                parent: None,
                metadata: BTreeMap::new(),
                notes: None,
            }];
            Self {
                state: Arc::new(Mutex::new(StatefulBdState {
                    beads,
                    bonds: Vec::new(),
                    next_child: 1,
                    duplicate_on_children_query: None,
                })),
            }
        }

        fn beads(&self) -> Vec<Bead> {
            self.state.lock().expect("state lock").beads.clone()
        }

        fn bonds(&self) -> Vec<(MoleculeId, BeadId)> {
            self.state.lock().expect("state lock").bonds.clone()
        }

        fn inject_duplicate_on_children_query(&self, finding: Finding) {
            self.state
                .lock()
                .expect("state lock")
                .duplicate_on_children_query = Some(finding);
        }
    }

    impl CommandRunner for StatefulBdRunner {
        async fn run(&self, args: Vec<OsString>, _t: Duration) -> Result<RunOutput, BdError> {
            let argv = args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            let mut state = self.state.lock().expect("state lock");
            match argv.first().map(String::as_str) {
                Some("show") => {
                    let id = argv.get(1).map(String::as_str).unwrap_or_default();
                    let rows = state
                        .beads
                        .iter()
                        .filter(|bead| bead.id.as_str() == id)
                        .cloned()
                        .collect::<Vec<_>>();
                    Ok(json_output(&rows))
                }
                Some("list") => {
                    if argv.iter().any(|arg| arg == "--parent=lm-mol")
                        && let Some(finding) = state.duplicate_on_children_query.take()
                    {
                        inject_duplicate_children(&mut state.beads, &finding);
                    }
                    let rows = filter_stateful_beads(&state.beads, &argv, false);
                    Ok(json_output(&rows))
                }
                Some("ready") => {
                    let rows = filter_stateful_beads(&state.beads, &argv, true);
                    Ok(json_output(&rows))
                }
                Some("create") => {
                    let id = BeadId::new(&format!("lm-mol.{}", state.next_child))?;
                    state.next_child += 1;
                    let labels = stateful_flag(&argv, "--labels")
                        .map(|value| value.split(',').map(Label::new).collect())
                        .unwrap_or_default();
                    let parent = stateful_flag(&argv, "--parent")
                        .map(BeadId::new)
                        .transpose()?;
                    let bead = Bead {
                        id: id.clone(),
                        title: stateful_flag(&argv, "--title")
                            .unwrap_or_default()
                            .to_string(),
                        description: stateful_flag(&argv, "--description")
                            .unwrap_or_default()
                            .to_string(),
                        status: "open".to_string(),
                        priority: 2,
                        issue_type: stateful_flag(&argv, "--type").unwrap_or("task").to_string(),
                        labels,
                        parent,
                        metadata: BTreeMap::new(),
                        notes: stateful_flag(&argv, "--notes").map(str::to_string),
                    };
                    state.beads.push(bead);
                    Ok(ok_stdout(&format!("{id}\n")))
                }
                Some("update") => {
                    let id = BeadId::new(argv.get(1).map(String::as_str).unwrap_or_default())?;
                    let bead = state
                        .beads
                        .iter_mut()
                        .find(|bead| bead.id == id)
                        .expect("updated bead exists");
                    apply_stateful_update(bead, &argv);
                    Ok(ok_stdout(""))
                }
                Some("mol") if argv.get(1).is_some_and(|arg| arg == "bond") => {
                    let molecule =
                        MoleculeId::new(argv.get(2).map(String::as_str).unwrap_or_default());
                    let bead = BeadId::new(argv.get(3).map(String::as_str).unwrap_or_default())?;
                    state.bonds.push((molecule, bead));
                    Ok(ok_stdout(""))
                }
                other => Ok(RunOutput {
                    status: 2,
                    stdout: Vec::new(),
                    stderr: format!("unsupported stateful bd command: {other:?}").into_bytes(),
                }),
            }
        }
    }

    fn inject_duplicate_children(beads: &mut Vec<Bead>, finding: &Finding) {
        for index in 1..=2 {
            beads.push(Bead {
                id: BeadId::new(&format!("lm-injected.{index}")).expect("injected child id"),
                title: "injected duplicate".to_string(),
                description: molecule_batch_description(
                    std::slice::from_ref(finding),
                    MoleculeBatchState::Ready,
                ),
                status: "open".to_string(),
                priority: 2,
                issue_type: "task".to_string(),
                labels: molecule_batch_labels(
                    std::slice::from_ref(finding),
                    MoleculeBatchState::Ready,
                )
                .into_iter()
                .map(Label::new)
                .collect(),
                parent: Some(BeadId::new("lm-mol").expect("molecule id")),
                metadata: BTreeMap::new(),
                notes: None,
            });
        }
    }

    fn stateful_flag<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
        argv.iter()
            .position(|arg| arg == flag)
            .and_then(|index| argv.get(index + 1))
            .map(String::as_str)
    }

    fn filter_stateful_beads(beads: &[Bead], argv: &[String], ready: bool) -> Vec<Bead> {
        let status = argv.iter().find_map(|arg| arg.strip_prefix("--status="));
        let label = argv.iter().find_map(|arg| arg.strip_prefix("--label="));
        let parent = argv.iter().find_map(|arg| arg.strip_prefix("--parent="));
        let issue_type = argv.iter().find_map(|arg| arg.strip_prefix("--type="));
        beads
            .iter()
            .filter(|bead| {
                (!ready || bead.status == "open")
                    && status.is_none_or(|statuses| statuses.split(',').any(|s| s == bead.status))
                    && label.is_none_or(|wanted| {
                        bead.labels
                            .iter()
                            .any(|candidate| candidate.as_str() == wanted)
                    })
                    && parent.is_none_or(|wanted| {
                        bead.parent
                            .as_ref()
                            .is_some_and(|candidate| candidate.as_str() == wanted)
                    })
                    && issue_type.is_none_or(|wanted| bead.issue_type == wanted)
            })
            .cloned()
            .collect()
    }

    fn apply_stateful_update(bead: &mut Bead, argv: &[String]) {
        let mut index = 2;
        while index < argv.len() {
            match argv[index].as_str() {
                "--status" => bead.status = argv[index + 1].clone(),
                "--add-label" => {
                    let label = Label::new(&argv[index + 1]);
                    if !bead.labels.contains(&label) {
                        bead.labels.push(label);
                    }
                }
                "--remove-label" => {
                    bead.labels
                        .retain(|label| label.as_str() != argv[index + 1]);
                }
                "--description" => bead.description = argv[index + 1].clone(),
                "--notes" => bead.notes = Some(argv[index + 1].clone()),
                _ => {
                    index += 1;
                    continue;
                }
            }
            index += 2;
        }
    }

    fn json_output<T: Serialize>(value: &T) -> RunOutput {
        RunOutput {
            status: 0,
            stdout: serde_json::to_vec(value).expect("serialize stateful bd response"),
            stderr: Vec::new(),
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

    fn spec_epic_row(id: &str, label: &str, status: &str) -> String {
        format!(
            r#"{{
                "id": "{id}",
                "title": "loom spec: {label}",
                "status": "{status}",
                "priority": 2,
                "issue_type": "epic",
                "labels": ["loom:spec", "spec:{label}"]
            }}"#,
        )
    }

    fn spec_epic_list(id: &str, label: &str, status: &str) -> String {
        format!("[{}]", spec_epic_row(id, label, status))
    }

    fn active_epic_row(id: &str, labels: &[&str]) -> String {
        let labels_json = labels
            .iter()
            .map(|label| format!("{label:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            r#"{{
                "id": "{id}",
                "title": "active work",
                "status": "open",
                "priority": 2,
                "issue_type": "epic",
                "labels": [{labels_json}]
            }}"#,
        )
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
    async fn stateful_bd_runner_conforms_to_routing_command_contract() {
        let runner = StatefulBdRunner::molecule();
        let state = runner.clone();
        let bd = BdClient::with_runner(runner);
        let parent = BeadId::new("lm-mol").expect("molecule id");

        let child = bd
            .create(CreateOpts {
                title: "deferred remediation".to_string(),
                description: "finding evidence".to_string(),
                issue_type: Some("task".to_string()),
                labels: vec![DEFERRED_LABEL.to_string()],
                parent: Some(parent.clone()),
                ..CreateOpts::default()
            })
            .await
            .expect("create child");
        bd.mol_bond("lm-mol", child.as_str())
            .await
            .expect("bond child");
        bd.update(
            &child,
            UpdateOpts {
                status: Some("deferred".to_string()),
                ..UpdateOpts::default()
            },
        )
        .await
        .expect("park child");

        let children = bd
            .list(ListOpts {
                status: Some("deferred".to_string()),
                parent: Some(parent.clone()),
                ..ListOpts::default()
            })
            .await
            .expect("list children");
        let ready = bd
            .ready(loom_driver::bd::ReadyOpts {
                parent: Some(parent),
                ..loom_driver::bd::ReadyOpts::default()
            })
            .await
            .expect("query ready");

        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, child);
        assert!(ready.is_empty());
        assert_eq!(state.bonds(), vec![(MoleculeId::new("lm-mol"), child)]);
    }

    #[tokio::test]
    async fn molecule_review_clarify_finding_creates_blocked_clarify_bead() {
        let clarify = invariant_clash_finding(
            vec![spec("agent")],
            spec("agent"),
            "Architecture",
            "routing-choice",
            "## Options — choose routing\n\n### Option 1 — keep molecule-local\nCost: coupling.\n",
        );
        let runner = StatefulBdRunner::molecule();
        let state = runner.clone();
        let bd = BdClient::with_runner(runner);

        let summary = route_molecule_findings(
            &bd,
            &MoleculeId::new("lm-mol"),
            std::slice::from_ref(&clarify),
            &MintOptions::default(),
        )
        .await;

        assert_eq!(summary.clarify_findings_raised, 1);
        let child = state
            .beads()
            .into_iter()
            .find(|bead| bead.id.as_str() != "lm-mol")
            .expect("clarify bead created");
        assert_eq!(child.status, "blocked");
        assert!(child.labels.iter().any(Label::is_clarify));
        assert!(child.description.contains("## Options — choose routing"));
    }

    #[tokio::test]
    async fn molecule_review_blocking_finding_creates_same_molecule_remediation() {
        let mut finding = contract_finding(
            vec![spec("agent")],
            "blocking-acceptance-gap",
            "pushed behavior is incomplete",
        );
        finding.route = FindingRoute::Blocking;
        let runner = StatefulBdRunner::molecule();
        let state = runner.clone();
        let bd = BdClient::with_runner(runner);

        let summary = route_molecule_findings(
            &bd,
            &MoleculeId::new("lm-mol"),
            std::slice::from_ref(&finding),
            &MintOptions::default(),
        )
        .await;

        assert_eq!(summary.blocking_findings, 1);
        assert_eq!(summary.ready_remediation_batches, 1);
        let remediation = state
            .beads()
            .into_iter()
            .find(|bead| bead.id.as_str() != "lm-mol")
            .expect("remediation created");
        assert_eq!(
            remediation.parent.as_ref().map(BeadId::as_str),
            Some("lm-mol")
        );
        assert_eq!(remediation.status, "open");
        assert!(
            remediation
                .labels
                .iter()
                .any(|label| { label.as_str() == finding_label(&finding) })
        );
        assert!(!remediation.labels.iter().any(Label::is_deferred));
        assert_eq!(
            state.bonds(),
            vec![(MoleculeId::new("lm-mol"), remediation.id)],
        );
    }

    #[tokio::test]
    async fn molecule_review_deferred_finding_creates_deferred_bead() {
        let finding = contract_finding(
            vec![spec("agent")],
            "deferred-adjacent-drift",
            "outside the pushed acceptance surface",
        );
        let runner = StatefulBdRunner::molecule();
        let state = runner.clone();
        let bd = BdClient::with_runner(runner);

        let summary = route_molecule_findings(
            &bd,
            &MoleculeId::new("lm-mol"),
            std::slice::from_ref(&finding),
            &MintOptions::default(),
        )
        .await;
        let ready = bd
            .ready(loom_driver::bd::ReadyOpts {
                parent: Some(BeadId::new("lm-mol").expect("molecule id")),
                ..loom_driver::bd::ReadyOpts::default()
            })
            .await
            .expect("ready query");

        assert_eq!(summary.deferred_findings_merged, 1);
        assert!(ready.is_empty(), "deferred remediation must not be ready");
        let remediation = state
            .beads()
            .into_iter()
            .find(|bead| bead.id.as_str() != "lm-mol")
            .expect("deferred remediation created");
        assert_eq!(remediation.status, "deferred");
        assert!(remediation.labels.iter().any(Label::is_deferred));
        assert_eq!(
            remediation.parent.as_ref().map(BeadId::as_str),
            Some("lm-mol")
        );
        assert_eq!(
            state.bonds(),
            vec![(MoleculeId::new("lm-mol"), remediation.id)],
        );
    }

    #[tokio::test]
    async fn molecule_routes_gate_routing_structural_conflict_to_blocked() {
        let finding = contract_finding(
            vec![spec("agent")],
            "duplicate-structural-finding",
            "duplicate injected for conflict",
        );
        let runner = StatefulBdRunner::molecule();
        runner.inject_duplicate_on_children_query(finding.clone());
        let state = runner.clone();
        let bd = BdClient::with_runner(runner);

        let summary = route_molecule_findings(
            &bd,
            &MoleculeId::new("lm-mol"),
            std::slice::from_ref(&finding),
            &MintOptions::default(),
        )
        .await;

        assert_eq!(summary.refused, 1);
        let molecule = state
            .beads()
            .into_iter()
            .find(|bead| bead.id.as_str() == "lm-mol")
            .expect("molecule remains");
        assert_eq!(molecule.status, "blocked");
        assert!(molecule.labels.iter().any(Label::is_blocked));
        assert!(
            molecule
                .notes
                .as_deref()
                .is_some_and(|notes| notes.contains(GATE_ROUTING_STRUCTURAL_VIOLATION_CAUSE))
        );
    }

    #[test]
    fn mint_end_of_run_summary_reports_finding_lifecycle_outcomes() {
        let summary = MintSummary {
            blocking_findings: 2,
            deferred_findings_merged: 3,
            promoted_deferred: 1,
            ready_remediation_batches: 2,
            clarify_findings_raised: 1,
            skipped: 4,
            suppressed: 5,
            stale_candidates: 6,
            partial_stale_candidates: 7,
            refused: 8,
            errors: 9,
            ..MintSummary::default()
        };

        let rendered = summary.render();
        for expected in [
            "blocking 2",
            "deferred merged 3",
            "deferred promoted 1",
            "ready remediation batches 2",
            "clarify raised 1",
            "skipped live 4",
            "suppressed 5",
            "stale 6",
            "partially stale 7",
            "structural conflicts 8",
            "transient errors 9",
        ] {
            assert!(
                rendered.contains(expected),
                "missing `{expected}`: {rendered}"
            );
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
        assert!(summary.render().contains("deferred promoted 1"));
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
            suppressions: Vec::new(),
            suppress_closed_same_molecule: false,
            report_stale: true,
        };
        let summary = mint_findings_with_options(&bd, &[finding], "head-sha", &opts).await;

        assert_eq!(summary.stale_candidates, 1);
        assert!(summary.render().contains("stale-candidate lm-stale.1"));
        let calls = rendered_calls(&invocations);
        assert!(
            calls.iter().any(|call| call
                .iter()
                .any(|arg| arg == &format!("--status={DEDUP_STATUSES}"))),
            "stale reporting must list live remediation beads tree-wide: {calls:?}",
        );
        assert!(
            calls.iter().any(|call| {
                call.iter()
                    .any(|arg| arg == &format!("--status={DEDUP_STATUSES}"))
                    && !call.iter().any(|arg| arg.starts_with("--label=spec:"))
            }),
            "tree stale-reporting list must not narrow by spec label: {calls:?}",
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

    #[tokio::test]
    async fn mint_tree_scope_resolves_bonded_spec_epics_without_per_spec_work_epics() {
        let finding = contract_finding(vec![spec("alpha"), spec("beta")], "x", "evidence");
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&spec_epic_list("lm-alphaspec", "alpha", "closed")),
            ok_stdout("[]"),
            ok_stdout("lm-betaspec\n"),
            ok_stdout(""),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let (plan, summary) =
            plan_tree_mint_with_options(&bd, &[finding], &MintOptions::default()).await;

        assert_eq!(
            plan.batches().len(),
            1,
            "one actionable tree batch is planned"
        );
        assert_eq!(summary.minted, 0, "planning does not create child work");
        assert_eq!(plan.batches()[0].lead_spec.as_str(), "alpha");
        let calls = rendered_calls(&invocations);
        assert!(
            calls
                .iter()
                .any(|call| call.iter().any(|arg| arg == "loom:spec,spec:beta")),
            "missing metadata epic is created with loom:spec + spec label: {calls:?}",
        );
        let expected_close = vec![
            "close".to_string(),
            "lm-betaspec".to_string(),
            "--reason".to_string(),
            "spec metadata carrier".to_string(),
        ];
        assert!(
            calls.iter().any(|call| call == &expected_close),
            "driver-created metadata epic is immediately closed: {calls:?}",
        );
        assert!(
            calls
                .iter()
                .all(|call| !call.iter().any(|arg| arg == "--parent")),
            "tree planning must not select a per-spec parent work epic: {calls:?}",
        );
        assert!(
            calls.iter().all(|call| {
                !(call.iter().any(|arg| arg == "create")
                    && call
                        .windows(2)
                        .any(|pair| pair[0] == "--type" && pair[1] == "task"))
            }),
            "tree planning creates metadata only, not child tasks: {calls:?}",
        );
    }

    #[tokio::test]
    async fn mint_tree_scope_duplicate_metadata_epics_refuse_before_child_work() {
        let finding = coherence_finding(vec![spec("alpha")], "x", "evidence");
        let duplicate = format!(
            "[{},{}]",
            spec_epic_row("lm-alphaa", "alpha", "open"),
            spec_epic_row("lm-alphab", "alpha", "closed"),
        );
        let runner = ScriptedRunner::new(vec![ok_stdout("[]"), ok_stdout(&duplicate)]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);

        let summary =
            mint_tree_findings_with_options(&bd, &[finding], "head-sha", &MintOptions::default())
                .await;

        assert_eq!(summary.refused, 1, "duplicate metadata epics refuse");
        assert_eq!(summary.planned, 0, "no actionable plan survives refusal");
        assert!(
            summary.render().contains("lm-alphaa") && summary.render().contains("lm-alphab"),
            "summary names conflicting metadata epics: {}",
            summary.render(),
        );
        let calls = rendered_calls(&invocations);
        assert!(
            calls
                .iter()
                .all(|call| !call.iter().any(|arg| arg == "create")),
            "refusal happens before remediation child creation: {calls:?}",
        );
    }

    #[tokio::test]
    async fn mint_tree_scope_without_actionable_findings_creates_no_epic() {
        let finding = coherence_finding(vec![spec("alpha")], "x", "evidence");
        let hash = finding.hash();
        let runner = ScriptedRunner::new(vec![ok_stdout(&format!(
            "[{}]",
            fixup_row("lm-existing.1", &hash),
        ))]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);

        let summary =
            mint_tree_findings_with_options(&bd, &[finding], "head-sha", &MintOptions::default())
                .await;

        assert_eq!(summary.skipped, 1, "live finding dedups");
        assert_eq!(summary.planned, 0, "deduped findings are not actionable");
        assert_eq!(summary.minted, 0, "no remediation work is created");
        let calls = rendered_calls(&invocations);
        assert_eq!(calls.len(), 1, "only the live dedup query runs: {calls:?}");
        assert!(
            calls
                .iter()
                .all(|call| !call.iter().any(|arg| arg == "create")),
            "no work or metadata epic is created for non-actionable findings: {calls:?}",
        );
    }

    #[derive(Debug)]
    struct MixedTreeRun {
        summary: MintSummary,
        calls: Vec<Vec<String>>,
        fix_label: String,
        clarify_label: String,
        blocked_label: String,
    }

    async fn run_mixed_tree_materialization() -> MixedTreeRun {
        let fix = contract_finding(
            vec![spec("gate"), spec("harness")],
            "cross-spec-contract",
            "fix evidence",
        );
        let clarify = invariant_clash_finding(
            vec![spec("harness")],
            spec("harness"),
            "Out of Scope",
            "clarify-it",
            "## Options — choose\n\n### Option 1 — fix\nfix it.\n",
        );
        let blocked = invariant_clash_finding(
            vec![spec("agent")],
            spec("agent"),
            "Out of Scope",
            "blocked-it",
            "missing options",
        );
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout("[]"),
            ok_stdout("[]"),
            ok_stdout(&spec_epic_list("lm-gatespec", "gate", "closed")),
            ok_stdout(&spec_epic_list("lm-harnessspec", "harness", "closed")),
            ok_stdout(&spec_epic_list("lm-agentspec", "agent", "closed")),
            ok_stdout(&format!(
                "[{}]",
                active_epic_row("lm-prev", &[ACTIVE_LABEL, "spec:old"]),
            )),
            ok_stdout("lm-tree\n"),
            ok_stdout("lm-tree.1\n"),
            ok_stdout(""),
            ok_stdout(""),
            ok_stdout("lm-tree.2\n"),
            ok_stdout("lm-tree.3\n"),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let summary = mint_tree_findings_with_options(
            &bd,
            &[fix.clone(), clarify.clone(), blocked.clone()],
            "head-sha",
            &MintOptions::default(),
        )
        .await;
        MixedTreeRun {
            summary,
            calls: rendered_calls(&invocations),
            fix_label: finding_label(&fix),
            clarify_label: finding_label(&clarify),
            blocked_label: finding_label(&blocked),
        }
    }

    #[tokio::test]
    async fn mint_tree_scope_mints_single_active_work_epic_for_all_actionable_batches() {
        let run = run_mixed_tree_materialization().await;

        assert_eq!(run.summary.minted, 3, "mixed children mint: {run:?}");
        assert_eq!(
            run.summary.active_epic.as_ref().map(BeadId::as_str),
            Some("lm-tree"),
        );
        let render = run.summary.render();
        assert!(
            render.contains("active remediation epic: lm-tree") && render.contains("loom loop"),
            "summary names active epic and follow-up command: {render}",
        );

        let epic_create = run
            .calls
            .iter()
            .find(|call| {
                call.iter().any(|arg| arg == "create")
                    && call
                        .windows(2)
                        .any(|pair| pair[0] == "--type" && pair[1] == "epic")
            })
            .expect("standing remediation epic create recorded");
        let epic_labels = labels_arg(epic_create);
        for label in ["spec:agent", "spec:gate", "spec:harness"] {
            assert!(epic_labels.contains(&label), "epic labels: {epic_labels:?}");
        }
        assert!(
            !epic_labels.contains(&ACTIVE_LABEL),
            "active is applied only after a child exists: {epic_labels:?}",
        );
        let metadata: serde_json::Value =
            serde_json::from_str(flag_arg(epic_create, "--metadata")).expect("metadata json");
        assert_eq!(metadata["loom.base_commit"], "head-sha");

        assert!(
            run.calls.iter().any(|call| call
                == &[
                    "update".to_string(),
                    "lm-tree".to_string(),
                    "--add-label".to_string(),
                    ACTIVE_LABEL.to_string(),
                ]),
            "new remediation epic is activated after child creation: {:?}",
            run.calls,
        );
        assert!(
            run.calls.iter().any(|call| call
                == &[
                    "update".to_string(),
                    "lm-prev".to_string(),
                    "--remove-label".to_string(),
                    ACTIVE_LABEL.to_string(),
                ]),
            "previous active epic is cleared: {:?}",
            run.calls,
        );
        assert!(
            run.calls.iter().flatten().all(|arg| {
                !arg.contains("current_spec")
                    && !arg.contains("current-spec")
                    && !arg.contains("loom.current")
            }),
            "tree mint must not write current-spec or pointer-table state: {:?}",
            run.calls,
        );
    }

    #[tokio::test]
    async fn mint_tree_sets_single_active_remediation_work_epic() {
        let run = run_mixed_tree_materialization().await;
        let task_creates = run
            .calls
            .iter()
            .filter(|call| {
                call.iter().any(|arg| arg == "create")
                    && call
                        .windows(2)
                        .any(|pair| pair[0] == "--type" && pair[1] == "task")
            })
            .collect::<Vec<_>>();

        assert_eq!(task_creates.len(), 3, "all actionable children mint");
        assert_eq!(
            run.summary.active_epic.as_ref().map(BeadId::as_str),
            Some("lm-tree"),
        );
        assert!(
            task_creates
                .iter()
                .all(|call| flag_arg(call, "--parent") == "lm-tree"),
            "every child is under the active remediation epic: {task_creates:?}",
        );
    }

    #[tokio::test]
    async fn mint_batches_parent_under_scope_selected_work_epic_with_union_spec_labels() {
        let run = run_mixed_tree_materialization().await;
        let task_creates = run
            .calls
            .iter()
            .filter(|call| {
                call.iter().any(|arg| arg == "create")
                    && call
                        .windows(2)
                        .any(|pair| pair[0] == "--type" && pair[1] == "task")
            })
            .collect::<Vec<_>>();
        assert!(
            task_creates
                .iter()
                .all(|call| flag_arg(call, "--parent") == "lm-tree"),
            "tree children use the standing work epic: {task_creates:?}",
        );

        let fixup = task_creates
            .iter()
            .find(|call| labels_arg(call).contains(&run.fix_label.as_str()))
            .expect("fix-up child create");
        let fixup_labels = labels_arg(fixup);
        assert!(
            fixup_labels.contains(&"spec:gate"),
            "labels: {fixup_labels:?}"
        );
        assert!(
            fixup_labels.contains(&"spec:harness"),
            "multi-spec finding contributes union spec labels: {fixup_labels:?}",
        );
        assert!(
            task_creates.iter().any(
                |call| labels_arg(call).contains(&run.clarify_label.as_str())
                    && labels_arg(call).contains(&"loom:clarify")
            ),
            "clarify child carries loom:clarify: {task_creates:?}",
        );
        assert!(
            task_creates.iter().any(|call| {
                let labels = labels_arg(call);
                labels.contains(&run.blocked_label.as_str())
                    && labels.contains(&"loom:blocked")
                    && !labels.contains(&"loom:clarify")
            }),
            "blocked-clarify child carries loom:blocked only: {task_creates:?}",
        );
    }

    #[tokio::test]
    async fn mint_tree_no_actionable_findings_leaves_active_unchanged() {
        let finding = coherence_finding(vec![spec("alpha")], "x", "evidence");
        let hash = finding.hash();
        let runner = ScriptedRunner::new(vec![ok_stdout(&format!(
            "[{}]",
            fixup_row("lm-existing.1", &hash),
        ))]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);

        let summary =
            mint_tree_findings_with_options(&bd, &[finding], "head-sha", &MintOptions::default())
                .await;

        assert_eq!(summary.skipped, 1);
        assert_eq!(summary.minted, 0);
        assert!(summary.active_epic.is_none());
        let calls = rendered_calls(&invocations);
        assert!(
            calls.iter().all(|call| {
                !call.iter().any(|arg| arg == "create")
                    && !call.iter().any(|arg| arg == "update")
                    && !call.iter().any(|arg| arg.contains(ACTIVE_LABEL))
            }),
            "active bookmark is untouched when nothing actionable remains: {calls:?}",
        );
    }

    async fn run_empty_tree_failure_cleanup() -> (MintSummary, Vec<Vec<String>>) {
        let finding = coherence_finding(vec![spec("gate")], "x", "evidence");
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout(&spec_epic_list("lm-gatespec", "gate", "closed")),
            ok_stdout(&format!(
                "[{}]",
                active_epic_row("lm-prev", &[ACTIVE_LABEL, "spec:gate"]),
            )),
            ok_stdout("lm-empty\n"),
            err_stderr("child create failed"),
            ok_stdout(""),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);
        let summary =
            mint_tree_findings_with_options(&bd, &[finding], "head-sha", &MintOptions::default())
                .await;
        (summary, rendered_calls(&invocations))
    }

    #[tokio::test]
    async fn mint_tree_partial_failure_never_leaves_empty_active_epic() {
        let (summary, calls) = run_empty_tree_failure_cleanup().await;

        assert_eq!(summary.minted, 0);
        assert_eq!(summary.errors, 1, "child failure is reported");
        assert!(summary.active_epic.is_none());
        assert!(
            calls.iter().any(|call| call
                == &[
                    "close".to_string(),
                    "lm-empty".to_string(),
                    "--reason".to_string(),
                    EMPTY_TREE_EPIC_CLOSE_REASON.to_string(),
                ]),
            "empty remediation epic is neutralized: {calls:?}",
        );
        assert!(
            calls
                .iter()
                .all(|call| !call.iter().any(|arg| arg == "update")),
            "active labels are not touched before the first child exists: {calls:?}",
        );
    }

    #[tokio::test]
    async fn mint_tree_never_leaves_empty_active_remediation_epic() {
        let (summary, calls) = run_empty_tree_failure_cleanup().await;

        assert_eq!(summary.minted, 0);
        assert!(summary.active_epic.is_none());
        assert!(
            calls
                .iter()
                .any(|call| call.first().map(String::as_str) == Some("close")),
            "empty failed epic is closed or neutralized: {calls:?}",
        );
        assert!(
            calls
                .iter()
                .all(|call| !call.iter().any(|arg| arg == ACTIVE_LABEL)),
            "no open active empty remediation epic is left behind: {calls:?}",
        );
    }

    #[tokio::test]
    async fn mint_tree_non_empty_partial_failure_leaves_active_epic_for_dedup_rerun() {
        let first = coherence_finding(vec![spec("gate")], "x", "first");
        let second = contract_finding(vec![spec("harness")], "y", "second");
        let first_label = finding_label(&first);
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout("[]"),
            ok_stdout(&spec_epic_list("lm-gatespec", "gate", "closed")),
            ok_stdout(&spec_epic_list("lm-harnessspec", "harness", "closed")),
            ok_stdout("[]"),
            ok_stdout("lm-tree\n"),
            ok_stdout("lm-tree.1\n"),
            ok_stdout(""),
            err_stderr("second child failed"),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);

        let summary = mint_tree_findings_with_options(
            &bd,
            &[first, second],
            "head-sha",
            &MintOptions::default(),
        )
        .await;

        assert_eq!(summary.minted, 1);
        assert_eq!(summary.errors, 1);
        assert_eq!(
            summary.active_epic.as_ref().map(BeadId::as_str),
            Some("lm-tree"),
        );
        let calls = rendered_calls(&invocations);
        assert!(
            calls
                .iter()
                .all(|call| call.first().map(String::as_str) != Some("close")),
            "non-empty remediation epic remains open: {calls:?}",
        );
        let first_child = calls
            .iter()
            .find(|call| {
                call.iter().any(|arg| arg == "create")
                    && call
                        .windows(2)
                        .any(|pair| pair[0] == "--type" && pair[1] == "task")
            })
            .expect("first child create");
        assert!(
            labels_arg(first_child).contains(&first_label.as_str()),
            "created child carries finding hash for rerun dedup: {first_child:?}",
        );
    }

    #[tokio::test]
    async fn mint_bonding_lead_groups_findings_without_selecting_tree_parent_epic() {
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
        let runner = ScriptedRunner::new(vec![
            ok_stdout("[]"),
            ok_stdout("[]"),
            ok_stdout(&spec_epic_list("lm-harnessspec", "harness", "closed")),
            ok_stdout(&spec_epic_list("lm-gatespec", "gate", "closed")),
        ]);
        let invocations = runner.invocations_handle();
        let bd = BdClient::with_runner(runner);

        let (plan, summary) =
            plan_tree_mint_with_options(&bd, &[f1.clone(), f2.clone()], &MintOptions::default())
                .await;

        assert_eq!(summary.refused, 0, "planning should be clean: {summary:?}");
        assert_eq!(plan.batches().len(), 1, "same lead groups into one batch");
        let batch = &plan.batches()[0];
        assert_eq!(batch.lead_spec.as_str(), "harness");
        assert_eq!(batch.findings.len(), 2);
        let labels = batch_labels(
            &batch.findings,
            &mint_label(&batch.fingerprint),
            FindingRouting::Fixup,
        );
        for (id, hash, label) in [
            (f1.id(), f1.hash(), finding_label(&f1)),
            (f2.id(), f2.hash(), finding_label(&f2)),
        ] {
            assert!(labels.contains(&label), "labels: {labels:?}");
            assert!(
                batch
                    .findings
                    .iter()
                    .any(|finding| finding.id() == id && finding.hash() == hash),
                "plan preserves finding identity",
            );
        }
        assert!(labels.contains(&"spec:harness".to_string()), "{labels:?}");
        assert!(labels.contains(&"spec:gate".to_string()), "{labels:?}");
        let calls = rendered_calls(&invocations);
        assert!(
            calls
                .iter()
                .all(|call| !call.iter().any(|arg| arg == "--parent")),
            "lead spec groups only; it does not choose a tree parent: {calls:?}",
        );
        assert!(
            calls
                .iter()
                .all(|call| !call.iter().any(|arg| arg == "create")),
            "existing metadata epics need no writes and no work epic is selected: {calls:?}",
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
        assert_eq!(summary.routing.len(), 1);
        assert!(summary.routing[0].clarify_downgraded());
        assert_eq!(
            summary.routing[0].options_parse_result,
            Some(OptionsParseResult::MissingOrMalformed),
        );
        assert!(!summary.routing[0].evidence_hash.is_empty());
        assert_eq!(summary.routing[0].evidence_excerpt, malformed_evidence);
        let event_kinds = summary
            .routing_events()
            .into_iter()
            .map(|event| event.driver_kind)
            .collect::<Vec<_>>();
        assert_eq!(
            event_kinds,
            vec![
                DriverKind::MarkerRouted,
                DriverKind::ClarifyDowngraded,
                DriverKind::BdStateTransition,
            ],
        );
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
        assert_eq!(
            flag_arg(create, "--notes"),
            CLARIFY_WITHOUT_OPTIONS_CAUSE,
            "mint downgrade must leave a compact Beads note breadcrumb",
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
            render.starts_with("minted 1 batches (2 findings across 1 specs); finding lifecycle:"),
            "header line shape: {render}",
        );
        assert!(
            render.contains("skipped live 1"),
            "header line shape: {render}"
        );
        assert!(
            render.contains("structural conflicts 1"),
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
}

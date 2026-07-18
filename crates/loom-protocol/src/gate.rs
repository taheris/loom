//! Typed wire-format contract for `loom gate` findings and review-walk
//! terminals.
//!
//! Consumers construct walk output through [`WalkOutput::from_stdout`]
//! and route findings via the validated [`Finding`] / [`BadWalk`] types.

pub mod options;

use displaydoc::Display;
use loom_events::identifier::SpecLabel;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One concern raised by either the LLM rubric or a deterministic
/// verifier, in the shape the mint pipeline consumes.
///
/// `bonds` is bonding metadata (which spec molecules the fix-up should
/// route to); `target` is identity metadata (what the finding is
/// about). The two are kept structurally separate so the driver can
/// shift bonding without invalidating the finding id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub token: ConcernToken,
    pub route: FindingRoute,
    pub bonds: Vec<SpecLabel>,
    pub target: FindingTarget,
    pub evidence: String,
}

impl Finding {
    /// Canonical versioned semantic identity for this finding.
    ///
    /// The id is target-centred, lower-kebab, and excludes volatile
    /// context such as evidence prose, bonds ordering, and line numbers.
    #[must_use]
    pub fn id(&self) -> String {
        format!(
            "{}:{}",
            IDENTITY_VERSION,
            self.target.identity_key(self.token)
        )
    }

    /// Compact bd-label key derived from [`Self::id`].
    #[must_use]
    pub fn hash(&self) -> String {
        finding_hash_from_id(&self.id())
    }
}

const IDENTITY_VERSION: &str = "v1";
const FINDING_HASH_HEX_LEN: usize = 12;

/// Workflow route carried by each rubric-origin finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FindingRoute {
    #[serde(rename = "blocking")]
    Blocking,
    #[serde(rename = "deferred")]
    Deferred,
    #[serde(rename = "clarify")]
    Clarify,
}

impl FindingRoute {
    /// Canonical wire string used in `LOOM_FINDING:` JSON.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Blocking => "blocking",
            Self::Deferred => "deferred",
            Self::Clarify => "clarify",
        }
    }
}

/// Closed-set concern tokens emitted by the rubric walk or normalised
/// from a deterministic verifier verdict.
///
/// The wire string (e.g. `spec-coherence-fail`) is the canonical name
/// across the `LOOM_FINDING:` JSON, bd labels, and log surfaces; the
/// Rust variant name is the same with kebab-case lowered to
/// PascalCase. Tokens carry a [`ScopeKind`] discoverable via
/// [`Self::scope_kind`]; the parse pipeline rejects a finding whose
/// token does not admit the active [`DispatchScope`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConcernToken {
    #[serde(rename = "spec-coherence-fail")]
    SpecCoherenceFail,
    #[serde(rename = "orphan-integration")]
    OrphanIntegration,
    #[serde(rename = "style-rule-violation")]
    StyleRuleViolation,
    #[serde(rename = "verifier-bypass")]
    VerifierBypass,
    #[serde(rename = "weak-assertion")]
    WeakAssertion,
    #[serde(rename = "fabricated-result")]
    FabricatedResult,
    #[serde(rename = "coincidental-pass")]
    CoincidentalPass,
    #[serde(rename = "mock-discipline")]
    MockDiscipline,
    #[serde(rename = "verifier-too-narrow")]
    VerifierTooNarrow,
    #[serde(rename = "concurrency-untested")]
    ConcurrencyUntested,
    #[serde(rename = "judge-flag")]
    JudgeFlag,
    #[serde(rename = "invariant-clash")]
    InvariantClash,
    #[serde(rename = "template-spec-drift")]
    TemplateSpecDrift,
    #[serde(rename = "cross-spec-clash")]
    CrossSpecClash,
    #[serde(rename = "spec-conventions-violation")]
    SpecConventionsViolation,
    #[serde(rename = "verifier-failed")]
    VerifierFailed,
    #[serde(rename = "dispatch-error")]
    DispatchError,
    #[serde(rename = "unresolved-annotation")]
    UnresolvedAnnotation,
    #[serde(rename = "stub-pointing")]
    StubPointing,
    #[serde(rename = "multiple-annotations")]
    MultipleAnnotations,
    #[serde(rename = "unneeded-pending-marker")]
    UnneededPendingMarker,
    #[serde(rename = "inputs-protocol-error")]
    InputsProtocolError,
    #[serde(rename = "scope-creep")]
    ScopeCreep,
    #[serde(rename = "scope-shortfall")]
    ScopeShortfall,
    #[serde(rename = "pending-marker-resolved")]
    PendingMarkerResolved,
}

impl ConcernToken {
    /// Canonical wire string used in `LOOM_FINDING:` JSON, bd labels,
    /// and finding identity input. Matches the leftmost column in
    /// `specs/gate.md` §"Concern tokens and target variants".
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::SpecCoherenceFail => "spec-coherence-fail",
            Self::OrphanIntegration => "orphan-integration",
            Self::StyleRuleViolation => "style-rule-violation",
            Self::VerifierBypass => "verifier-bypass",
            Self::WeakAssertion => "weak-assertion",
            Self::FabricatedResult => "fabricated-result",
            Self::CoincidentalPass => "coincidental-pass",
            Self::MockDiscipline => "mock-discipline",
            Self::VerifierTooNarrow => "verifier-too-narrow",
            Self::ConcurrencyUntested => "concurrency-untested",
            Self::JudgeFlag => "judge-flag",
            Self::InvariantClash => "invariant-clash",
            Self::TemplateSpecDrift => "template-spec-drift",
            Self::CrossSpecClash => "cross-spec-clash",
            Self::SpecConventionsViolation => "spec-conventions-violation",
            Self::VerifierFailed => "verifier-failed",
            Self::DispatchError => "dispatch-error",
            Self::UnresolvedAnnotation => "unresolved-annotation",
            Self::StubPointing => "stub-pointing",
            Self::MultipleAnnotations => "multiple-annotations",
            Self::UnneededPendingMarker => "unneeded-pending-marker",
            Self::InputsProtocolError => "inputs-protocol-error",
            Self::ScopeCreep => "scope-creep",
            Self::ScopeShortfall => "scope-shortfall",
            Self::PendingMarkerResolved => "pending-marker-resolved",
        }
    }

    /// Target-variant the token MUST carry per `specs/gate.md`
    /// §"Concern tokens and target variants". The parser rejects any
    /// `LOOM_FINDING:` payload whose `target.kind` does not equal the
    /// value returned here.
    #[must_use]
    pub fn expected_target_kind(self) -> TargetKind {
        match self {
            Self::SpecCoherenceFail
            | Self::VerifierTooNarrow
            | Self::JudgeFlag
            | Self::MultipleAnnotations
            | Self::CrossSpecClash
            | Self::SpecConventionsViolation
            | Self::ScopeCreep
            | Self::ScopeShortfall => TargetKind::Criterion,
            Self::PendingMarkerResolved => TargetKind::MatrixCell,
            Self::OrphanIntegration => TargetKind::Contract,
            Self::StyleRuleViolation => TargetKind::StyleRule,
            Self::VerifierBypass
            | Self::WeakAssertion
            | Self::FabricatedResult
            | Self::CoincidentalPass
            | Self::VerifierFailed
            | Self::DispatchError
            | Self::UnresolvedAnnotation
            | Self::StubPointing
            | Self::UnneededPendingMarker
            | Self::InputsProtocolError => TargetKind::Annotation,
            Self::MockDiscipline => TargetKind::TestPath,
            Self::ConcurrencyUntested => TargetKind::LockSite,
            Self::InvariantClash => TargetKind::Invariant,
            Self::TemplateSpecDrift => TargetKind::Template,
        }
    }

    /// True iff `actual` is an allowed target variant for this token.
    #[must_use]
    pub fn allows_target_kind(self, actual: TargetKind) -> bool {
        if self == Self::PendingMarkerResolved {
            return matches!(actual, TargetKind::MatrixCell | TargetKind::SurfaceElement);
        }
        self.expected_target_kind() == actual
    }

    /// Scope class for the token — which dispatch scopes the parse
    /// pipeline admits the token from. Per `specs/gate.md` § *Concern
    /// tokens and target variants* and § *Scope-dependent walk*:
    ///
    /// - `template-spec-drift` / `verifier-failed` / `dispatch-error` /
    ///   `multiple-annotations` are emitted only at `--tree` scope.
    /// - `unresolved-annotation` / `stub-pointing` /
    ///   `unneeded-pending-marker` / `inputs-protocol-error` are emitted
    ///   at standing `--tree` scope and molecule-completion push-gate
    ///   scope.
    /// - `scope-creep` / `scope-shortfall` are per-bead-only — the
    ///   tree-scope walk never emits them.
    /// - Everything else is admissible at any scope.
    #[must_use]
    pub fn scope_kind(self) -> ScopeKind {
        match self {
            Self::TemplateSpecDrift
            | Self::CrossSpecClash
            | Self::SpecConventionsViolation
            | Self::VerifierFailed
            | Self::DispatchError
            | Self::MultipleAnnotations => ScopeKind::TreeOnly,
            Self::UnresolvedAnnotation
            | Self::StubPointing
            | Self::UnneededPendingMarker
            | Self::InputsProtocolError => ScopeKind::TreeAndPushGate,
            Self::ScopeCreep | Self::ScopeShortfall => ScopeKind::PerBead,
            Self::SpecCoherenceFail
            | Self::OrphanIntegration
            | Self::StyleRuleViolation
            | Self::VerifierBypass
            | Self::WeakAssertion
            | Self::FabricatedResult
            | Self::CoincidentalPass
            | Self::MockDiscipline
            | Self::VerifierTooNarrow
            | Self::ConcurrencyUntested
            | Self::JudgeFlag
            | Self::InvariantClash
            | Self::PendingMarkerResolved => ScopeKind::AnyScope,
        }
    }
}

/// Dispatch scope the parse pipeline ran under — `--bead` / regular
/// `--diff` / `--files` collapse to [`Self::PerBead`]; `--tree` is
/// [`Self::Tree`]; molecule-completion integrity recovery uses
/// [`Self::PushGate`]. Threaded into [`Finding::parse_payload`],
/// [`WalkOutput::from_stdout`], and [`parse_walk_output`] so token-scope
/// alignment is enforced at the wire boundary per `specs/gate.md` §
/// *Concern tokens and target variants*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchScope {
    /// `--bead <id>` / ordinary `--diff <range>` / `--files <paths>` —
    /// the per-bead walks. Tree/push-gate integrity tokens and tree-only
    /// rubric tokens are rejected.
    PerBead,
    /// Molecule-completion push-gate integrity recovery. Admits the
    /// integrity tokens that also run at standing tree scope, but rejects
    /// tree-only rubric/verifier tokens and per-bead-only tokens.
    PushGate,
    /// `--tree` — the standing-safety-net walk. Per-bead-only tokens
    /// (`scope-creep`, `scope-shortfall`) are rejected.
    Tree,
}

impl DispatchScope {
    /// Stable label for error messages and log surfaces.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::PerBead => "per-bead",
            Self::PushGate => "push-gate",
            Self::Tree => "tree",
        }
    }
}

/// Per-token scope class. Compared against the active [`DispatchScope`]
/// at parse time via [`Self::admits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    /// Emitted only at per-bead dispatch scope (`--bead` / `--diff` /
    /// `--files`); rejected at `--tree`.
    PerBead,
    /// Emitted only at `--tree` dispatch scope (deterministic verifier
    /// dispatch and tree-only rubric checks); rejected at per-bead and
    /// push-gate scopes.
    TreeOnly,
    /// Emitted at standing `--tree` scope and molecule-completion
    /// push-gate scope; rejected at regular per-bead review scopes.
    TreeAndPushGate,
    /// Admissible at any dispatch scope.
    AnyScope,
}

impl ScopeKind {
    /// True iff the token (with this scope class) may be parsed under
    /// the active dispatch scope.
    #[must_use]
    pub fn admits(self, scope: DispatchScope) -> bool {
        match (self, scope) {
            (Self::AnyScope, _)
            | (Self::PerBead, DispatchScope::PerBead)
            | (Self::TreeOnly, DispatchScope::Tree)
            | (Self::TreeAndPushGate, DispatchScope::Tree | DispatchScope::PushGate) => true,
            (Self::PerBead, DispatchScope::Tree | DispatchScope::PushGate)
            | (Self::TreeOnly, DispatchScope::PerBead | DispatchScope::PushGate)
            | (Self::TreeAndPushGate, DispatchScope::PerBead) => false,
        }
    }

    /// Stable label for error messages.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::PerBead => "per-bead-only",
            Self::TreeOnly => "tree-only",
            Self::TreeAndPushGate => "tree-and-push-gate",
            Self::AnyScope => "any-scope",
        }
    }
}

/// Discriminator tag for [`FindingTarget`]. Matches the `kind` field in
/// the wire JSON one-to-one; the parser compares
/// [`ConcernToken::expected_target_kind`] against [`FindingTarget::kind`]
/// to enforce the token/variant alignment from `specs/gate.md`
/// §"Concern tokens and target variants".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Criterion,
    Contract,
    StyleRule,
    Annotation,
    TestPath,
    LockSite,
    Invariant,
    Template,
    MatrixCell,
    SurfaceElement,
}

impl TargetKind {
    /// Canonical wire string — matches the `#[serde(tag = "kind")]`
    /// variant name in [`FindingTarget`], so error messages and the wire
    /// payload agree byte-for-byte.
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Criterion => "Criterion",
            Self::Contract => "Contract",
            Self::StyleRule => "StyleRule",
            Self::Annotation => "Annotation",
            Self::TestPath => "TestPath",
            Self::LockSite => "LockSite",
            Self::Invariant => "Invariant",
            Self::Template => "Template",
            Self::MatrixCell => "MatrixCell",
            Self::SurfaceElement => "SurfaceElement",
        }
    }
}

impl std::fmt::Display for TargetKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire())
    }
}

/// Identity-bearing payload that narrows what the finding is about.
///
/// The variant is selected by `kind` in the wire JSON
/// (`#[serde(tag = "kind")]`). Each token in [`ConcernToken`] is
/// paired with a specific variant per `specs/gate.md`
/// §"Concern tokens and target variants" — the parser enforces
/// token/variant alignment so mismatch is rejected at the wire boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum FindingTarget {
    Criterion {
        spec: SpecLabel,
        anchor: String,
    },
    Contract {
        id: String,
    },
    StyleRule {
        rule_id: String,
        subject: String,
    },
    Annotation {
        target_string: String,
    },
    TestPath {
        path: String,
    },
    LockSite {
        file: String,
        line: u32,
    },
    Invariant {
        spec: SpecLabel,
        section: String,
        tag: String,
    },
    Template {
        path: String,
    },
    MatrixCell {
        spec: SpecLabel,
        partial: String,
        template: String,
    },
    SurfaceElement {
        spec: SpecLabel,
        element_kind: String,
        name: String,
    },
}

impl FindingTarget {
    /// Discriminator equivalent to the `kind` field in the wire JSON.
    /// Used by the parser to enforce token/variant alignment per
    /// `specs/gate.md` §"Concern tokens and target variants".
    #[must_use]
    pub fn kind(&self) -> TargetKind {
        match self {
            Self::Criterion { .. } => TargetKind::Criterion,
            Self::Contract { .. } => TargetKind::Contract,
            Self::StyleRule { .. } => TargetKind::StyleRule,
            Self::Annotation { .. } => TargetKind::Annotation,
            Self::TestPath { .. } => TargetKind::TestPath,
            Self::LockSite { .. } => TargetKind::LockSite,
            Self::Invariant { .. } => TargetKind::Invariant,
            Self::Template { .. } => TargetKind::Template,
            Self::MatrixCell { .. } => TargetKind::MatrixCell,
            Self::SurfaceElement { .. } => TargetKind::SurfaceElement,
        }
    }

    /// The spec field, when this variant carries one. `Criterion` and
    /// `Invariant` are the only variants whose target identity is bound
    /// to a spec; the parser uses this to enforce the
    /// "target.spec MUST appear in bonds" rule from `specs/gate.md`
    /// § *Findings and Minting*.
    #[must_use]
    pub fn spec(&self) -> Option<&SpecLabel> {
        match self {
            Self::Criterion { spec, .. }
            | Self::Invariant { spec, .. }
            | Self::MatrixCell { spec, .. }
            | Self::SurfaceElement { spec, .. } => Some(spec),
            Self::Contract { .. }
            | Self::StyleRule { .. }
            | Self::Annotation { .. }
            | Self::TestPath { .. }
            | Self::LockSite { .. }
            | Self::Template { .. } => None,
        }
    }

    fn canonicalized(self) -> Self {
        match self {
            Self::Annotation { target_string } => Self::Annotation {
                target_string: canonical_annotation_target(&target_string),
            },
            other => other,
        }
    }

    /// Variant-aware canonical string for human-facing batch surfaces.
    #[must_use]
    pub fn canonical_form(&self) -> String {
        match self {
            Self::Criterion { spec, anchor } => format!("criterion:{spec}:{anchor}"),
            Self::Contract { id } => format!("contract:{id}"),
            Self::StyleRule { rule_id, subject } => format!("style:{rule_id}:{subject}"),
            Self::Annotation { target_string } => format!("annotation:{target_string}"),
            Self::TestPath { path } => format!("test:{path}"),
            Self::LockSite { file, line } => format!("lock:{file}:{line}"),
            Self::Invariant { spec, section, tag } => {
                format!("invariant:{spec}:{section}:{tag}")
            }
            Self::Template { path } => format!("template:{path}"),
            Self::MatrixCell {
                spec,
                partial,
                template,
            } => format!("matrix-cell:{spec}:{partial}:{template}"),
            Self::SurfaceElement {
                spec,
                element_kind,
                name,
            } => format!("surface-element:{spec}:{element_kind}:{name}"),
        }
    }

    fn identity_key(&self, token: ConcernToken) -> String {
        match self {
            Self::Criterion { spec, anchor } => {
                format!(
                    "criterion:{}:{spec}#{}",
                    token.as_wire(),
                    lower_kebab(anchor)
                )
            }
            Self::Contract { id } => format!("contract:{}", lower_kebab(id)),
            Self::StyleRule { rule_id, subject } => {
                format!(
                    "style-rule:{}:{}",
                    lower_kebab(rule_id),
                    lower_kebab(subject)
                )
            }
            Self::Annotation { target_string } => {
                format!(
                    "annotation:{}:{}",
                    token.as_wire(),
                    lower_kebab(target_string)
                )
            }
            Self::TestPath { path } => format!("test-path:{}", lower_kebab(path)),
            Self::LockSite { file, .. } => format!("lock-site:{}", lower_kebab(file)),
            Self::Invariant { spec, section, tag } => {
                format!(
                    "invariant:{spec}#{}#{}",
                    lower_kebab(section),
                    lower_kebab(tag),
                )
            }
            Self::Template { path } => format!("template:{}", lower_kebab(path)),
            Self::MatrixCell {
                spec,
                partial,
                template,
            } => format!(
                "matrix-cell:{spec}#{}#{}",
                lower_kebab(partial),
                lower_kebab(template),
            ),
            Self::SurfaceElement {
                spec,
                element_kind,
                name,
            } => format!(
                "surface-element:{spec}#{}#{}",
                lower_kebab(element_kind),
                lower_kebab(name),
            ),
        }
    }
}

fn finding_hash_from_id(id: &str) -> String {
    format!("{IDENTITY_VERSION}:{}", finding_hash_body(id))
}

fn finding_hash_body(id: &str) -> String {
    let hash = blake3::hash(id.as_bytes());
    let hex = hash.to_hex();
    hex.as_str()[..FINDING_HASH_HEX_LEN].to_owned()
}

fn lower_kebab(input: &str) -> String {
    let mut out = String::new();
    let mut previous_was_separator = true;
    for c in input.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            previous_was_separator = false;
        } else if !previous_was_separator {
            out.push('-');
            previous_was_separator = true;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}

fn canonical_annotation_target(target: &str) -> String {
    embedded_annotation_target(target).unwrap_or_else(|| target.trim().to_owned())
}

fn embedded_annotation_target(target: &str) -> Option<String> {
    let mut offset = 0;
    while let Some(start_rel) = target[offset..].find('[') {
        let start = offset + start_rel;
        let label_start = start + 1;
        let close_rel = target[label_start..].find(']')?;
        let close = label_start + close_rel;
        let label = &target[label_start..close];
        let tier = label.strip_suffix('?').unwrap_or(label);
        let after_close = close + 1;
        if !annotation_wrapper_prefix(&target[..start])
            || !matches!(tier, "check" | "test" | "system" | "judge")
            || !target[after_close..].starts_with('(')
        {
            offset = after_close;
            continue;
        }
        let inner_start = after_close + 1;
        let inner_end = matching_annotation_paren(target, inner_start)?;
        return Some(target[inner_start..inner_end].trim().to_owned());
    }
    None
}

fn annotation_wrapper_prefix(prefix: &str) -> bool {
    let trimmed = prefix.trim();
    trimmed.is_empty()
        || trimmed.rsplit_once(':').is_some_and(|(path, line)| {
            path.ends_with(".md") && line.chars().all(|ch| ch.is_ascii_digit())
        })
}

fn matching_annotation_paren(input: &str, start: usize) -> Option<usize> {
    let mut depth = 1usize;
    for (rel, ch) in input[start..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(start + rel);
                }
            }
            _ => {}
        }
    }
    None
}

/// Wire-format prefix the LLM rubric emits before each finding's JSON
/// payload (per `specs/gate.md` § *Emit shape*). Matches the prefix
/// shape `parse_review_flag` / `parse_exit_signal` use for the marker
/// surface so consumers can scan a single agent-stdout buffer for both.
pub const LOOM_FINDING_PREFIX: &str = "LOOM_FINDING:";

/// Resolver injected into the walk-level parser for the I/O-bearing
/// validation layers — Layer 3 (spec-label resolution) and Layer 5
/// (target-content resolution). Wrapping these in a trait keeps the
/// parser unit-testable against synthetic fixtures and lets the mint
/// driver plug in the real on-disk implementations.
pub trait FindingValidator {
    /// Layer 3 — every element of `bonds` MUST be a known workspace
    /// spec label (the basename of a file under `specs/` minus `.md`).
    fn spec_label_is_known(&self, label: &SpecLabel) -> bool;

    /// Layer 5 — `Criterion { spec, anchor }` resolves when the spec
    /// file contains the named anchor.
    fn criterion_anchor_resolves(&self, spec: &SpecLabel, anchor: &str) -> bool;

    /// Layer 5 — `Annotation { target_string }` resolves when the
    /// integrity gate's forward-resolution would accept the same string
    /// as a valid annotation target (command on PATH, test name in
    /// scope, judge file on disk).
    fn annotation_resolves(&self, target_string: &str) -> bool;

    /// Layer 5 — `TestPath { path }` / `Template { path }` /
    /// `LockSite { file, .. }` resolve when the named file exists on
    /// disk (relative to repo root).
    fn file_exists(&self, path: &str) -> bool;

    /// Layer 5 — `Invariant { spec, section, tag }` resolves when the
    /// spec file declares an invariant matching `(section, tag)`.
    fn invariant_resolves(&self, spec: &SpecLabel, section: &str, tag: &str) -> bool;
}

/// Per-layer parse / validation failure. Every variant carries the
/// offending record's 1-based start line and verbatim text so a re-run
/// prompt has the evidence it needs (per `specs/gate.md` § *Strict
/// parse-time validation*).
///
/// Clone / PartialEq / Eq are required so this error type can ride
/// inside `BadWalk::MalformedFinding { errors: Vec<FindingParseError>,
/// .. }` (and through `ExitSignal::BadWalk`); for the `Json` variant the
/// `source` field is the rendered `serde_json::Error` message rather
/// than the typed error, so the chain via `std::error::Error::source`
/// is the rendered string.
#[derive(Debug, Display, Error, Clone, PartialEq, Eq)]
pub enum FindingParseError {
    /// line {line_number}: LOOM_FINDING payload is not valid JSON ({message}) — `{raw}`
    Json {
        line_number: usize,
        raw: String,
        message: String,
    },
    /// line {line_number}: bonds element `{spec}` does not resolve to a workspace spec — `{raw}`
    UnknownBondSpec {
        line_number: usize,
        spec: String,
        raw: String,
    },
    /// line {line_number}: target.kind `{actual}` does not match token `{token}` (expected `{expected}`) — `{raw}`
    TokenVariantMismatch {
        line_number: usize,
        token: &'static str,
        expected: TargetKind,
        actual: TargetKind,
        raw: String,
    },
    /// line {line_number}: target.spec `{spec}` not present in bonds {bonds:?} — `{raw}`
    TargetSpecNotInBonds {
        line_number: usize,
        spec: String,
        bonds: Vec<String>,
        raw: String,
    },
    /// line {line_number}: target content does not resolve — {detail} — `{raw}`
    UnresolvedTarget {
        line_number: usize,
        detail: String,
        raw: String,
    },
    /// line {line_number}: StyleRule subject is not a stable concrete subject ({reason}) — `{raw}`
    InvalidStyleRuleSubject {
        line_number: usize,
        reason: &'static str,
        raw: String,
    },
    /// line {line_number}: token `{token}` is {scope_kind} but the walk runs at {dispatch_scope} scope — `{raw}`
    TokenScopeMismatch {
        line_number: usize,
        token: &'static str,
        scope_kind: &'static str,
        dispatch_scope: &'static str,
        raw: String,
    },
    /// line {line_number}: route `{route}` is not valid at {dispatch_scope} scope — `{raw}`
    RouteScopeMismatch {
        line_number: usize,
        route: &'static str,
        dispatch_scope: &'static str,
        raw: String,
    },
}

impl Finding {
    /// Pure parse for a single `LOOM_FINDING:` payload: JSON syntax
    /// (Layer 1), token/kind closed-set membership (Layer 2; enforced
    /// by `serde` deserialization since both are `#[serde(rename = ...)]`
    /// enums), token/variant alignment (Layer 4), token-scope
    /// admissibility against `scope`, and the "target.spec ∈ bonds"
    /// rule. Layers 3 and 5 are deferred to [`Finding::validate`]
    /// because they require I/O context.
    ///
    /// `line_number` is the 1-based line offset in the agent's stdout
    /// buffer; included in every error variant so the caller can quote
    /// the offending line back to the agent on a re-run. `scope` is
    /// the active dispatch scope per `specs/gate.md` § *Concern tokens
    /// and target variants* — tokens whose [`ConcernToken::scope_kind`]
    /// does not [`ScopeKind::admits`] this scope surface a typed
    /// [`FindingParseError::TokenScopeMismatch`].
    pub fn parse_payload(
        payload: &str,
        line_number: usize,
        raw_line: &str,
        scope: DispatchScope,
    ) -> Result<Self, FindingParseError> {
        let mut finding: Finding =
            serde_json::from_str(payload).map_err(|source| FindingParseError::Json {
                line_number,
                raw: raw_line.to_owned(),
                message: source.to_string(),
            })?;
        finding.target = finding.target.canonicalized();

        let expected_kind = finding.token.expected_target_kind();
        let actual_kind = finding.target.kind();
        if !finding.token.allows_target_kind(actual_kind) {
            return Err(FindingParseError::TokenVariantMismatch {
                line_number,
                token: finding.token.as_wire(),
                expected: expected_kind,
                actual: actual_kind,
                raw: raw_line.to_owned(),
            });
        }

        let scope_kind = finding.token.scope_kind();
        if !scope_kind.admits(scope) {
            return Err(FindingParseError::TokenScopeMismatch {
                line_number,
                token: finding.token.as_wire(),
                scope_kind: scope_kind.label(),
                dispatch_scope: scope.label(),
                raw: raw_line.to_owned(),
            });
        }

        if finding.route == FindingRoute::Blocking && scope == DispatchScope::PushGate {
            return Err(FindingParseError::RouteScopeMismatch {
                line_number,
                route: finding.route.as_wire(),
                dispatch_scope: scope.label(),
                raw: raw_line.to_owned(),
            });
        }

        if let Some(target_spec) = finding.target.spec()
            && !finding.bonds.contains(target_spec)
        {
            return Err(FindingParseError::TargetSpecNotInBonds {
                line_number,
                spec: target_spec.to_string(),
                bonds: finding.bonds.iter().map(ToString::to_string).collect(),
                raw: raw_line.to_owned(),
            });
        }

        if let FindingTarget::StyleRule { subject, .. } = &finding.target
            && let Some(reason) = invalid_style_rule_subject_reason(subject)
        {
            return Err(FindingParseError::InvalidStyleRuleSubject {
                line_number,
                reason,
                raw: raw_line.to_owned(),
            });
        }

        Ok(finding)
    }

    /// I/O-bearing validation: Layer 3 (every bond resolves to a known
    /// workspace spec) and Layer 5 (the target's identity-bearing
    /// fields resolve on disk). Pure JSON / closed-set / variant /
    /// bonds-spec rules already fired in [`Finding::parse_payload`];
    /// this is the second stage that the mint driver runs once the
    /// resolver is wired.
    pub fn validate<V: FindingValidator + ?Sized>(
        &self,
        line_number: usize,
        raw_line: &str,
        validator: &V,
    ) -> Result<(), FindingParseError> {
        for bond in &self.bonds {
            if !validator.spec_label_is_known(bond) {
                return Err(FindingParseError::UnknownBondSpec {
                    line_number,
                    spec: bond.to_string(),
                    raw: raw_line.to_owned(),
                });
            }
        }

        let resolved = match &self.target {
            FindingTarget::Criterion { spec, anchor } => {
                validator.criterion_anchor_resolves(spec, anchor)
            }
            FindingTarget::Annotation { target_string } => {
                validator.annotation_resolves(target_string)
            }
            FindingTarget::TestPath { path } | FindingTarget::Template { path } => {
                validator.file_exists(path)
            }
            FindingTarget::LockSite { file, .. } => validator.file_exists(file),
            FindingTarget::Invariant { spec, section, tag } => {
                validator.invariant_resolves(spec, section, tag)
            }
            FindingTarget::Contract { .. }
            | FindingTarget::StyleRule { .. }
            | FindingTarget::MatrixCell { .. }
            | FindingTarget::SurfaceElement { .. } => true,
        };
        if !resolved {
            return Err(FindingParseError::UnresolvedTarget {
                line_number,
                detail: target_unresolved_detail(&self.target),
                raw: raw_line.to_owned(),
            });
        }
        Ok(())
    }
}

fn invalid_style_rule_subject_reason(subject: &str) -> Option<&'static str> {
    let trimmed = subject.trim();
    if trimmed.is_empty() {
        return Some("empty subject");
    }
    if trimmed.chars().all(|c| c.is_ascii_digit()) {
        return Some("bare line number");
    }
    None
}

fn target_unresolved_detail(target: &FindingTarget) -> String {
    match target {
        FindingTarget::Criterion { spec, anchor } => {
            format!("criterion `{anchor}` not present in spec `{spec}`")
        }
        FindingTarget::Annotation { target_string } => {
            format!("annotation target `{target_string}` does not resolve")
        }
        FindingTarget::TestPath { path } | FindingTarget::Template { path } => {
            format!("file `{path}` not found on disk")
        }
        FindingTarget::LockSite { file, line } => {
            format!("lock-site file `{file}` (line {line}) not found on disk")
        }
        FindingTarget::Invariant { spec, section, tag } => {
            format!("invariant `{tag}` in section `{section}` not present in spec `{spec}`",)
        }
        FindingTarget::MatrixCell {
            spec,
            partial,
            template,
        } => format!("matrix cell `{partial}` / `{template}` not present in spec `{spec}`"),
        FindingTarget::SurfaceElement {
            spec,
            element_kind,
            name,
        } => {
            format!("surface element `{element_kind}` / `{name}` not present in spec `{spec}`")
        }
        FindingTarget::Contract { .. } | FindingTarget::StyleRule { .. } => {
            "no resolver registered for this variant".to_owned()
        }
    }
}

/// Review-walk malformation variants surfaced by the verdict gate when the
/// terminal `LOOM_CONCERN:` payload fails to parse or the
/// `LOOM_FINDING:` stream and terminator disagree. Mirrors the
/// `RecoveryCause::BadWalk(BadWalk)` wrapped pattern that
/// `RecoveryCause::ReviewConcern(ReviewFlag)` already uses at the workflow
/// layer (per `specs/templates.md` § Typed `PreviousFailure`).
///
/// Each variant carries the **maximum well-formed context** by struct
/// shape per `specs/gate.md` § *Maximum-context preservation invariant*
/// — the failure mode "lost the agent's diagnosis when one piece of the
/// walk was malformed" is structurally unrepresentable.
///
/// Constructing [`BadWalk::Concern`] without `parsed_findings` is a
/// compile error:
///
/// ```compile_fail
/// use loom_protocol::gate::BadWalk;
/// let _ = BadWalk::Concern { payload: String::new() };
/// ```
///
/// Constructing [`BadWalk::FindingsWithoutConcern`] without `findings`
/// is a compile error:
///
/// ```compile_fail
/// use loom_protocol::gate::BadWalk;
/// let _ = BadWalk::FindingsWithoutConcern { finding_count: 0 };
/// ```
///
/// Constructing [`BadWalk::MalformedFinding`] without `terminal` is a
/// compile error:
///
/// ```compile_fail
/// use loom_protocol::gate::BadWalk;
/// let _ = BadWalk::MalformedFinding { errors: Vec::new() };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BadWalk {
    /// `LOOM_CONCERN:` payload did not parse as
    /// `{"summary": "<non-empty>"}` — invalid JSON, missing
    /// `summary` field, or empty `summary`. The literal post-marker
    /// text is preserved for the recovery prompt, alongside any
    /// `LOOM_FINDING:` records that streamed cleanly before the bad
    /// terminator.
    Concern {
        payload: String,
        parsed_findings: Vec<Finding>,
    },

    /// Terminator claimed concern but zero `LOOM_FINDING:` records
    /// streamed during the walk. The parsed summary is preserved
    /// so the recovery prompt can quote it back.
    ConcernWithoutFindings { summary: String },

    /// One or more `LOOM_FINDING:` records streamed but the
    /// terminator was `LOOM_COMPLETE`. The parsed findings ride
    /// through so the next iteration's prompt can name them
    /// per the pairing-rule table in `specs/gate.md`.
    FindingsWithoutConcern {
        finding_count: usize,
        findings: Vec<Finding>,
    },

    /// One or more `LOOM_FINDING:` records failed strict validation.
    /// The well-formed terminal surface rides through alongside the
    /// per-record errors so the recovery prompt can name both pieces
    /// (when the terminator was also malformed, it is preserved via
    /// `TerminalSurface::Malformed { payload }`).
    MalformedFinding {
        errors: Vec<FindingParseError>,
        terminal: TerminalSurface,
    },
}

/// Typed terminal surface a review walk left behind. Mirrors
/// [`ExitSignal`]'s well-formed variants and adds
/// [`TerminalSurface::Malformed`] for the terminal-marker parse-failure
/// case and [`TerminalSurface::Missing`] for the absent-terminator case,
/// so `BadWalk::MalformedFinding { terminal, .. }` can carry every
/// possible terminal shape by struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalSurface {
    /// `LOOM_COMPLETE` on the final non-empty line.
    Complete,
    /// `LOOM_NOOP` on the final non-empty line.
    Noop,
    /// `LOOM_BLOCKED` on the final non-empty line; `reason` is the
    /// non-empty adjacent prose read by the parser.
    Blocked { reason: String },
    /// `LOOM_CLARIFY` on the final non-empty line; `question` is the
    /// adjacent prose read by the parser.
    Clarify { question: String },
    /// `LOOM_RETRY` on the final non-empty line; `reason` is the
    /// adjacent prose the parser captured verbatim. Worker-phase-only
    /// per `specs/harness.md` § Marker definitions — the verdict gate
    /// rejects this in interactive phases as `wrong-phase-marker`.
    Retry { reason: String },
    /// `LOOM_CONCERN: {"summary": "..."}` parsed cleanly.
    Concern { summary: String },
    /// `LOOM_CONCERN:` was present but its JSON payload failed parse
    /// (invalid JSON, missing `summary`, or empty `summary`). The
    /// literal post-marker text is preserved.
    Malformed { payload: String },
    /// No terminator on the final non-empty line.
    Missing,
}

impl TerminalSurface {
    /// Canonical marker identity used by route observability.
    #[must_use]
    pub const fn identity(&self) -> &'static str {
        match self {
            Self::Complete => "LOOM_COMPLETE",
            Self::Noop => "LOOM_NOOP",
            Self::Blocked { .. } => "LOOM_BLOCKED",
            Self::Clarify { .. } => "LOOM_CLARIFY",
            Self::Retry { .. } => "LOOM_RETRY",
            Self::Concern { .. } | Self::Malformed { .. } => "LOOM_CONCERN",
            Self::Missing => "missing",
        }
    }

    /// Stable rendering used in `BadWalk::MalformedFinding` recovery
    /// prompts so the agent sees what the terminal looked like alongside
    /// the per-finding errors.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Complete => "LOOM_COMPLETE".to_owned(),
            Self::Noop => "LOOM_NOOP".to_owned(),
            Self::Blocked { .. } => "LOOM_BLOCKED".to_owned(),
            Self::Clarify { .. } => "LOOM_CLARIFY".to_owned(),
            Self::Retry { .. } => "LOOM_RETRY".to_owned(),
            Self::Concern { summary } => format!("LOOM_CONCERN: {summary}"),
            Self::Malformed { payload } => format!("LOOM_CONCERN: <malformed: {payload}>"),
            Self::Missing => "(no terminal on the final non-empty line)".to_owned(),
        }
    }
}

/// Parsed exit signal from an agent session — the trailing line the agent
/// emits to signal the gate's verdict.
///
/// Markers are **mutually exclusive** and live on the final non-empty line
/// of the agent's last assistant message. [`parse_exit_signal`] enforces
/// the mechanical half of that rule: only the final line is inspected, and
/// a final line carrying more than one marker is treated as a
/// swallowed-marker (returned as `None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitSignal {
    /// Agent finished cleanly; the driver advances per-spec cursors and
    /// commits the spec file.
    Complete,

    /// Agent finished cleanly but the phase intentionally produced an
    /// empty diff — the work was already done. Without this signal an
    /// empty diff is treated as zero-progress.
    Noop,

    /// Agent could not proceed; the driver surfaces the non-empty reason
    /// to the user without advancing state.
    Blocked { reason: String },

    /// Agent needs human input; the driver applies the `loom:clarify`
    /// label and bails.
    Clarify { question: String },

    /// Worker-phase self-report: this attempt cannot finish but a fresh
    /// dispatch is likely to succeed (environmental failure or agent
    /// self-reset per `specs/harness.md` § Marker definitions). `reason`
    /// is the prose the agent wrote on the line preceding the marker,
    /// captured verbatim. The verdict gate maps this to
    /// `RecoveryCause::AgentRetry` and routes through the existing
    /// `[loop] max_retries` counter; consecutive `LOOM_RETRY` exits that
    /// exhaust the counter escalate to `loom:blocked` with cause
    /// `retry-exhausted`. Worker-phase-only — emitting `LOOM_RETRY` from
    /// an interactive phase is a `wrong-phase-marker` error.
    Retry { reason: String },

    /// Review-phase concern. Carries the parsed `summary` field from the
    /// terminal `LOOM_CONCERN: {"summary": "..."}` marker. The summary is
    /// for the verdict log only; per-finding routing is decided on each
    /// streamed `LOOM_FINDING:` record's token per `specs/gate.md` §
    /// LOOM_CONCERN payload. Review-phase-only — emitting `LOOM_CONCERN`
    /// from any other phase is a `wrong-phase-marker` error in the verdict
    /// gate per `specs/harness.md` § Marker definitions.
    Concern { summary: String },

    /// Review walk's terminal `LOOM_CONCERN:` payload was malformed —
    /// invalid JSON, missing `summary`, or empty `summary`. Wraps the
    /// typed [`BadWalk`] variant so the verdict gate routes to
    /// `RecoveryCause::BadWalk` per `specs/gate.md` § LOOM_CONCERN payload
    /// — JSON shape and parse discipline. Only the
    /// [`BadWalk::Concern`] sub-variant is produced here; the
    /// stream/terminator pairing-rule variants are owned by the verdict
    /// gate.
    BadWalk(BadWalk),
}

impl ExitSignal {
    /// Canonical marker identity used by route observability.
    #[must_use]
    pub const fn identity(&self) -> &'static str {
        match self {
            Self::Complete => "LOOM_COMPLETE",
            Self::Noop => "LOOM_NOOP",
            Self::Blocked { .. } => "LOOM_BLOCKED",
            Self::Clarify { .. } => "LOOM_CLARIFY",
            Self::Retry { .. } => "LOOM_RETRY",
            Self::Concern { .. } | Self::BadWalk(_) => "LOOM_CONCERN",
        }
    }
}

const COMPLETE: &str = "LOOM_COMPLETE";
const NOOP: &str = "LOOM_NOOP";
const BLOCKED: &str = "LOOM_BLOCKED";
const CLARIFY: &str = "LOOM_CLARIFY";
const RETRY: &str = "LOOM_RETRY";
const CONCERN: &str = "LOOM_CONCERN";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalMarker {
    Complete,
    Noop,
    Blocked,
    Clarify,
    Retry,
    Concern,
}

impl TerminalMarker {
    const ALL: [(Self, &'static str); 6] = [
        (Self::Complete, COMPLETE),
        (Self::Noop, NOOP),
        (Self::Blocked, BLOCKED),
        (Self::Clarify, CLARIFY),
        (Self::Retry, RETRY),
        (Self::Concern, CONCERN),
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MarkerMatch {
    marker: TerminalMarker,
    start: usize,
    end: usize,
}

/// Scan the agent's combined output (or the `result` field of the final
/// stream-json line) for an exit signal.
///
/// The parser inspects **only the final non-empty line** of `output`. Any
/// marker emitted earlier in the session is treated as swallowed; multiple
/// markers on the final line likewise collapse to `None` per the
/// mutual-exclusivity rule in `specs/harness.md` § Marker definitions.
/// Marker-shaped text inside a quoted JSON string is payload, not a terminal.
///
/// `LOOM_BLOCKED` and `LOOM_CLARIFY` are bare markers — no trailing colon,
/// no trailing payload. The reason / question is read from the text
/// **before** the marker on the final line, falling back to the most recent
/// non-empty line before the final line if the same-line prefix is empty.
/// `LOOM_BLOCKED` is accepted only when that captured reason is non-empty.
///
/// `LOOM_CONCERN` carries a JSON payload on the final line:
/// `LOOM_CONCERN: {"summary": "<non-empty string>"}`. A well-formed payload
/// surfaces as [`ExitSignal::Concern`]; a malformed payload (invalid JSON,
/// missing `summary`, or empty `summary`) surfaces as
/// [`ExitSignal::BadWalk`] carrying [`BadWalk::Concern`] with the literal
/// post-marker text so the verdict gate can route to recovery without
/// silently collapsing.
///
/// `None` means no signal was found on the final line and the caller
/// should surface the equivalent swallowed-marker recovery cause.
pub fn parse_exit_signal(output: &str) -> Option<ExitSignal> {
    let lines: Vec<&str> = output.lines().collect();
    let final_idx = lines.iter().rposition(|line| !line.trim().is_empty())?;
    let final_line = lines[final_idx];
    let prior = &lines[..final_idx];

    let markers = terminal_markers(final_line);
    let [terminal] = markers.as_slice() else {
        return None;
    };

    match terminal.marker {
        TerminalMarker::Complete => Some(ExitSignal::Complete),
        TerminalMarker::Noop => Some(ExitSignal::Noop),
        TerminalMarker::Blocked => required_reason_at(terminal.start, final_line, prior)
            .map(|reason| ExitSignal::Blocked { reason }),
        TerminalMarker::Clarify => Some(ExitSignal::Clarify {
            question: reason_at(terminal.start, final_line, prior),
        }),
        TerminalMarker::Retry => Some(ExitSignal::Retry {
            reason: reason_at(terminal.start, final_line, prior),
        }),
        TerminalMarker::Concern => Some(parse_concern(&final_line[terminal.end..])),
    }
}

fn terminal_markers(line: &str) -> Vec<MarkerMatch> {
    let mut markers = Vec::new();
    let mut in_string = false;
    let mut escaping = false;
    for (start, ch) in line.char_indices() {
        if in_string {
            if escaping {
                escaping = false;
            } else if ch == '\\' {
                escaping = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            continue;
        }
        for (marker, token) in TerminalMarker::ALL {
            if line[start..].starts_with(token) {
                markers.push(MarkerMatch {
                    marker,
                    start,
                    end: start + token.len(),
                });
            }
        }
    }
    markers
}

fn final_line_contains_marker(output: &str, marker: TerminalMarker) -> bool {
    output
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| {
            terminal_markers(line)
                .iter()
                .any(|candidate| candidate.marker == marker)
        })
}

#[derive(Deserialize)]
struct ConcernPayload {
    summary: String,
}

/// Parse the post-`LOOM_CONCERN` payload into a typed [`ExitSignal`].
///
/// Returns [`ExitSignal::Concern`] with the JSON-parsed `summary` when the
/// payload is a well-formed `{"summary": "<non-empty>"}` object. Any parse
/// failure (invalid JSON, missing field, empty summary) returns
/// [`ExitSignal::BadWalk`] carrying [`BadWalk::Concern`] with the literal
/// post-marker text so the verdict gate can render the recovery prompt
/// with the original payload intact.
fn parse_concern(after_marker: &str) -> ExitSignal {
    let payload = after_marker.trim_start_matches(':').trim();
    match serde_json::from_str::<ConcernPayload>(payload) {
        Ok(body) if !body.summary.is_empty() => ExitSignal::Concern {
            summary: body.summary,
        },
        _ => ExitSignal::BadWalk(BadWalk::Concern {
            payload: payload.to_string(),
            parsed_findings: Vec::new(),
        }),
    }
}

fn required_reason_at(marker_start: usize, line: &str, prior: &[&str]) -> Option<String> {
    let reason = reason_at(marker_start, line, prior);
    (!reason.is_empty()).then_some(reason)
}

fn reason_at(marker_start: usize, line: &str, prior: &[&str]) -> String {
    let same_line = line[..marker_start].trim();
    if !same_line.is_empty() {
        return same_line.to_string();
    }
    prior
        .iter()
        .rev()
        .find_map(|line| {
            let trimmed = line.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
        .unwrap_or_default()
}

/// Top-level error for [`parse_walk_output`]. Either a per-record
/// validation failure or the terminal-marker enforcement — a walk that
/// emits `LOOM_FINDING:` records without a terminal marker per
/// `specs/gate.md` § *Findings and Minting*.
#[derive(Debug, Display, Error)]
pub enum WalkOutputError {
    /// walk output contained an invalid LOOM_FINDING record
    Finding(#[from] FindingParseError),
    /// walk output violated the LOOM_FINDING / terminal-marker pairing rule: {bad_walk:?}
    BadWalk { bad_walk: BadWalk },
    /// review walk emitted direct LOOM_CLARIFY; emit a route="clarify" LOOM_FINDING with Options evidence and LOOM_CONCERN instead (question: {question})
    WrongReviewPath { question: String },
    /// review walk could not complete: {marker} {reason}
    CannotComplete {
        marker: &'static str,
        reason: String,
    },
    /// review walk used invalid terminal {marker}; expected LOOM_COMPLETE / LOOM_CONCERN / LOOM_RETRY / LOOM_BLOCKED
    InvalidTerminal { marker: &'static str },
    /// walk emitted {findings_count} LOOM_FINDING record(s) but no terminal marker (LOOM_COMPLETE / LOOM_CONCERN)
    MissingTerminalMarker { findings_count: usize },
}

/// Typed product the review-phase classifier consumes — a single
/// pre-parsed snapshot of the agent's stdout containing the typed
/// terminal surface, the well-formed [`Finding`] records, and any
/// per-record parse errors.
///
/// `WalkOutput`'s fields are private at the `loom-protocol` crate
/// boundary. The silent-loss failure class — production caller
/// constructs `WalkOutput` with bogus fields, bypassing the typed parse
/// pipeline — is structurally unrepresentable via field-privacy per
/// `specs/gate.md` § *Structural enforcement* and the
/// `walk_output_fields_private_only_constructor_is_from_stdout`
/// criterion. [`Self::from_stdout`] is the only construction path;
/// consumers read state via the [`Self::terminal`] / [`Self::findings`] /
/// [`Self::finding_errors`] accessors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkOutput {
    /// Typed terminal surface read from the final non-empty line of
    /// stdout — well-formed [`ExitSignal`] variants surface as the
    /// corresponding [`TerminalSurface`] variant; a malformed
    /// `LOOM_CONCERN:` payload surfaces as
    /// [`TerminalSurface::Malformed`]; absence of any marker surfaces
    /// as [`TerminalSurface::Missing`].
    terminal: TerminalSurface,
    /// Findings that passed strict per-layer validation. Order
    /// preserves stdout emission order.
    findings: Vec<Finding>,
    /// Per-record parse failures for `LOOM_FINDING:` substring matches
    /// that did not pass strict validation. Carries the offending
    /// 1-based start line and verbatim record text so the recovery
    /// prompt can quote it back.
    finding_errors: Vec<FindingParseError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RawFindingRecord {
    line_number: usize,
    raw: String,
    payload: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CapturedRecord {
    record: RawFindingRecord,
    next_offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JsonObjectScan {
    end: usize,
    normalized: String,
}

fn finding_records(output: &str) -> Vec<RawFindingRecord> {
    let mut records = Vec::new();
    let mut offset = 0;
    while let Some(relative_start) = output[offset..].find(LOOM_FINDING_PREFIX) {
        let prefix_start = offset + relative_start;
        let line_number = line_number_at(output, prefix_start);
        let line_start = line_start_at(output, prefix_start);
        let payload_start = prefix_start + LOOM_FINDING_PREFIX.len();
        let object_start = skip_horizontal_whitespace(output, payload_start);
        let captured = if output[object_start..].starts_with('{') {
            capture_object_record(output, line_start, payload_start, object_start, line_number)
        } else {
            capture_line_record(output, line_start, payload_start, line_number)
        };
        offset = captured.next_offset;
        records.push(captured.record);
    }
    records
}

fn capture_object_record(
    output: &str,
    line_start: usize,
    payload_start: usize,
    object_start: usize,
    line_number: usize,
) -> CapturedRecord {
    let line_end = line_end_after(output, payload_start);
    let line_payload = output[payload_start..line_end].trim_start();
    if !payload_needs_multiline_scan(line_payload) {
        return capture_line_record(output, line_start, payload_start, line_number);
    }
    let Some(scan) = scan_json_object(output, object_start) else {
        return capture_line_record(output, line_start, payload_start, line_number);
    };
    let line_end = line_end_after(output, scan.end);
    let trailing = &output[scan.end..line_end];
    let payload = if trailing.trim().is_empty() {
        scan.normalized
    } else {
        output[object_start..line_end].to_owned()
    };
    CapturedRecord {
        record: RawFindingRecord {
            line_number,
            raw: output[line_start..line_end].to_owned(),
            payload,
        },
        next_offset: next_line_offset(output, line_end),
    }
}

fn capture_line_record(
    output: &str,
    line_start: usize,
    payload_start: usize,
    line_number: usize,
) -> CapturedRecord {
    let line_end = line_end_after(output, payload_start);
    CapturedRecord {
        record: RawFindingRecord {
            line_number,
            raw: output[line_start..line_end].to_owned(),
            payload: output[payload_start..line_end].trim_start().to_owned(),
        },
        next_offset: next_line_offset(output, line_end),
    }
}

fn payload_needs_multiline_scan(payload: &str) -> bool {
    match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(_) => false,
        Err(err) => matches!(err.classify(), serde_json::error::Category::Eof),
    }
}

fn scan_json_object(output: &str, object_start: usize) -> Option<JsonObjectScan> {
    let mut normalized = String::new();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaping = false;
    let mut chars = output[object_start..].char_indices().peekable();
    while let Some((relative_index, ch)) = chars.next() {
        let absolute_index = object_start + relative_index;
        if in_string {
            if escaping {
                normalized.push(ch);
                escaping = false;
                continue;
            }
            match ch {
                '\\' => {
                    normalized.push(ch);
                    escaping = true;
                }
                '"' => {
                    normalized.push(ch);
                    in_string = false;
                }
                '\r' => {
                    normalized.push_str("\\n");
                    if chars.peek().is_some_and(|(_, next)| *next == '\n') {
                        chars.next();
                    }
                }
                '\n' => normalized.push_str("\\n"),
                c if c.is_control() => push_control_escape(&mut normalized, c),
                c => normalized.push(c),
            }
            continue;
        }
        match ch {
            '"' => {
                normalized.push(ch);
                in_string = true;
            }
            '{' => {
                normalized.push(ch);
                depth += 1;
            }
            '}' => {
                if depth == 0 {
                    return None;
                }
                normalized.push(ch);
                depth -= 1;
                if depth == 0 {
                    return Some(JsonObjectScan {
                        end: absolute_index + ch.len_utf8(),
                        normalized,
                    });
                }
            }
            c => normalized.push(c),
        }
    }
    None
}

fn push_control_escape(out: &mut String, ch: char) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let code = ch as u32;
    out.push_str("\\u00");
    out.push(HEX[((code >> 4) & 0x0f) as usize] as char);
    out.push(HEX[(code & 0x0f) as usize] as char);
}

fn line_number_at(output: &str, byte_index: usize) -> usize {
    output[..byte_index].bytes().filter(|b| *b == b'\n').count() + 1
}

fn line_start_at(output: &str, byte_index: usize) -> usize {
    output[..byte_index]
        .rfind('\n')
        .map_or(0, |newline| newline + 1)
}

fn line_end_after(output: &str, byte_index: usize) -> usize {
    output[byte_index..]
        .find('\n')
        .map_or(output.len(), |relative_end| byte_index + relative_end)
}

fn next_line_offset(output: &str, line_end: usize) -> usize {
    if line_end < output.len() {
        line_end + 1
    } else {
        line_end
    }
}

fn skip_horizontal_whitespace(output: &str, byte_index: usize) -> usize {
    let mut index = byte_index;
    while let Some(ch) = output[index..].chars().next() {
        if ch != ' ' && ch != '\t' {
            return index;
        }
        index += ch.len_utf8();
    }
    index
}

impl WalkOutput {
    /// Parse the agent's combined stdout into a typed `WalkOutput`.
    /// Runs `LOOM_FINDING:` substring search, strict per-record
    /// validation against `validator`, and terminal-marker
    /// classification through [`parse_exit_signal`] — once, here, so
    /// downstream classifier code consumes the typed product and
    /// cannot accidentally re-derive it from `&str`.
    ///
    /// The struct's fields are private at the crate boundary; this is
    /// the only construction path. Consumers read state through the
    /// [`Self::terminal`] / [`Self::findings`] / [`Self::finding_errors`]
    /// accessors.
    pub fn from_stdout<V: FindingValidator + ?Sized>(
        output: &str,
        scope: DispatchScope,
        validator: &V,
    ) -> Self {
        let mut findings = Vec::new();
        let mut finding_errors = Vec::new();
        for record in finding_records(output) {
            match Finding::parse_payload(&record.payload, record.line_number, &record.raw, scope)
                .and_then(|f| {
                    f.validate(record.line_number, &record.raw, validator)
                        .map(|()| f)
                }) {
                Ok(finding) => findings.push(finding),
                Err(e) => finding_errors.push(e),
            }
        }
        let terminal = terminal_surface_from_stdout(output);
        Self {
            terminal,
            findings,
            finding_errors,
        }
    }

    /// Typed terminal surface read from the final non-empty line of
    /// the parsed stdout.
    #[must_use]
    pub fn terminal(&self) -> &TerminalSurface {
        &self.terminal
    }

    /// Well-formed findings, in stdout emission order.
    #[must_use]
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    /// Per-record parse failures for `LOOM_FINDING:` substring matches
    /// that did not pass strict validation.
    #[must_use]
    pub fn finding_errors(&self) -> &[FindingParseError] {
        &self.finding_errors
    }
}

/// Resolve the typed [`TerminalSurface`] from the agent's combined
/// stdout. Well-formed terminal markers route through
/// [`parse_exit_signal`] so the parser surface is shared with the
/// `LOOM_FINDING:` stream check; a malformed terminal surfaces as
/// [`TerminalSurface::Malformed`]; absence surfaces as
/// [`TerminalSurface::Missing`].
fn terminal_surface_from_stdout(output: &str) -> TerminalSurface {
    match parse_exit_signal(output) {
        Some(ExitSignal::Complete) => TerminalSurface::Complete,
        Some(ExitSignal::Noop) => TerminalSurface::Noop,
        Some(ExitSignal::Blocked { reason }) => TerminalSurface::Blocked { reason },
        Some(ExitSignal::Clarify { question }) => TerminalSurface::Clarify { question },
        Some(ExitSignal::Retry { reason }) => TerminalSurface::Retry { reason },
        Some(ExitSignal::Concern { summary }) => TerminalSurface::Concern { summary },
        Some(ExitSignal::BadWalk(BadWalk::Concern { payload, .. })) => {
            TerminalSurface::Malformed { payload }
        }
        Some(ExitSignal::BadWalk(_)) | None => TerminalSurface::Missing,
    }
}

/// Scan `output` for `LOOM_FINDING:` records, parse and fully-validate
/// each (Layers 1–5 plus the `target.spec ∈ bonds` rule), then enforce
/// the review-walk pairing rule before returning findings to mint.
/// Findings may reach mint only when the raw stream and terminal agree:
/// zero findings with `LOOM_COMPLETE` is clean, and one or more
/// findings require a well-formed `LOOM_CONCERN` terminal.
///
/// Findings interleave with markers in stdout order — the returned
/// vector preserves emission order, and the terminal-marker check
/// reads through [`parse_exit_signal`] so the parser surface used by
/// `LOOM_CONCERN` / `LOOM_COMPLETE` is the same one consulted here
/// (no separate channel).
pub fn parse_walk_output<V: FindingValidator + ?Sized>(
    output: &str,
    scope: DispatchScope,
    validator: &V,
) -> Result<Vec<Finding>, WalkOutputError> {
    let walk = WalkOutput::from_stdout(output, scope, validator);
    if !walk.finding_errors().is_empty() {
        return Err(WalkOutputError::BadWalk {
            bad_walk: BadWalk::MalformedFinding {
                errors: walk.finding_errors().to_vec(),
                terminal: walk.terminal().clone(),
            },
        });
    }

    match walk.terminal() {
        TerminalSurface::Clarify { question } => Err(WalkOutputError::WrongReviewPath {
            question: question.clone(),
        }),
        TerminalSurface::Concern { summary } if walk.findings().is_empty() => {
            Err(WalkOutputError::BadWalk {
                bad_walk: BadWalk::ConcernWithoutFindings {
                    summary: summary.clone(),
                },
            })
        }
        TerminalSurface::Concern { .. } => Ok(walk.findings().to_vec()),
        TerminalSurface::Malformed { payload } => Err(WalkOutputError::BadWalk {
            bad_walk: BadWalk::Concern {
                payload: payload.clone(),
                parsed_findings: walk.findings().to_vec(),
            },
        }),
        TerminalSurface::Noop => Err(WalkOutputError::InvalidTerminal { marker: NOOP }),
        TerminalSurface::Complete if !walk.findings().is_empty() => Err(WalkOutputError::BadWalk {
            bad_walk: BadWalk::FindingsWithoutConcern {
                finding_count: walk.findings().len(),
                findings: walk.findings().to_vec(),
            },
        }),
        TerminalSurface::Blocked { .. } | TerminalSurface::Retry { .. }
            if !walk.findings().is_empty() =>
        {
            Err(WalkOutputError::BadWalk {
                bad_walk: BadWalk::FindingsWithoutConcern {
                    finding_count: walk.findings().len(),
                    findings: walk.findings().to_vec(),
                },
            })
        }
        TerminalSurface::Missing if final_line_contains_marker(output, TerminalMarker::Blocked) => {
            Err(WalkOutputError::InvalidTerminal { marker: BLOCKED })
        }
        TerminalSurface::Missing if !walk.findings().is_empty() => {
            Err(WalkOutputError::MissingTerminalMarker {
                findings_count: walk.findings().len(),
            })
        }
        TerminalSurface::Blocked { reason } => Err(WalkOutputError::CannotComplete {
            marker: BLOCKED,
            reason: reason.clone(),
        }),
        TerminalSurface::Retry { reason } => Err(WalkOutputError::CannotComplete {
            marker: RETRY,
            reason: reason.clone(),
        }),
        TerminalSurface::Missing | TerminalSurface::Complete => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(s: &str) -> SpecLabel {
        s.parse().expect("valid spec label")
    }

    fn finding(
        token: ConcernToken,
        bonds: Vec<SpecLabel>,
        target: FindingTarget,
        evidence: &str,
    ) -> Finding {
        Finding {
            token,
            route: FindingRoute::Deferred,
            bonds,
            target,
            evidence: evidence.to_owned(),
        }
    }

    fn sample_finding() -> Finding {
        finding(
            ConcernToken::SpecCoherenceFail,
            vec![spec("gate")],
            FindingTarget::Criterion {
                spec: spec("gate"),
                anchor: "verifier-honesty".to_owned(),
            },
            "criterion's annotation does not exercise the contract",
        )
    }

    #[test]
    fn finding_hash_is_versioned_twelve_lowercase_hex_chars() {
        let hash = sample_finding().hash();
        let Some(body) = hash.strip_prefix("v1:") else {
            panic!("hash carries identity-version prefix: {hash}");
        };
        assert_eq!(body.len(), 12, "hash body width is 12 hex chars");
        assert!(
            body.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "hash body is lowercase hex: {hash}",
        );
    }

    #[test]
    fn mint_computes_versioned_finding_id_excluding_volatile_context() {
        let a = finding(
            ConcernToken::SpecCoherenceFail,
            vec![spec("gate"), spec("harness")],
            FindingTarget::Criterion {
                spec: spec("gate"),
                anchor: "Verifier Honesty".to_owned(),
            },
            "first walk phrasing",
        );
        let b = finding(
            ConcernToken::SpecCoherenceFail,
            vec![spec("harness"), spec("gate")],
            FindingTarget::Criterion {
                spec: spec("gate"),
                anchor: "verifier-honesty".to_owned(),
            },
            "second walk phrasing, identical identity",
        );
        assert_eq!(
            a.id(),
            "v1:criterion:spec-coherence-fail:gate#verifier-honesty"
        );
        assert_eq!(a.id(), b.id());
        assert_eq!(a.hash(), b.hash());
    }

    #[test]
    fn finding_identity_excludes_bonds_for_target_centred_contracts() {
        let identity = (
            ConcernToken::OrphanIntegration,
            FindingTarget::Contract {
                id: "Molecule Lifecycle".to_owned(),
            },
        );
        let single_spec = finding(identity.0, vec![spec("harness")], identity.1.clone(), "");
        let multi_spec = finding(
            identity.0,
            vec![spec("gate"), spec("harness")],
            identity.1,
            "",
        );
        assert_eq!(single_spec.id(), "v1:contract:molecule-lifecycle");
        assert_eq!(single_spec.id(), multi_spec.id());
        assert_eq!(single_spec.hash(), multi_spec.hash());
    }

    #[test]
    fn canonical_form_per_variant_matches_spec_shapes() {
        assert_eq!(
            FindingTarget::Criterion {
                spec: spec("gate"),
                anchor: "verifier-honesty".to_owned(),
            }
            .canonical_form(),
            "criterion:gate:verifier-honesty",
        );
        assert_eq!(
            FindingTarget::Contract {
                id: "molecule-lifecycle".to_owned(),
            }
            .canonical_form(),
            "contract:molecule-lifecycle",
        );
        assert_eq!(
            FindingTarget::StyleRule {
                rule_id: "RS-12".to_owned(),
                subject: "crates/loom-gate/src/integrity.rs".to_owned(),
            }
            .canonical_form(),
            "style:RS-12:crates/loom-gate/src/integrity.rs",
        );
        assert_eq!(
            FindingTarget::MatrixCell {
                spec: spec("gate"),
                partial: "findings_walk".to_owned(),
                template: "review".to_owned(),
            }
            .canonical_form(),
            "matrix-cell:gate:findings_walk:review",
        );
        assert_eq!(
            FindingTarget::SurfaceElement {
                spec: spec("gate"),
                element_kind: "command".to_owned(),
                name: "loom gate verify".to_owned(),
            }
            .canonical_form(),
            "surface-element:gate:command:loom gate verify",
        );
    }

    #[test]
    fn concern_token_serde_uses_wire_kebab_case() {
        let token = ConcernToken::SpecCoherenceFail;
        let json = serde_json::to_string(&token).expect("serialize");
        assert_eq!(json, "\"spec-coherence-fail\"");
        let back: ConcernToken = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, token);
    }

    #[test]
    fn finding_target_serde_uses_tagged_kind_discriminator() {
        let target = FindingTarget::Criterion {
            spec: spec("gate"),
            anchor: "verifier-honesty".to_owned(),
        };
        let json = serde_json::to_value(&target).expect("serialize");
        assert_eq!(json["kind"], "Criterion");
        assert_eq!(json["spec"], "gate");
        assert_eq!(json["anchor"], "verifier-honesty");
        let back: FindingTarget = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, target);
    }

    #[test]
    fn concern_token_expected_target_kind_table() {
        for (token, expected) in [
            (ConcernToken::SpecCoherenceFail, TargetKind::Criterion),
            (ConcernToken::OrphanIntegration, TargetKind::Contract),
            (ConcernToken::StyleRuleViolation, TargetKind::StyleRule),
            (ConcernToken::VerifierBypass, TargetKind::Annotation),
            (ConcernToken::WeakAssertion, TargetKind::Annotation),
            (ConcernToken::FabricatedResult, TargetKind::Annotation),
            (ConcernToken::CoincidentalPass, TargetKind::Annotation),
            (ConcernToken::MockDiscipline, TargetKind::TestPath),
            (ConcernToken::VerifierTooNarrow, TargetKind::Criterion),
            (ConcernToken::ConcurrencyUntested, TargetKind::LockSite),
            (ConcernToken::JudgeFlag, TargetKind::Criterion),
            (ConcernToken::InvariantClash, TargetKind::Invariant),
            (ConcernToken::TemplateSpecDrift, TargetKind::Template),
            (ConcernToken::CrossSpecClash, TargetKind::Criterion),
            (
                ConcernToken::SpecConventionsViolation,
                TargetKind::Criterion,
            ),
            (ConcernToken::VerifierFailed, TargetKind::Annotation),
            (ConcernToken::DispatchError, TargetKind::Annotation),
            (ConcernToken::UnresolvedAnnotation, TargetKind::Annotation),
            (ConcernToken::StubPointing, TargetKind::Annotation),
            (ConcernToken::MultipleAnnotations, TargetKind::Criterion),
            (ConcernToken::UnneededPendingMarker, TargetKind::Annotation),
            (ConcernToken::InputsProtocolError, TargetKind::Annotation),
            (ConcernToken::ScopeCreep, TargetKind::Criterion),
            (ConcernToken::ScopeShortfall, TargetKind::Criterion),
            (ConcernToken::PendingMarkerResolved, TargetKind::MatrixCell),
        ] {
            assert_eq!(token.expected_target_kind(), expected, "{token:?}");
        }
    }

    #[test]
    fn pending_marker_resolved_allows_matrix_cell_and_surface_element_targets() {
        let token = ConcernToken::PendingMarkerResolved;
        assert_eq!(token.as_wire(), "pending-marker-resolved");
        assert!(token.allows_target_kind(TargetKind::MatrixCell));
        assert!(token.allows_target_kind(TargetKind::SurfaceElement));
        assert!(!token.allows_target_kind(TargetKind::Annotation));
    }

    /// Field-private seal pin per
    /// `walk_output_fields_private_only_constructor_is_from_stdout`:
    /// [`WalkOutput::from_stdout`] is the only construction path, and
    /// consumers read state through accessor methods. The struct
    /// literal `WalkOutput { .. }` is rejected at compile time outside
    /// the defining crate (`loom-protocol`).
    #[test]
    fn walk_output_constructed_only_via_from_stdout() {
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
        let walk = WalkOutput::from_stdout("LOOM_COMPLETE\n", DispatchScope::Tree, &AlwaysValid);
        assert_eq!(walk.terminal(), &TerminalSurface::Complete);
        assert!(walk.findings().is_empty());
        assert!(walk.finding_errors().is_empty());
    }

    #[test]
    fn complete_on_bare_marker() {
        assert_eq!(
            parse_exit_signal("ok\nLOOM_COMPLETE\n"),
            Some(ExitSignal::Complete)
        );
    }

    #[test]
    fn noop_on_bare_marker() {
        assert_eq!(
            parse_exit_signal("already done\nLOOM_NOOP\n"),
            Some(ExitSignal::Noop)
        );
    }

    #[test]
    fn blocked_carries_reason_from_prior_line() {
        let out = "doing things\nspec is missing the requirements section\nLOOM_BLOCKED\n";
        match parse_exit_signal(out) {
            Some(ExitSignal::Blocked { reason }) => {
                assert_eq!(reason, "spec is missing the requirements section");
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn clarify_carries_question_from_prior_line() {
        let out = "should the migration be additive only?\nLOOM_CLARIFY";
        match parse_exit_signal(out) {
            Some(ExitSignal::Clarify { question }) => {
                assert_eq!(question, "should the migration be additive only?");
            }
            other => panic!("expected Clarify, got {other:?}"),
        }
    }

    #[test]
    fn retry_co_occurring_with_other_marker_swallowed() {
        let out = "LOOM_RETRY LOOM_BLOCKED\n";
        assert_eq!(parse_exit_signal(out), None);
    }

    #[test]
    fn terminal_surface_retry_label_round_trips() {
        let ts = TerminalSurface::Retry {
            reason: "tool exec broke".into(),
        };
        assert_eq!(ts.label(), "LOOM_RETRY");
    }

    #[test]
    fn no_signal_returns_none() {
        assert_eq!(
            parse_exit_signal("just some output\nno marker here\n"),
            None
        );
    }

    #[test]
    fn marker_recognized_inside_a_longer_line() {
        let out = "Final result: missing schema LOOM_BLOCKED\n";
        match parse_exit_signal(out) {
            Some(ExitSignal::Blocked { reason }) => {
                assert_eq!(reason, "Final result: missing schema");
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn blank_lines_between_reason_and_marker_are_skipped() {
        let out = "the actual reason\n\n\nLOOM_BLOCKED\n";
        match parse_exit_signal(out) {
            Some(ExitSignal::Blocked { reason }) => assert_eq!(reason, "the actual reason"),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn blocked_marker_without_prior_reason_is_not_a_valid_exit_signal() {
        let out = "LOOM_BLOCKED";
        assert_eq!(parse_exit_signal(out), None);
    }

    #[test]
    fn marker_on_non_final_line_is_swallowed() {
        let out = "LOOM_COMPLETE\nfollow-up prose that hides the marker\n";
        assert_eq!(parse_exit_signal(out), None);
    }

    #[test]
    fn multiple_markers_on_final_line_swallow_the_signal() {
        let out = "LOOM_BLOCKED LOOM_COMPLETE\n";
        assert_eq!(parse_exit_signal(out), None);
    }

    #[test]
    fn final_line_is_authoritative_when_prior_line_also_has_a_marker() {
        let out = "tentative\nLOOM_BLOCKED\nactually nevermind\nLOOM_COMPLETE";
        assert_eq!(parse_exit_signal(out), Some(ExitSignal::Complete));
    }

    #[test]
    fn concern_payload_parses_as_json_with_summary_field() {
        let out = r#"LOOM_CONCERN: {"summary": "verifier-bypass on the agent backend mock"}"#;
        match parse_exit_signal(out) {
            Some(ExitSignal::Concern { summary }) => {
                assert_eq!(summary, "verifier-bypass on the agent backend mock");
            }
            other => panic!("expected Concern, got {other:?}"),
        }
    }

    #[test]
    fn concern_payload_marker_names_are_data_not_terminal_markers() {
        let expected =
            "missing-marker path reported as LOOM_COMPLETE, not LOOM_BLOCKED or LOOM_RETRY";
        let out = format!(r#"LOOM_CONCERN: {{"summary":"{expected}"}}"#);
        match parse_exit_signal(&out) {
            Some(ExitSignal::Concern { summary }) => assert_eq!(summary, expected),
            other => panic!("expected Concern, got {other:?}"),
        }
    }

    #[test]
    fn concern_with_trailing_terminal_marker_is_rejected() {
        let out = r#"LOOM_CONCERN: {"summary":"scope drift"} LOOM_COMPLETE"#;
        assert_eq!(parse_exit_signal(out), None);
    }

    #[test]
    fn duplicate_same_terminal_marker_is_rejected() {
        assert_eq!(parse_exit_signal("LOOM_COMPLETE LOOM_COMPLETE"), None);
    }

    #[test]
    fn concern_trims_whitespace_around_payload() {
        let out = "LOOM_CONCERN:    {\"summary\":\"scope drift\"}   \n";
        match parse_exit_signal(out) {
            Some(ExitSignal::Concern { summary }) => assert_eq!(summary, "scope drift"),
            other => panic!("expected Concern, got {other:?}"),
        }
    }

    #[test]
    fn concern_malformed_payload_routes_to_bad_walk_concern_with_literal_payload() {
        let out = "LOOM_CONCERN: malformed payload with no separator\n";
        match parse_exit_signal(out) {
            Some(ExitSignal::BadWalk(BadWalk::Concern { payload, .. })) => {
                assert_eq!(payload, "malformed payload with no separator");
            }
            other => panic!("expected BadWalk::Concern, got {other:?}"),
        }
    }

    #[test]
    fn concern_with_empty_summary_routes_to_bad_walk_concern() {
        let out = r#"LOOM_CONCERN: {"summary": ""}"#;
        match parse_exit_signal(out) {
            Some(ExitSignal::BadWalk(BadWalk::Concern { payload, .. })) => {
                assert_eq!(payload, r#"{"summary": ""}"#);
            }
            other => panic!("expected BadWalk::Concern, got {other:?}"),
        }
    }

    #[test]
    fn concern_with_missing_summary_field_routes_to_bad_walk_concern() {
        let out = r#"LOOM_CONCERN: {"summery": "typo in the field name"}"#;
        match parse_exit_signal(out) {
            Some(ExitSignal::BadWalk(BadWalk::Concern { payload, .. })) => {
                assert_eq!(payload, r#"{"summery": "typo in the field name"}"#);
            }
            other => panic!("expected BadWalk::Concern, got {other:?}"),
        }
    }

    #[test]
    fn concern_on_non_final_line_is_swallowed() {
        let out = "LOOM_CONCERN: {\"summary\": \"scope drift\"}\nclosing prose\n";
        assert_eq!(parse_exit_signal(out), None);
    }

    #[test]
    fn legacy_review_flag_keyword_on_prior_line_does_not_shadow_final_complete() {
        let out =
            "LOOM_REVIEW_FLAG: verifier-bypass -- test mocks the agent backend\nLOOM_COMPLETE\n";
        assert_eq!(parse_exit_signal(out), Some(ExitSignal::Complete));
    }

    #[test]
    fn retry_carries_reason_from_prior_line() {
        let out = "tools failing mid-session, sandbox unlinked\nLOOM_RETRY\n";
        match parse_exit_signal(out) {
            Some(ExitSignal::Retry { reason }) => {
                assert_eq!(reason, "tools failing mid-session, sandbox unlinked");
            }
            other => panic!("expected Retry, got {other:?}"),
        }
    }

    #[test]
    fn retry_reads_reason_from_same_line_prefix() {
        let out = "prompt context exhausted LOOM_RETRY\n";
        match parse_exit_signal(out) {
            Some(ExitSignal::Retry { reason }) => {
                assert_eq!(reason, "prompt context exhausted");
            }
            other => panic!("expected Retry, got {other:?}"),
        }
    }

    #[test]
    fn retry_at_start_with_no_prior_lines_yields_empty_reason() {
        let out = "LOOM_RETRY";
        match parse_exit_signal(out) {
            Some(ExitSignal::Retry { reason }) => assert!(reason.is_empty()),
            other => panic!("expected Retry with empty reason, got {other:?}"),
        }
    }

    #[test]
    fn retry_with_other_marker_on_final_line_is_swallowed() {
        let out = "LOOM_RETRY LOOM_COMPLETE\n";
        assert_eq!(parse_exit_signal(out), None);
    }

    #[test]
    fn retry_with_blocked_on_final_line_is_swallowed() {
        let out = "LOOM_BLOCKED LOOM_RETRY\n";
        assert_eq!(parse_exit_signal(out), None);
    }

    #[test]
    fn retry_on_non_final_line_is_swallowed() {
        let out = "LOOM_RETRY\nfollow-up prose that hides the marker\n";
        assert_eq!(parse_exit_signal(out), None);
    }

    #[test]
    fn terminal_surface_retry_label_round_trips_to_loom_retry() {
        let surface = TerminalSurface::Retry {
            reason: "tools failing mid-session".to_owned(),
        };
        assert_eq!(surface.label(), "LOOM_RETRY");
    }

    #[test]
    fn walk_output_terminal_surfaces_retry_from_stdout() {
        struct AcceptAll;
        impl FindingValidator for AcceptAll {
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
        let walk = WalkOutput::from_stdout(
            "sandbox cwd unlinked\nLOOM_RETRY\n",
            DispatchScope::Tree,
            &AcceptAll,
        );
        match walk.terminal() {
            TerminalSurface::Retry { reason } => {
                assert_eq!(reason, "sandbox cwd unlinked");
            }
            other => panic!("expected Retry terminal, got {other:?}"),
        }
    }

    /// Spec contract `specs/templates.md` § Typed `PreviousFailure` —
    /// every [`BadWalk`] variant carries the maximum well-formed
    /// context by struct shape. The variants enumerated here cover
    /// every cell in the (stream-shape × terminal-shape) cross product
    /// from the maximum-context preservation invariant; the literal
    /// presence of `parsed_findings` / `findings` / `errors` +
    /// `terminal` in the field-init shorthand below is the structural
    /// pin — a future contributor cannot drop them without breaking
    /// this test, and cannot construct the variants from outside the
    /// crate without them either (see the `compile_fail` doctests on
    /// the [`BadWalk`] enum).
    #[test]
    fn bad_walk_variants_preserve_max_context_invariant_by_struct_shape() {
        let payload = "literal post-marker text".to_owned();
        let parsed_findings: Vec<Finding> = Vec::new();
        let concern = BadWalk::Concern {
            payload: payload.clone(),
            parsed_findings: parsed_findings.clone(),
        };
        match &concern {
            BadWalk::Concern {
                payload: p,
                parsed_findings: f,
            } => {
                assert_eq!(p, &payload);
                assert_eq!(f, &parsed_findings);
            }
            other => panic!("expected BadWalk::Concern, got {other:?}"),
        }

        let concern_without_findings = BadWalk::ConcernWithoutFindings {
            summary: "terminal concern with no stream".to_owned(),
        };
        match &concern_without_findings {
            BadWalk::ConcernWithoutFindings { summary } => {
                assert_eq!(summary, "terminal concern with no stream");
            }
            other => panic!("expected BadWalk::ConcernWithoutFindings, got {other:?}"),
        }

        let findings: Vec<Finding> = Vec::new();
        let no_concern = BadWalk::FindingsWithoutConcern {
            finding_count: findings.len(),
            findings: findings.clone(),
        };
        match &no_concern {
            BadWalk::FindingsWithoutConcern {
                finding_count,
                findings: f,
            } => {
                assert_eq!(*finding_count, findings.len());
                assert_eq!(f, &findings);
            }
            other => panic!("expected BadWalk::FindingsWithoutConcern, got {other:?}"),
        }

        let errors: Vec<FindingParseError> = Vec::new();
        let terminal = TerminalSurface::Complete;
        let malformed = BadWalk::MalformedFinding {
            errors: errors.clone(),
            terminal: terminal.clone(),
        };
        match &malformed {
            BadWalk::MalformedFinding {
                errors: e,
                terminal: t,
            } => {
                assert_eq!(e, &errors);
                assert_eq!(t, &terminal);
            }
            other => panic!("expected BadWalk::MalformedFinding, got {other:?}"),
        }
    }

    #[test]
    fn terminal_surface_carries_malformed_and_missing_variants() {
        let malformed = TerminalSurface::Malformed {
            payload: "{not json}".to_owned(),
        };
        assert_eq!(malformed.label(), "LOOM_CONCERN: <malformed: {not json}>");

        let concern = TerminalSurface::Concern {
            summary: "two findings".to_owned(),
        };
        assert_eq!(concern.label(), "LOOM_CONCERN: two findings");

        let missing = TerminalSurface::Missing;
        assert_eq!(missing.label(), "(no terminal on the final non-empty line)");
    }

    #[test]
    fn legacy_token_reason_payload_routes_to_bad_walk_concern() {
        let out = "LOOM_CONCERN: verifier-bypass -- test mocks the agent backend\n";
        match parse_exit_signal(out) {
            Some(ExitSignal::BadWalk(BadWalk::Concern { payload, .. })) => {
                assert_eq!(payload, "verifier-bypass -- test mocks the agent backend");
            }
            other => panic!("expected BadWalk::Concern, got {other:?}"),
        }
    }

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

    struct KnownSpecs<'a>(&'a [&'a str]);

    impl FindingValidator for KnownSpecs<'_> {
        fn spec_label_is_known(&self, label: &SpecLabel) -> bool {
            self.0.iter().any(|s| *s == label.as_str())
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

    struct NothingResolves;

    impl FindingValidator for NothingResolves {
        fn spec_label_is_known(&self, _label: &SpecLabel) -> bool {
            true
        }
        fn criterion_anchor_resolves(&self, _spec: &SpecLabel, _anchor: &str) -> bool {
            false
        }
        fn annotation_resolves(&self, _target_string: &str) -> bool {
            false
        }
        fn file_exists(&self, _path: &str) -> bool {
            false
        }
        fn invariant_resolves(&self, _spec: &SpecLabel, _section: &str, _tag: &str) -> bool {
            false
        }
    }

    fn payload(token: &str, bonds: &[&str], target_json: &str, evidence: &str) -> String {
        let bonds_json = bonds
            .iter()
            .map(|b| format!("\"{b}\""))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"token":"{token}","route":"deferred","bonds":[{bonds_json}],"target":{target_json},"evidence":"{evidence}"}}"#,
        )
    }

    fn finding_line(token: &str, bonds: &[&str], target_json: &str, evidence: &str) -> String {
        format!(
            "{} {}",
            LOOM_FINDING_PREFIX,
            payload(token, bonds, target_json, evidence)
        )
    }

    fn single_malformed_finding_error(
        output: &str,
        scope: DispatchScope,
        validator: &dyn FindingValidator,
    ) -> (FindingParseError, TerminalSurface) {
        match parse_walk_output(output, scope, validator) {
            Err(WalkOutputError::BadWalk {
                bad_walk: BadWalk::MalformedFinding { errors, terminal },
            }) => {
                let [error] = errors.as_slice() else {
                    panic!("expected one malformed finding error, got {errors:?}");
                };
                (error.clone(), terminal)
            }
            other => panic!("expected MalformedFinding bad walk, got {other:?}"),
        }
    }

    #[test]
    fn annotation_target_wrapper_canonicalizes_to_inner_target() {
        let output = concat!(
            r#"LOOM_FINDING: {"token":"verifier-bypass","route":"deferred","bonds":["gate"],"target":{"kind":"Annotation","target_string":"specs/pre-commit.md:141 [check?](grep -nE 'test-ci' flake.nix)"},"evidence":"wrapped target"}"#,
            "\nLOOM_CONCERN: {\"summary\":\"wrapped annotation\"}\n",
        );
        let findings = parse_walk_output(output, DispatchScope::Tree, &AlwaysValid)
            .expect("wrapped annotation target parses");

        match &findings[0].target {
            FindingTarget::Annotation { target_string } => {
                assert_eq!(target_string, "grep -nE 'test-ci' flake.nix");
            }
            other => panic!("expected Annotation target, got {other:?}"),
        }
    }

    #[test]
    fn annotation_target_command_text_is_not_treated_as_wrapper() {
        let output = concat!(
            r#"LOOM_FINDING: {"token":"verifier-bypass","route":"deferred","bonds":["gate"],"target":{"kind":"Annotation","target_string":"bash -c 'printf [check](still-raw)'"},"evidence":"literal bracket text"}"#,
            "\nLOOM_CONCERN: {\"summary\":\"literal annotation text\"}\n",
        );
        let findings = parse_walk_output(output, DispatchScope::Tree, &AlwaysValid)
            .expect("literal annotation text parses");

        match &findings[0].target {
            FindingTarget::Annotation { target_string } => {
                assert_eq!(target_string, "bash -c 'printf [check](still-raw)'");
            }
            other => panic!("expected Annotation target, got {other:?}"),
        }
    }

    #[test]
    fn finding_without_route_field_routes_to_bad_walk_malformed_finding() {
        let output = concat!(
            r#"LOOM_FINDING: {"token":"spec-coherence-fail","bonds":["gate"],"target":{"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"},"evidence":"missing route"}"#,
            "\nLOOM_CONCERN: {\"summary\":\"missing route\"}\n",
        );
        let (error, terminal) =
            single_malformed_finding_error(output, DispatchScope::Tree, &AlwaysValid);
        match error {
            FindingParseError::Json { message, raw, .. } => {
                assert!(
                    message.contains("missing field `route`"),
                    "missing route should be a serde shape error, got: {message}",
                );
                assert!(raw.contains(r#""token":"spec-coherence-fail""#));
            }
            other => panic!("expected Json error for missing route field, got {other:?}"),
        }
        assert!(matches!(terminal, TerminalSurface::Concern { .. }));
    }

    #[test]
    fn backtick_wrapped_loom_finding_line_routes_to_bad_walk_malformed_finding_with_terminal_preserved()
     {
        let good_line = finding_line(
            "spec-coherence-fail",
            &["gate"],
            r#"{"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"}"#,
            "well-formed",
        );
        let bad_line = format!("`{LOOM_FINDING_PREFIX} {{not valid json — fenced in backticks}}`");
        let output = format!("{good_line}\n{bad_line}\nLOOM_COMPLETE\n");
        let walk = WalkOutput::from_stdout(&output, DispatchScope::Tree, &AlwaysValid);
        assert_eq!(walk.findings().len(), 1, "well-formed line still parses");
        assert_eq!(walk.findings()[0].token, ConcernToken::SpecCoherenceFail);
        assert_eq!(
            walk.finding_errors().len(),
            1,
            "backtick-wrapped line errored"
        );
        match &walk.finding_errors()[0] {
            FindingParseError::Json { raw, .. } => {
                assert!(
                    raw.contains("not valid json"),
                    "raw payload preserved: {raw}",
                );
                assert!(
                    raw.starts_with('`'),
                    "raw line preserves the surrounding backticks: {raw}",
                );
            }
            other => panic!("expected Json error, got {other:?}"),
        }
        assert_eq!(
            walk.terminal(),
            &TerminalSurface::Complete,
            "well-formed terminator survives the per-line error",
        );
    }

    #[test]
    fn parse_walk_output_malformed_findings_preserves_all_errors_and_terminal() {
        let bad_json = format!("{LOOM_FINDING_PREFIX} {{not valid json");
        let bad_target = finding_line(
            "orphan-integration",
            &["gate"],
            r#"{"kind":"Criterion","spec":"gate","anchor":"x"}"#,
            "",
        );
        let output =
            format!("{bad_json}\n{bad_target}\nLOOM_CONCERN: {{\"summary\":\"bad findings\"}}\n");

        match parse_walk_output(&output, DispatchScope::Tree, &AlwaysValid) {
            Err(WalkOutputError::BadWalk {
                bad_walk: BadWalk::MalformedFinding { errors, terminal },
            }) => {
                assert_eq!(errors.len(), 2);
                assert!(matches!(errors[0], FindingParseError::Json { .. }));
                assert!(matches!(
                    errors[1],
                    FindingParseError::TokenVariantMismatch { .. }
                ));
                assert_eq!(
                    terminal,
                    TerminalSurface::Concern {
                        summary: "bad findings".to_owned()
                    }
                );
            }
            other => panic!("expected all malformed finding errors, got {other:?}"),
        }
    }

    #[test]
    fn malformed_syntax_record_does_not_swallow_later_valid_finding() {
        let good_line = finding_line(
            "spec-coherence-fail",
            &["gate"],
            r#"{"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"}"#,
            "well-formed after malformed syntax",
        );
        let output = format!(
            "{LOOM_FINDING_PREFIX} {{not valid json\n}}\n{good_line}\nLOOM_CONCERN: {{\"summary\":\"mixed\"}}\n"
        );
        let walk = WalkOutput::from_stdout(&output, DispatchScope::Tree, &AlwaysValid);
        assert_eq!(walk.finding_errors().len(), 1);
        assert_eq!(walk.findings().len(), 1);
        assert_eq!(walk.findings()[0].token, ConcernToken::SpecCoherenceFail);
    }

    #[test]
    fn loom_finding_substring_match_requires_uppercase_and_colon_suffix() {
        let no_colon = "the LOOM_FINDING marker is mentioned in prose";
        let lowercase = format!("loom_finding: {}", "{\"token\":\"x\"}");
        let output = format!("{no_colon}\n{lowercase}\nLOOM_COMPLETE\n",);
        let walk = WalkOutput::from_stdout(&output, DispatchScope::Tree, &AlwaysValid);
        assert!(
            walk.findings().is_empty(),
            "no findings parsed from bare-prose or lowercase mention: {:?}",
            walk.findings(),
        );
        assert!(
            walk.finding_errors().is_empty(),
            "no errors either — those lines did not match the prefix",
        );
        assert_eq!(walk.terminal(), &TerminalSurface::Complete);
    }

    #[test]
    fn mint_walk_emits_loom_finding_json_lines_streamed_per_finding() {
        let line_a = finding_line(
            "spec-coherence-fail",
            &["gate"],
            r#"{"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"}"#,
            "first finding",
        );
        let line_b = finding_line(
            "orphan-integration",
            &["harness"],
            r#"{"kind":"Contract","id":"molecule-lifecycle"}"#,
            "second finding",
        );
        let output = format!(
            "preamble\n{line_a}\nintermediate prose\n{line_b}\nLOOM_CONCERN: {{\"summary\":\"two findings\"}}"
        );
        let findings =
            parse_walk_output(&output, DispatchScope::Tree, &AlwaysValid).expect("parses cleanly");
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].token, ConcernToken::SpecCoherenceFail);
        assert_eq!(findings[0].route, FindingRoute::Deferred);
        assert_eq!(findings[0].evidence, "first finding");
        assert_eq!(findings[1].token, ConcernToken::OrphanIntegration);
        assert_eq!(findings[1].route, FindingRoute::Deferred);
        assert_eq!(findings[1].evidence, "second finding");
    }

    #[test]
    fn mint_walk_without_terminal_marker_fails_run() {
        let line = finding_line(
            "spec-coherence-fail",
            &["gate"],
            r#"{"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"}"#,
            "no terminal marker follows",
        );
        let output = format!("preamble\n{line}\ntrailing prose without a marker\n");
        match parse_walk_output(&output, DispatchScope::Tree, &AlwaysValid) {
            Err(WalkOutputError::MissingTerminalMarker { findings_count }) => {
                assert_eq!(findings_count, 1);
            }
            other => panic!("expected MissingTerminalMarker, got {other:?}"),
        }
    }

    #[test]
    fn mint_walk_without_findings_does_not_require_terminal_marker() {
        let output = "preamble with no findings and no markers\n";
        let findings =
            parse_walk_output(output, DispatchScope::Tree, &AlwaysValid).expect("vacuous case");
        assert!(findings.is_empty());
    }

    #[test]
    fn direct_clarify_terminal_is_wrong_review_path() {
        let output = "Should the reviewer pick a schema path?\nLOOM_CLARIFY\n";
        match parse_walk_output(output, DispatchScope::Tree, &AlwaysValid) {
            Err(WalkOutputError::WrongReviewPath { question }) => {
                assert_eq!(question, "Should the reviewer pick a schema path?");
            }
            other => panic!("expected WrongReviewPath for direct LOOM_CLARIFY, got {other:?}"),
        }
    }

    #[test]
    fn cannot_complete_review_terminals_do_not_parse_as_empty_success() {
        for (output, expected_marker, expected_reason) in [
            (
                "review logs were truncated\nLOOM_RETRY\n",
                "LOOM_RETRY",
                "review logs were truncated",
            ),
            (
                "cannot access the workspace\nLOOM_BLOCKED\n",
                "LOOM_BLOCKED",
                "cannot access the workspace",
            ),
        ] {
            match parse_walk_output(output, DispatchScope::Tree, &AlwaysValid) {
                Err(WalkOutputError::CannotComplete { marker, reason }) => {
                    assert_eq!(marker, expected_marker);
                    assert_eq!(reason, expected_reason);
                }
                other => panic!("expected CannotComplete for {expected_marker}, got {other:?}",),
            }
        }
    }

    #[test]
    fn blocked_review_terminal_without_reason_is_invalid_not_clean() {
        match parse_walk_output("LOOM_BLOCKED\n", DispatchScope::Tree, &AlwaysValid) {
            Err(WalkOutputError::InvalidTerminal { marker }) => assert_eq!(marker, "LOOM_BLOCKED"),
            other => panic!("expected InvalidTerminal for reasonless LOOM_BLOCKED, got {other:?}"),
        }
    }

    #[test]
    fn noop_terminal_is_not_a_review_walk_success() {
        match parse_walk_output("LOOM_NOOP\n", DispatchScope::Tree, &AlwaysValid) {
            Err(WalkOutputError::InvalidTerminal { marker }) => assert_eq!(marker, "LOOM_NOOP"),
            other => panic!("expected InvalidTerminal for LOOM_NOOP, got {other:?}"),
        }
    }

    #[test]
    fn clarify_route_finding_with_options_and_concern_reaches_mint_pipeline() {
        let gate = spec("gate");
        let finding = Finding {
            token: ConcernToken::SpecCoherenceFail,
            route: FindingRoute::Clarify,
            bonds: vec![gate.clone()],
            target: FindingTarget::Criterion {
                spec: gate,
                anchor: "review-terminal-contract".to_owned(),
            },
            evidence: "Needs human choice.\n\n## Options — pick path\n\n### Option 1 — Keep current\nCost: debt.\n\n### Option 2 — Change contract\nCost: churn."
                .to_owned(),
        };
        let payload = serde_json::to_string(&finding).expect("serialize clarify finding");
        let output = format!(
            "preamble\n{LOOM_FINDING_PREFIX} {payload}\nLOOM_CONCERN: {{\"summary\":\"clarify needed\"}}\n",
        );
        let findings = parse_walk_output(&output, DispatchScope::Tree, &AlwaysValid)
            .expect("clarify-route finding parses");
        assert_eq!(findings, vec![finding]);
    }

    #[test]
    fn raw_multiline_evidence_is_normalized_before_strict_validation() {
        let output = concat!(
            "preamble\n",
            "LOOM_FINDING: {\"token\":\"invariant-clash\",\"route\":\"clarify\",\"bonds\":[\"gate\"],\"target\":{\"kind\":\"Invariant\",\"spec\":\"gate\",\"section\":\"Out of Scope\",\"tag\":\"loom-runs-podman\"},\"evidence\":\"The implementation conflicts with the invariant.\n",
            "\n",
            "## Options — resolve invariant clash\n",
            "\n",
            "### Option 1 — Preserve invariant\n",
            "Cost: more implementation churn.\n",
            "\n",
            "### Option 2 — Change invariant\n",
            "Cost: spec update and follow-up work.\"}\n",
            "LOOM_CONCERN: {\"summary\":\"clarify invariant clash\"}\n",
        );
        let findings = parse_walk_output(output, DispatchScope::Tree, &AlwaysValid)
            .expect("raw multiline evidence parses");
        let [finding] = findings.as_slice() else {
            panic!("expected one finding, got {findings:?}");
        };
        assert_eq!(finding.token, ConcernToken::InvariantClash);
        assert_eq!(finding.route, FindingRoute::Clarify);
        assert!(
            finding
                .evidence
                .contains("## Options — resolve invariant clash\n\n### Option 1"),
            "evidence preserves raw line breaks: {:?}",
            finding.evidence,
        );
        assert!(
            finding.evidence.contains("### Option 2 — Change invariant"),
            "second option survived normalization: {:?}",
            finding.evidence,
        );
    }

    #[test]
    fn findings_streamed_with_complete_terminator_routes_to_badwalk_findings_without_concern() {
        let line = finding_line(
            "orphan-integration",
            &["harness"],
            r#"{"kind":"Contract","id":"x"}"#,
            "",
        );
        let output = format!("{line}\nLOOM_COMPLETE\n");

        match parse_walk_output(&output, DispatchScope::Tree, &AlwaysValid) {
            Err(WalkOutputError::BadWalk {
                bad_walk:
                    BadWalk::FindingsWithoutConcern {
                        finding_count,
                        findings,
                    },
            }) => {
                assert_eq!(finding_count, 1);
                assert_eq!(findings[0].token, ConcernToken::OrphanIntegration);
            }
            other => panic!("expected FindingsWithoutConcern, got {other:?}"),
        }
    }

    #[test]
    fn malformed_concern_payload_preserves_parsed_findings_in_walk_parser() {
        let line = finding_line(
            "orphan-integration",
            &["harness"],
            r#"{"kind":"Contract","id":"x"}"#,
            "",
        );
        let output = format!("{line}\nLOOM_CONCERN: orphan-integration -- legacy\n");

        match parse_walk_output(&output, DispatchScope::Tree, &AlwaysValid) {
            Err(WalkOutputError::BadWalk {
                bad_walk:
                    BadWalk::Concern {
                        payload,
                        parsed_findings,
                    },
            }) => {
                assert_eq!(payload, "orphan-integration -- legacy");
                assert_eq!(parsed_findings[0].token, ConcernToken::OrphanIntegration);
            }
            other => panic!("expected BadWalk::Concern, got {other:?}"),
        }
    }

    #[test]
    fn mint_parses_loom_finding_json_into_typed_record_with_tagged_target() {
        let line = finding_line(
            "orphan-integration",
            &["harness"],
            r#"{"kind":"Contract","id":"molecule-lifecycle"}"#,
            "contract is dangling",
        );
        let output = format!("{line}\nLOOM_CONCERN: {{\"summary\":\"found one\"}}\n");
        let findings =
            parse_walk_output(&output, DispatchScope::Tree, &AlwaysValid).expect("parses cleanly");
        let [parsed] = findings.as_slice() else {
            panic!("expected exactly one finding, got {findings:?}")
        };
        assert_eq!(parsed.token, ConcernToken::OrphanIntegration);
        assert!(
            matches!(parsed.target, FindingTarget::Contract { ref id } if id == "molecule-lifecycle")
        );
        assert_eq!(parsed.target.kind(), TargetKind::Contract);
        assert_eq!(parsed.token.expected_target_kind(), parsed.target.kind(),);
    }

    #[test]
    fn mint_malformed_loom_finding_fails_run_with_typed_error() {
        let valid_terminal = r#"LOOM_CONCERN: {"summary":"summary"}"#;

        let line = format!("{LOOM_FINDING_PREFIX} {{not valid json");
        let output = format!("{line}\n{valid_terminal}\n");
        match single_malformed_finding_error(&output, DispatchScope::Tree, &AlwaysValid).0 {
            FindingParseError::Json {
                line_number, raw, ..
            } => {
                assert_eq!(line_number, 1);
                assert!(raw.contains("not valid json"), "raw: {raw}");
            }
            other => panic!("expected Json error, got {other:?}"),
        }

        let line = finding_line(
            "not-a-known-token",
            &["gate"],
            r#"{"kind":"Criterion","spec":"gate","anchor":"x"}"#,
            "",
        );
        let output = format!("{line}\n{valid_terminal}\n");
        match single_malformed_finding_error(&output, DispatchScope::Tree, &AlwaysValid).0 {
            FindingParseError::Json {
                line_number, raw, ..
            } => {
                assert_eq!(line_number, 1);
                assert!(raw.contains("not-a-known-token"), "raw: {raw}");
            }
            other => panic!("expected Json error for unknown token, got {other:?}"),
        }

        let line = finding_line(
            "orphan-integration",
            &["not-a-real-spec"],
            r#"{"kind":"Contract","id":"x"}"#,
            "",
        );
        let output = format!("{line}\n{valid_terminal}\n");
        let known = KnownSpecs(&["gate", "harness"]);
        match single_malformed_finding_error(&output, DispatchScope::Tree, &known).0 {
            FindingParseError::UnknownBondSpec {
                line_number,
                spec: bad,
                raw,
            } => {
                assert_eq!(line_number, 1);
                assert_eq!(bad, "not-a-real-spec");
                assert!(raw.contains("not-a-real-spec"));
            }
            other => panic!("expected UnknownBondSpec, got {other:?}"),
        }

        let line = finding_line(
            "orphan-integration",
            &["gate"],
            r#"{"kind":"Criterion","spec":"gate","anchor":"x"}"#,
            "",
        );
        let output = format!("{line}\n{valid_terminal}\n");
        match single_malformed_finding_error(&output, DispatchScope::Tree, &AlwaysValid).0 {
            FindingParseError::TokenVariantMismatch {
                line_number,
                token,
                expected,
                actual,
                raw,
            } => {
                assert_eq!(line_number, 1);
                assert_eq!(token, "orphan-integration");
                assert_eq!(expected, TargetKind::Contract);
                assert_eq!(actual, TargetKind::Criterion);
                assert!(raw.contains("orphan-integration"));
            }
            other => panic!("expected TokenVariantMismatch, got {other:?}"),
        }

        let line = finding_line(
            "judge-flag",
            &["gate"],
            r#"{"kind":"Criterion","spec":"gate","anchor":"missing-anchor"}"#,
            "",
        );
        let output = format!("{line}\n{valid_terminal}\n");
        match single_malformed_finding_error(&output, DispatchScope::Tree, &NothingResolves).0 {
            FindingParseError::UnresolvedTarget {
                line_number,
                detail,
                raw,
            } => {
                assert_eq!(line_number, 1);
                assert!(detail.contains("missing-anchor"), "detail: {detail}");
                assert!(raw.contains("missing-anchor"));
            }
            other => panic!("expected UnresolvedTarget, got {other:?}"),
        }
    }

    #[test]
    fn style_rule_finding_requires_concrete_subject() {
        let valid_terminal = "LOOM_CONCERN: {\"summary\":\"style\"}";
        let rule_only = finding_line(
            "style-rule-violation",
            &["gate"],
            r#"{"kind":"StyleRule","rule_id":"RS-3"}"#,
            "too broad",
        );
        let output = format!("{rule_only}\n{valid_terminal}\n");
        match single_malformed_finding_error(&output, DispatchScope::Tree, &AlwaysValid).0 {
            FindingParseError::Json { raw, .. } => {
                assert!(raw.contains("RS-3"), "raw: {raw}");
            }
            other => panic!("expected missing subject to fail serde, got {other:?}"),
        }

        for (subject, reason) in [("", "empty subject"), ("42", "bare line number")] {
            let target =
                format!(r#"{{"kind":"StyleRule","rule_id":"RS-3","subject":"{subject}"}}"#,);
            let line = finding_line("style-rule-violation", &["gate"], &target, "bad subject");
            let output = format!("{line}\n{valid_terminal}\n");
            match single_malformed_finding_error(&output, DispatchScope::Tree, &AlwaysValid).0 {
                FindingParseError::InvalidStyleRuleSubject {
                    reason: actual,
                    raw,
                    ..
                } => {
                    assert_eq!(actual, reason);
                    assert!(raw.contains("StyleRule"), "raw: {raw}");
                }
                other => panic!("expected invalid style subject `{subject}`, got {other:?}"),
            }
        }

        let valid = finding_line(
            "style-rule-violation",
            &["gate"],
            r#"{"kind":"StyleRule","rule_id":"RS-3","subject":"crates/loom-gate/src/integrity.rs#Verifier"}"#,
            "concrete subject",
        );
        let output = format!("{valid}\n{valid_terminal}\n");
        let findings = parse_walk_output(&output, DispatchScope::Tree, &AlwaysValid)
            .expect("style finding with concrete subject parses");
        assert_eq!(
            findings[0].id(),
            "v1:style-rule:rs-3:crates-loom-gate-src-integrity-rs-verifier"
        );
    }

    #[test]
    fn mint_rejects_criterion_target_whose_spec_is_not_in_bonds() {
        let line = finding_line(
            "spec-coherence-fail",
            &["harness"],
            r#"{"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"}"#,
            "criterion belongs to gate but bonds names harness only",
        );
        let output = format!("{line}\nLOOM_CONCERN: {{\"summary\":\"bad bonds\"}}\n");
        match single_malformed_finding_error(&output, DispatchScope::Tree, &AlwaysValid).0 {
            FindingParseError::TargetSpecNotInBonds {
                line_number,
                spec: missing,
                bonds,
                raw,
            } => {
                assert_eq!(line_number, 1);
                assert_eq!(missing, "gate");
                assert_eq!(bonds, vec!["harness".to_owned()]);
                assert!(raw.contains("\"spec\":\"gate\""));
            }
            other => panic!("expected TargetSpecNotInBonds, got {other:?}"),
        }

        let line = finding_line(
            "invariant-clash",
            &["gate"],
            r#"{"kind":"Invariant","spec":"harness","section":"Out of Scope","tag":"loom-runs-podman"}"#,
            "invariant target spec missing from bonds",
        );
        let output = format!("{line}\nLOOM_CONCERN: {{\"summary\":\"bad bonds\"}}\n");
        match single_malformed_finding_error(&output, DispatchScope::Tree, &AlwaysValid).0 {
            FindingParseError::TargetSpecNotInBonds { spec: missing, .. } => {
                assert_eq!(missing, "harness");
            }
            other => panic!("expected TargetSpecNotInBonds, got {other:?}"),
        }

        let line = finding_line(
            "spec-coherence-fail",
            &["harness", "gate"],
            r#"{"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"}"#,
            "criterion belongs to gate, bonds names both",
        );
        let output = format!("{line}\nLOOM_CONCERN: {{\"summary\":\"ok\"}}\n");
        let findings =
            parse_walk_output(&output, DispatchScope::Tree, &AlwaysValid).expect("should parse");
        assert_eq!(findings.len(), 1);

        let _ = spec("gate");
    }

    fn canonical_target(token: ConcernToken, gate: &SpecLabel) -> FindingTarget {
        match token {
            ConcernToken::SpecCoherenceFail => FindingTarget::Criterion {
                spec: gate.clone(),
                anchor: "verifier-honesty".to_owned(),
            },
            ConcernToken::VerifierTooNarrow => FindingTarget::Criterion {
                spec: gate.clone(),
                anchor: "cross-component-sufficiency".to_owned(),
            },
            ConcernToken::JudgeFlag => FindingTarget::Criterion {
                spec: gate.clone(),
                anchor: "judge-rubric".to_owned(),
            },
            ConcernToken::MultipleAnnotations => FindingTarget::Criterion {
                spec: gate.clone(),
                anchor: "atomic-acceptance".to_owned(),
            },
            ConcernToken::OrphanIntegration => FindingTarget::Contract {
                id: "molecule-lifecycle".to_owned(),
            },
            ConcernToken::StyleRuleViolation => FindingTarget::StyleRule {
                rule_id: "RS-12".to_owned(),
                subject: "crates/loom-gate/src/integrity.rs".to_owned(),
            },
            ConcernToken::VerifierBypass => FindingTarget::Annotation {
                target_string: "cargo test --lib verifier_bypass_case".to_owned(),
            },
            ConcernToken::WeakAssertion => FindingTarget::Annotation {
                target_string: "cargo test --lib weak_assertion_case".to_owned(),
            },
            ConcernToken::FabricatedResult => FindingTarget::Annotation {
                target_string: "cargo test --lib fabricated_result_case".to_owned(),
            },
            ConcernToken::CoincidentalPass => FindingTarget::Annotation {
                target_string: "cargo test --lib coincidental_pass_case".to_owned(),
            },
            ConcernToken::VerifierFailed => FindingTarget::Annotation {
                target_string: "cargo run -p loom-walk -- example".to_owned(),
            },
            ConcernToken::DispatchError => FindingTarget::Annotation {
                target_string: "missing-command --flag".to_owned(),
            },
            ConcernToken::UnresolvedAnnotation => FindingTarget::Annotation {
                target_string: "cargo run -p loom-walk -- unresolved".to_owned(),
            },
            ConcernToken::StubPointing => FindingTarget::Annotation {
                target_string: "cargo test --lib stub_pointing_case".to_owned(),
            },
            ConcernToken::UnneededPendingMarker => FindingTarget::Annotation {
                target_string: "cargo test --lib already_resolved".to_owned(),
            },
            ConcernToken::InputsProtocolError => FindingTarget::Annotation {
                target_string: "cargo run -p loom-walk -- inputs".to_owned(),
            },
            ConcernToken::MockDiscipline => FindingTarget::TestPath {
                path: "crates/loom-gate/src/integrity.rs::mock_disciplined".to_owned(),
            },
            ConcernToken::ConcurrencyUntested => FindingTarget::LockSite {
                file: "crates/loom-workflow/src/run/runner.rs".to_owned(),
                line: 210,
            },
            ConcernToken::InvariantClash => FindingTarget::Invariant {
                spec: gate.clone(),
                section: "Out of Scope".to_owned(),
                tag: "loom-runs-podman".to_owned(),
            },
            ConcernToken::TemplateSpecDrift => FindingTarget::Template {
                path: "crates/loom-templates/templates/review.md".to_owned(),
            },
            ConcernToken::CrossSpecClash => FindingTarget::Criterion {
                spec: gate.clone(),
                anchor: "cross-spec-clash".to_owned(),
            },
            ConcernToken::SpecConventionsViolation => FindingTarget::Criterion {
                spec: gate.clone(),
                anchor: "spec-conventions-violation".to_owned(),
            },
            ConcernToken::ScopeCreep => FindingTarget::Criterion {
                spec: gate.clone(),
                anchor: "scope-appropriateness".to_owned(),
            },
            ConcernToken::ScopeShortfall => FindingTarget::Criterion {
                spec: gate.clone(),
                anchor: "scope-appropriateness".to_owned(),
            },
            ConcernToken::PendingMarkerResolved => FindingTarget::MatrixCell {
                spec: gate.clone(),
                partial: "findings_walk".to_owned(),
                template: "review".to_owned(),
            },
        }
    }

    #[test]
    fn every_finding_round_trips_through_wire_format_with_stable_identity() {
        let gate = spec("gate");
        let tokens = [
            ConcernToken::SpecCoherenceFail,
            ConcernToken::OrphanIntegration,
            ConcernToken::StyleRuleViolation,
            ConcernToken::VerifierBypass,
            ConcernToken::WeakAssertion,
            ConcernToken::FabricatedResult,
            ConcernToken::CoincidentalPass,
            ConcernToken::MockDiscipline,
            ConcernToken::VerifierTooNarrow,
            ConcernToken::ConcurrencyUntested,
            ConcernToken::JudgeFlag,
            ConcernToken::InvariantClash,
            ConcernToken::TemplateSpecDrift,
            ConcernToken::CrossSpecClash,
            ConcernToken::SpecConventionsViolation,
            ConcernToken::VerifierFailed,
            ConcernToken::DispatchError,
            ConcernToken::UnresolvedAnnotation,
            ConcernToken::StubPointing,
            ConcernToken::MultipleAnnotations,
            ConcernToken::UnneededPendingMarker,
            ConcernToken::InputsProtocolError,
            ConcernToken::ScopeCreep,
            ConcernToken::ScopeShortfall,
            ConcernToken::PendingMarkerResolved,
        ];
        let terminators = ["LOOM_CONCERN: {\"summary\":\"round-trip\"}"];

        for token in tokens {
            let target = canonical_target(token, &gate);
            assert!(
                token.allows_target_kind(target.kind()),
                "canonical pairing self-check for {}",
                token.as_wire(),
            );
            let bonds = match target.spec() {
                Some(s) => vec![s.clone()],
                None => vec![gate.clone()],
            };
            let input = Finding {
                token,
                route: FindingRoute::Deferred,
                bonds,
                target,
                evidence: format!("round-trip evidence for {}", token.as_wire()),
            };
            let payload = serde_json::to_string(&input).expect("serialize finding");
            let payload_value: serde_json::Value =
                serde_json::from_str(&payload).expect("payload is JSON");
            let payload_object = payload_value
                .as_object()
                .expect("finding payload is object");
            assert!(
                !payload_object.contains_key("id") && !payload_object.contains_key("hash"),
                "derived identity fields stay out of LOOM_FINDING payload: {payload}",
            );

            let dispatch_scope = match token.scope_kind() {
                ScopeKind::PerBead => DispatchScope::PerBead,
                ScopeKind::TreeOnly | ScopeKind::AnyScope => DispatchScope::Tree,
                ScopeKind::TreeAndPushGate => DispatchScope::PushGate,
            };
            for terminator in terminators {
                let output = format!(
                    "preamble\n{LOOM_FINDING_PREFIX} {payload}\nintermediate prose\n{terminator}\n",
                );
                let parsed = parse_walk_output(&output, dispatch_scope, &AlwaysValid)
                    .unwrap_or_else(|e| {
                        panic!(
                            "round-trip parse failed for {} with terminator `{terminator}`: {e}",
                            token.as_wire(),
                        )
                    });
                let [round] = parsed.as_slice() else {
                    panic!(
                        "expected exactly one finding for {} with terminator `{terminator}`, got {parsed:?}",
                        token.as_wire(),
                    )
                };
                assert_eq!(
                    round,
                    &input,
                    "byte-equal struct round-trip for {} with terminator `{terminator}`",
                    token.as_wire(),
                );
                assert_eq!(
                    round.id(),
                    input.id(),
                    "id stability for {} with terminator `{terminator}`",
                    token.as_wire(),
                );
                assert_eq!(
                    round.hash(),
                    input.hash(),
                    "hash stability for {} with terminator `{terminator}`",
                    token.as_wire(),
                );
            }
        }
    }

    /// Spec contract `specs/gate.md` § *Concern tokens and target
    /// variants* (criterion `tree_scope_only_tokens_rejected_at_non_tree_scope`):
    /// tokens whose [`ConcernToken::scope_kind`] is
    /// [`ScopeKind::TreeOnly`] surface a typed
    /// [`FindingParseError::TokenScopeMismatch`] when parsed at
    /// non-tree dispatch scopes, and per-bead-only tokens
    /// (`scope-creep` / `scope-shortfall`) surface the same error at
    /// tree and push-gate dispatch scopes. Anywhere-admissible tokens
    /// pass at all scopes.
    #[test]
    fn tree_scope_only_tokens_rejected_at_non_tree_scope() {
        let gate = spec("gate");
        let terminator = "LOOM_CONCERN: {\"summary\":\"scope mismatch\"}";

        let tree_only = [
            ConcernToken::TemplateSpecDrift,
            ConcernToken::CrossSpecClash,
            ConcernToken::SpecConventionsViolation,
            ConcernToken::VerifierFailed,
            ConcernToken::DispatchError,
            ConcernToken::MultipleAnnotations,
        ];
        for token in tree_only {
            assert_eq!(token.scope_kind(), ScopeKind::TreeOnly, "{token:?}");

            let finding = Finding {
                token,
                route: FindingRoute::Deferred,
                bonds: vec![gate.clone()],
                target: canonical_target(token, &gate),
                evidence: "scope mismatch fixture".to_owned(),
            };
            let payload = serde_json::to_string(&finding).expect("serialize");
            let output = format!("preamble\n{LOOM_FINDING_PREFIX} {payload}\n{terminator}\n");

            for scope in [DispatchScope::PerBead, DispatchScope::PushGate] {
                match single_malformed_finding_error(&output, scope, &AlwaysValid).0 {
                    FindingParseError::TokenScopeMismatch {
                        token: bad_token,
                        scope_kind,
                        dispatch_scope,
                        ..
                    } => {
                        assert_eq!(bad_token, token.as_wire());
                        assert_eq!(scope_kind, "tree-only");
                        assert_eq!(dispatch_scope, scope.label());
                    }
                    other => panic!(
                        "expected TokenScopeMismatch for tree-only token `{}` at {} scope, got {other:?}",
                        token.as_wire(),
                        scope.label(),
                    ),
                }
            }

            parse_walk_output(&output, DispatchScope::Tree, &AlwaysValid).unwrap_or_else(|e| {
                panic!(
                    "tree-only token `{}` must parse cleanly at tree scope: {e}",
                    token.as_wire(),
                )
            });
        }

        let per_bead_only = [ConcernToken::ScopeCreep, ConcernToken::ScopeShortfall];
        for token in per_bead_only {
            assert_eq!(token.scope_kind(), ScopeKind::PerBead, "{token:?}");

            let finding = Finding {
                token,
                route: FindingRoute::Deferred,
                bonds: vec![gate.clone()],
                target: canonical_target(token, &gate),
                evidence: "scope mismatch fixture".to_owned(),
            };
            let payload = serde_json::to_string(&finding).expect("serialize");
            let output = format!("preamble\n{LOOM_FINDING_PREFIX} {payload}\n{terminator}\n");

            for scope in [DispatchScope::Tree, DispatchScope::PushGate] {
                match single_malformed_finding_error(&output, scope, &AlwaysValid).0 {
                    FindingParseError::TokenScopeMismatch {
                        token: bad_token,
                        scope_kind,
                        dispatch_scope,
                        ..
                    } => {
                        assert_eq!(bad_token, token.as_wire());
                        assert_eq!(scope_kind, "per-bead-only");
                        assert_eq!(dispatch_scope, scope.label());
                    }
                    other => panic!(
                        "expected TokenScopeMismatch for per-bead-only token `{}` at {} scope, got {other:?}",
                        token.as_wire(),
                        scope.label(),
                    ),
                }
            }

            parse_walk_output(&output, DispatchScope::PerBead, &AlwaysValid).unwrap_or_else(|e| {
                panic!(
                    "per-bead-only token `{}` must parse cleanly at per-bead scope: {e}",
                    token.as_wire(),
                )
            });
        }

        let any_scope_finding = Finding {
            token: ConcernToken::SpecCoherenceFail,
            route: FindingRoute::Deferred,
            bonds: vec![gate.clone()],
            target: FindingTarget::Criterion {
                spec: gate.clone(),
                anchor: "verifier-honesty".to_owned(),
            },
            evidence: "any-scope token".to_owned(),
        };
        let payload = serde_json::to_string(&any_scope_finding).expect("serialize");
        let output = format!("preamble\n{LOOM_FINDING_PREFIX} {payload}\n{terminator}\n");
        for scope in [
            DispatchScope::PerBead,
            DispatchScope::PushGate,
            DispatchScope::Tree,
        ] {
            parse_walk_output(&output, scope, &AlwaysValid).unwrap_or_else(|e| {
                panic!("AnyScope token must parse at {} scope: {e}", scope.label())
            });
        }
    }

    #[test]
    fn blocking_route_rejected_at_push_gate_scope() {
        let gate = spec("gate");
        let finding = Finding {
            token: ConcernToken::SpecCoherenceFail,
            route: FindingRoute::Blocking,
            bonds: vec![gate.clone()],
            target: FindingTarget::Criterion {
                spec: gate,
                anchor: "verifier-honesty".to_owned(),
            },
            evidence: "tree mint treats blocking as ready remediation".to_owned(),
        };
        let payload = serde_json::to_string(&finding).expect("serialize");
        let output = format!(
            "preamble\n{LOOM_FINDING_PREFIX} {payload}\nLOOM_CONCERN: {{\"summary\":\"blocking route\"}}\n"
        );

        match single_malformed_finding_error(&output, DispatchScope::PushGate, &AlwaysValid).0 {
            FindingParseError::RouteScopeMismatch {
                route,
                dispatch_scope,
                ..
            } => {
                assert_eq!(route, "blocking");
                assert_eq!(dispatch_scope, DispatchScope::PushGate.label());
            }
            other => panic!(
                "expected RouteScopeMismatch for blocking route at push-gate scope, got {other:?}",
            ),
        }
        for scope in [DispatchScope::PerBead, DispatchScope::Tree] {
            parse_walk_output(&output, scope, &AlwaysValid).unwrap_or_else(|e| {
                panic!("blocking route must parse at {} scope: {e}", scope.label())
            });
        }
    }

    #[test]
    fn integrity_tokens_parse_at_tree_and_push_gate_but_not_per_bead_scope() {
        let gate = spec("gate");
        let terminator = "LOOM_CONCERN: {\"summary\":\"integrity scope\"}";
        let integrity_tokens = [
            ConcernToken::UnresolvedAnnotation,
            ConcernToken::StubPointing,
            ConcernToken::UnneededPendingMarker,
            ConcernToken::InputsProtocolError,
        ];

        for token in integrity_tokens {
            assert_eq!(token.scope_kind(), ScopeKind::TreeAndPushGate, "{token:?}");
            let finding = Finding {
                token,
                route: FindingRoute::Deferred,
                bonds: vec![gate.clone()],
                target: canonical_target(token, &gate),
                evidence: "integrity fixture".to_owned(),
            };
            let payload = serde_json::to_string(&finding).expect("serialize");
            let output = format!("preamble\n{LOOM_FINDING_PREFIX} {payload}\n{terminator}\n");

            match single_malformed_finding_error(&output, DispatchScope::PerBead, &AlwaysValid).0 {
                FindingParseError::TokenScopeMismatch {
                    token: bad_token,
                    scope_kind,
                    dispatch_scope,
                    ..
                } => {
                    assert_eq!(bad_token, token.as_wire());
                    assert_eq!(scope_kind, "tree-and-push-gate");
                    assert_eq!(dispatch_scope, "per-bead");
                }
                other => panic!(
                    "expected TokenScopeMismatch for integrity token `{}` at per-bead scope, got {other:?}",
                    token.as_wire(),
                ),
            }
            for scope in [DispatchScope::Tree, DispatchScope::PushGate] {
                parse_walk_output(&output, scope, &AlwaysValid).unwrap_or_else(|e| {
                    panic!(
                        "integrity token `{}` must parse cleanly at {} scope: {e}",
                        token.as_wire(),
                        scope.label(),
                    )
                });
            }
        }
    }

    /// Spec contract `specs/gate.md` § *`loom-protocol` crate* — the
    /// `cross-spec-clash` rubric token round-trips byte-equal through
    /// `serde_json` and `parse_walk_output` with canonical target
    /// `Criterion { spec, anchor }`, and is a tree-scope-only token.
    #[test]
    fn concern_token_cross_spec_clash_round_trips_with_criterion_target() {
        let token = ConcernToken::CrossSpecClash;
        assert_eq!(token.as_wire(), "cross-spec-clash");
        assert_eq!(token.expected_target_kind(), TargetKind::Criterion);
        assert_eq!(token.scope_kind(), ScopeKind::TreeOnly);

        let gate = spec("gate");
        let target = canonical_target(token, &gate);
        assert!(matches!(target, FindingTarget::Criterion { .. }));

        let finding = Finding {
            token,
            route: FindingRoute::Deferred,
            bonds: vec![gate.clone()],
            target,
            evidence: "cross-spec-clash round-trip".to_owned(),
        };
        let payload = serde_json::to_string(&finding).expect("serialize");
        let output = format!(
            "preamble\n{LOOM_FINDING_PREFIX} {payload}\nLOOM_CONCERN: {{\"summary\":\"round-trip\"}}\n"
        );
        let parsed = parse_walk_output(&output, DispatchScope::Tree, &AlwaysValid)
            .expect("round-trip parse");
        assert_eq!(parsed, vec![finding]);
    }

    /// Spec contract `specs/gate.md` § *`loom-protocol` crate* — the
    /// `spec-conventions-violation` rubric token round-trips byte-equal
    /// through `serde_json` and `parse_walk_output` with canonical target
    /// `Criterion { spec, anchor }`, and is a tree-scope-only token.
    #[test]
    fn concern_token_spec_conventions_violation_round_trips_with_criterion_target() {
        let token = ConcernToken::SpecConventionsViolation;
        assert_eq!(token.as_wire(), "spec-conventions-violation");
        assert_eq!(token.expected_target_kind(), TargetKind::Criterion);
        assert_eq!(token.scope_kind(), ScopeKind::TreeOnly);

        let gate = spec("gate");
        let target = canonical_target(token, &gate);
        assert!(matches!(target, FindingTarget::Criterion { .. }));

        let finding = Finding {
            token,
            route: FindingRoute::Deferred,
            bonds: vec![gate.clone()],
            target,
            evidence: "spec-conventions-violation round-trip".to_owned(),
        };
        let payload = serde_json::to_string(&finding).expect("serialize");
        let output = format!(
            "preamble\n{LOOM_FINDING_PREFIX} {payload}\nLOOM_CONCERN: {{\"summary\":\"round-trip\"}}\n"
        );
        let parsed = parse_walk_output(&output, DispatchScope::Tree, &AlwaysValid)
            .expect("round-trip parse");
        assert_eq!(parsed, vec![finding]);
    }

    /// Spec contract `specs/gate.md` § *Concern tokens and target
    /// variants* — the `inputs-protocol-error` integrity-gate token
    /// round-trips byte-equal through `serde_json` and
    /// `parse_walk_output` with canonical target
    /// `Annotation { target_string }`, and is a tree-and-push-gate token
    /// emitted by the integrity gate's inputs-protocol check.
    #[test]
    fn concern_token_inputs_protocol_error_round_trips_with_annotation_target() {
        let token = ConcernToken::InputsProtocolError;
        assert_eq!(token.as_wire(), "inputs-protocol-error");
        assert_eq!(token.expected_target_kind(), TargetKind::Annotation);
        assert_eq!(token.scope_kind(), ScopeKind::TreeAndPushGate);

        let gate = spec("gate");
        let target = canonical_target(token, &gate);
        assert!(matches!(target, FindingTarget::Annotation { .. }));

        let finding = Finding {
            token,
            route: FindingRoute::Deferred,
            bonds: vec![gate.clone()],
            target,
            evidence: "inputs-protocol-error round-trip".to_owned(),
        };
        let payload = serde_json::to_string(&finding).expect("serialize");
        let output = format!(
            "preamble\n{LOOM_FINDING_PREFIX} {payload}\nLOOM_CONCERN: {{\"summary\":\"round-trip\"}}\n"
        );
        for scope in [DispatchScope::Tree, DispatchScope::PushGate] {
            let parsed = parse_walk_output(&output, scope, &AlwaysValid).expect("round-trip parse");
            assert_eq!(parsed, vec![finding.clone()]);
        }
    }

    /// Spec contract `specs/gate.md` § *Concern tokens and target
    /// variants* — the `pending-marker-resolved` sweeping-walker token
    /// accepts both matrix-cell and surface-element target variants.
    #[test]
    fn concern_token_pending_marker_resolved_round_trips_with_walker_targets() {
        let token = ConcernToken::PendingMarkerResolved;
        assert_eq!(token.as_wire(), "pending-marker-resolved");
        assert_eq!(token.scope_kind(), ScopeKind::AnyScope);

        let gate = spec("gate");
        let targets = [
            FindingTarget::MatrixCell {
                spec: gate.clone(),
                partial: "findings_walk".to_owned(),
                template: "review".to_owned(),
            },
            FindingTarget::SurfaceElement {
                spec: gate.clone(),
                element_kind: "command".to_owned(),
                name: "loom gate verify".to_owned(),
            },
        ];

        for target in targets {
            assert!(token.allows_target_kind(target.kind()));
            let finding = Finding {
                token,
                route: FindingRoute::Deferred,
                bonds: vec![gate.clone()],
                target,
                evidence: "pending marker resolved".to_owned(),
            };
            let payload = serde_json::to_string(&finding).expect("serialize");
            let output = format!(
                "preamble\n{LOOM_FINDING_PREFIX} {payload}\nLOOM_CONCERN: {{\"summary\":\"round-trip\"}}\n"
            );
            for scope in [
                DispatchScope::PerBead,
                DispatchScope::PushGate,
                DispatchScope::Tree,
            ] {
                let parsed =
                    parse_walk_output(&output, scope, &AlwaysValid).expect("round-trip parse");
                assert_eq!(parsed, vec![finding.clone()]);
            }
        }
    }
}

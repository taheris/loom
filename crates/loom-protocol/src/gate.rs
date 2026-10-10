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
///
/// Trusted findings cannot be deserialized or mutated without resolution.
///
/// ```compile_fail
/// use loom_protocol::gate::Finding;
/// let _: Finding = serde_json::from_str("{}").unwrap();
/// ```
/// ```compile_fail
/// use loom_protocol::gate::{Finding, ConcernToken, FindingRoute, FindingTarget};
/// let _ = Finding {
///     token: ConcernToken::VerifierBypass, route: FindingRoute::Deferred,
///     bonds: vec!["gate".parse().unwrap()],
///     target: FindingTarget::Annotation { target_string: "cargo test known".into() },
///     evidence: "observed".into(),
/// };
/// ```
/// ```compile_fail
/// use loom_protocol::gate::{Finding, FindingRoute};
/// fn forge(finding: &mut Finding) { finding.route = FindingRoute::Blocking; }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    token: ConcernToken,
    route: FindingRoute,
    bonds: Vec<SpecLabel>,
    target: FindingTarget,
    evidence: String,
}

/// Untrusted wire or driver-authored input. Resolve before minting or review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawFinding {
    pub token: ConcernToken,
    pub route: FindingRoute,
    pub bonds: Vec<SpecLabel>,
    pub target: FindingTarget,
    pub evidence: String,
}

impl RawFinding {
    /// Check intrinsic invariants and context-dependent identity in one boundary.
    ///
    /// # Errors
    /// Returns a typed finding error for malformed shape, scope, bonds, or unresolved identity.
    pub fn resolve<V: FindingValidator + ?Sized>(
        self,
        scope: DispatchScope,
        validator: &V,
    ) -> Result<Finding, FindingParseError> {
        Finding::resolve(self, 1, "driver-authored finding", scope, validator)
    }
}

impl Finding {
    pub const fn token(&self) -> ConcernToken {
        self.token
    }
    pub const fn route(&self) -> FindingRoute {
        self.route
    }
    pub fn bonds(&self) -> &[SpecLabel] {
        &self.bonds
    }
    pub const fn target(&self) -> &FindingTarget {
        &self.target
    }
    pub fn evidence(&self) -> &str {
        &self.evidence
    }

    /// Return mutable wire data; changing it requires resolution again.
    pub fn into_raw(self) -> RawFinding {
        RawFinding {
            token: self.token,
            route: self.route,
            bonds: self.bonds,
            target: self.target,
            evidence: self.evidence,
        }
    }

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
/// `PascalCase`. Tokens carry a [`ScopeKind`] discoverable via
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
    pub const fn as_wire(self) -> &'static str {
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
    pub const fn expected_target_kind(self) -> TargetKind {
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
    pub const fn scope_kind(self) -> ScopeKind {
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

/// Dispatch scope used to validate finding tokens at the wire boundary.
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
    pub const fn label(self) -> &'static str {
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
    pub const fn admits(self, scope: DispatchScope) -> bool {
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
    pub const fn label(self) -> &'static str {
        match self {
            Self::PerBead => "per-bead-only",
            Self::TreeOnly => "tree-only",
            Self::TreeAndPushGate => "tree-and-push-gate",
            Self::AnyScope => "any-scope",
        }
    }
}

/// Wire `kind` discriminator for [`FindingTarget`].
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
    pub const fn as_wire(self) -> &'static str {
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
    pub const fn kind(&self) -> TargetKind {
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
    pub const fn spec(&self) -> Option<&SpecLabel> {
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
            std::path::Path::new(path)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
                && line.chars().all(|ch| ch.is_ascii_digit())
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

/// Wire prefix emitted before each finding JSON payload.
pub const LOOM_FINDING_PREFIX: &str = "LOOM_FINDING:";

/// Resolver for finding validation that requires workspace state.
pub trait FindingValidator {
    /// Layer 3 — every element of `bonds` MUST be a known workspace spec label
    /// in the current owner inventory.
    fn spec_label_is_known(&self, label: &SpecLabel) -> bool;

    /// Layer 5 — `Criterion { spec, anchor }` resolves when the owner's current
    /// contract or acceptance document contains the named anchor.
    fn criterion_anchor_resolves(&self, spec: &SpecLabel, anchor: &str) -> bool;

    /// Layer 5 — `Annotation { target_string }` resolves when the
    /// integrity gate's forward-resolution would accept the same string
    /// as a valid annotation target (tier-matched runner ownership or an
    /// ad hoc command on PATH, test name in scope, judge file on disk).
    fn annotation_resolves(&self, target_string: &str) -> bool;

    /// Deterministic failures name a declared annotation even when its command does not resolve.
    fn annotation_is_declared(&self, target_string: &str) -> bool {
        self.annotation_resolves(target_string)
    }

    /// Layer 5 — `TestPath { path }` / `Template { path }` /
    /// `LockSite { file, .. }` resolve when the named file exists on
    /// disk (relative to repo root).
    fn file_exists(&self, path: &str) -> bool;

    /// Layer 5 — `Invariant { spec, section, tag }` resolves when the owner's
    /// current contract declares an invariant matching `(section, tag)`.
    fn invariant_resolves(&self, spec: &SpecLabel, section: &str, tag: &str) -> bool;
}

/// Finding validation failure carrying its source line and record text.
#[expect(
    clippy::doc_markdown,
    reason = "displaydoc fields are format placeholders; wrapping them in backticks makes the generated format string invalid"
)]
#[derive(Debug, Display, Error, Clone, PartialEq, Eq)]
pub enum FindingParseError {
    /// line {line_number}: finding requires at least one bond — `{raw}`
    EmptyBonds { line_number: usize, raw: String },
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
    /// Resolve typed finding input against its dispatch context, retaining its source location.
    fn resolve<V: FindingValidator + ?Sized>(
        raw: RawFinding,
        line_number: usize,
        raw_line: &str,
        scope: DispatchScope,
        validator: &V,
    ) -> Result<Self, FindingParseError> {
        let mut finding = raw;
        finding.target = finding.target.canonicalized();
        if finding.bonds.is_empty() {
            return Err(FindingParseError::EmptyBonds {
                line_number,
                raw: raw_line.to_owned(),
            });
        }

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

        finding.validate(line_number, raw_line, validator)?;
        Ok(Self {
            token: finding.token,
            route: finding.route,
            bonds: finding.bonds,
            target: finding.target,
            evidence: finding.evidence,
        })
    }
}

impl RawFinding {
    /// I/O-bearing validation: Layer 3 (every bond resolves to a known
    /// workspace spec) and Layer 5 (the target's identity-bearing
    /// fields resolve on disk). Pure JSON / closed-set / variant /
    /// bonds-spec rules already fired in [`Finding::resolve`];
    /// this is the second stage that the mint driver runs once the
    /// resolver is wired.
    ///
    /// # Errors
    ///
    /// Returns [`FindingParseError`] when a bond is unknown or the target
    /// does not resolve through `validator`.
    fn validate<V: FindingValidator + ?Sized>(
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
                if matches!(
                    self.token,
                    ConcernToken::VerifierFailed
                        | ConcernToken::DispatchError
                        | ConcernToken::UnresolvedAnnotation
                        | ConcernToken::StubPointing
                        | ConcernToken::UnneededPendingMarker
                        | ConcernToken::InputsProtocolError
                ) {
                    validator.annotation_is_declared(target_string)
                } else {
                    validator.annotation_resolves(target_string)
                }
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
            format!("invariant `{tag}` in section `{section}` not present in spec `{spec}`")
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

/// Review-walk malformation with the maximum well-formed context retained.
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
    /// Canonical framing or phase admission failed; diagnostic context is not authority.
    Protocol {
        context: Box<crate::output::Context>,
        diagnostics: Vec<(crate::output::Span, String)>,
        parsed_findings: Vec<Finding>,
        finding_errors: Vec<FindingParseError>,
    },
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

/// Compatibility diagnostic projection; only canonical decoding can admit a walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalSurface {
    /// Independently decoded completion.
    Complete,
    /// Independently decoded no-op, rejected in Review.
    Noop,
    /// Independently decoded dependency wait, rejected in Review.
    Waiting,
    /// Reason from a decoded typed blocked payload.
    Blocked { reason: String },
    /// Legacy recovery context, never produced by canonical terminal admission.
    Clarify { question: String },
    /// Reason from a decoded typed retry payload.
    Retry { reason: String },
    /// `LOOM_CONCERN: {"summary": "..."}` parsed cleanly.
    Concern { summary: String },
    /// Legacy recovery context; canonical failures retain full context and spans.
    Malformed { payload: String },
    /// No unique independently decoded terminal.
    Missing,
}

impl TerminalSurface {
    /// Canonical marker identity used by route observability.
    #[must_use]
    pub const fn identity(&self) -> &'static str {
        match self {
            Self::Complete => "LOOM_COMPLETE",
            Self::Noop => "LOOM_NOOP",
            Self::Waiting => "LOOM_WAITING",
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
            Self::Waiting => "LOOM_WAITING".to_owned(),
            Self::Blocked { .. } => "LOOM_BLOCKED".to_owned(),
            Self::Clarify { .. } => "LOOM_CLARIFY".to_owned(),
            Self::Retry { .. } => "LOOM_RETRY".to_owned(),
            Self::Concern { summary } => format!("LOOM_CONCERN: {summary}"),
            Self::Malformed { payload } => format!("LOOM_CONCERN: <malformed: {payload}>"),
            Self::Missing => "(no unique independently decoded terminal)".to_owned(),
        }
    }
}

pub use crate::output::Message as ExitSignal;

/// Decode a phase terminal without granting workflow authority.
///
/// # Errors
/// Returns original text, independently decoded context, and located diagnostics.
pub fn parse_exit_signal(
    output: &str,
    phase: crate::output::Phase,
) -> Result<ExitSignal, crate::output::Failure> {
    crate::output::decode(output, phase).map(|session| session.terminal().message.clone())
}

/// Finding or terminal-contract failure from [`parse_walk_output`].
#[expect(
    clippy::doc_markdown,
    reason = "displaydoc fields are format placeholders; wrapping them in backticks makes the generated format string invalid"
)]
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

/// Canonical review admission and resolved diagnostic findings; fields cannot be forged.
#[derive(Debug, Clone)]
pub struct WalkOutput {
    decoded: std::sync::Arc<Result<crate::output::Session, crate::output::Failure>>,
    /// Independently decoded terminal context, not successful admission.
    terminal: TerminalSurface,
    /// Findings that passed strict per-layer validation. Order
    /// preserves stdout emission order.
    findings: Vec<Finding>,
    /// Located finding syntax or resolution diagnostics.
    finding_errors: Vec<FindingParseError>,
}

impl WalkOutput {
    /// Decode agent-origin Review text and resolve independently decoded findings.
    /// Protocol failures preserve raw context and spans, without granting scope evidence.
    pub fn from_stdout<V: FindingValidator + ?Sized>(
        output: &str,
        scope: DispatchScope,
        validator: &V,
    ) -> Self {
        let mut findings = Vec::new();
        let mut finding_errors = Vec::new();
        let decoded = crate::output::decode(output, crate::output::Phase::Review);
        let context = match &decoded {
            Ok(session) => session.context(),
            Err(failure) => failure.context(),
        };
        for located in context.messages() {
            if let crate::output::Message::Finding(raw) = &located.message {
                match Finding::resolve(
                    raw.clone(),
                    located.span.line,
                    &output[located.span.bytes.clone()],
                    scope,
                    validator,
                ) {
                    Ok(finding) => findings.push(finding),
                    Err(error) => finding_errors.push(error),
                }
            }
        }
        if let Err(failure) = &decoded {
            for diagnostic in failure.diagnostics() {
                let raw = &output[diagnostic.span.bytes.clone()];
                if raw.starts_with(LOOM_FINDING_PREFIX) {
                    finding_errors.push(FindingParseError::Json {
                        line_number: diagnostic.span.line,
                        raw: raw.to_owned(),
                        message: format!("{:?}", diagnostic.error),
                    });
                }
            }
        }
        let terminal = terminal_surface_from_context(context);
        Self {
            decoded: std::sync::Arc::new(decoded),
            terminal,
            findings,
            finding_errors,
        }
    }

    /// Canonical decode/admission result, including original context and spans on failure.
    pub fn decoded(&self) -> &Result<crate::output::Session, crate::output::Failure> {
        &self.decoded
    }

    /// Diagnostic recovery context; never a successful admitted walk.
    pub fn protocol_bad_walk(&self) -> Option<BadWalk> {
        match self.decoded() {
            Ok(_) => None,
            Err(failure) => Some(BadWalk::Protocol {
                context: Box::new(failure.context().clone()),
                diagnostics: failure
                    .diagnostics()
                    .iter()
                    .map(|diagnostic| (diagnostic.span.clone(), format!("{:?}", diagnostic.error)))
                    .collect(),
                parsed_findings: self.findings.clone(),
                finding_errors: self.finding_errors.clone(),
            }),
        }
    }

    /// Independently established terminal for diagnostics, not admission.
    #[must_use]
    pub const fn terminal(&self) -> &TerminalSurface {
        &self.terminal
    }

    /// Well-formed findings, in stdout emission order.
    #[must_use]
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    /// Located finding syntax or resolution diagnostics.
    #[must_use]
    pub fn finding_errors(&self) -> &[FindingParseError] {
        &self.finding_errors
    }
}

/// Diagnostic projection from canonical context, never a second parser.
fn terminal_surface_from_context(context: &crate::output::Context) -> TerminalSurface {
    match context.terminal().map(|located| &located.message) {
        Some(ExitSignal::Complete) => TerminalSurface::Complete,
        Some(ExitSignal::Noop) => TerminalSurface::Noop,
        Some(ExitSignal::Waiting) => TerminalSurface::Waiting,
        Some(ExitSignal::Blocked(payload)) => TerminalSurface::Blocked {
            reason: payload.reason.to_string(),
        },
        Some(ExitSignal::Retry(payload)) => TerminalSurface::Retry {
            reason: payload.reason.to_string(),
        },
        Some(ExitSignal::Concern(payload)) => TerminalSurface::Concern {
            summary: payload.summary.to_string(),
        },
        _ => TerminalSurface::Missing,
    }
}

/// Resolve phase-admitted findings whose emission order agrees with their Review terminal.
///
/// # Errors
///
/// Returns [`WalkOutputError`] when a finding is malformed, terminal
/// markers disagree with the finding stream, or review uses a forbidden
/// terminal path.
pub fn parse_walk_output<V: FindingValidator + ?Sized>(
    output: &str,
    scope: DispatchScope,
    validator: &V,
) -> Result<Vec<Finding>, WalkOutputError> {
    let walk = WalkOutput::from_stdout(output, scope, validator);
    if let Some(bad_walk) = walk.protocol_bad_walk() {
        return Err(WalkOutputError::BadWalk { bad_walk });
    }
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
        TerminalSurface::Noop => Err(WalkOutputError::InvalidTerminal {
            marker: ExitSignal::Noop.marker(),
        }),
        TerminalSurface::Waiting if walk.findings().is_empty() => {
            Err(WalkOutputError::InvalidTerminal {
                marker: ExitSignal::Waiting.marker(),
            })
        }
        TerminalSurface::Complete if !walk.findings().is_empty() => Err(WalkOutputError::BadWalk {
            bad_walk: BadWalk::FindingsWithoutConcern {
                finding_count: walk.findings().len(),
                findings: walk.findings().to_vec(),
            },
        }),
        TerminalSurface::Waiting
        | TerminalSurface::Blocked { .. }
        | TerminalSurface::Retry { .. }
            if !walk.findings().is_empty() =>
        {
            Err(WalkOutputError::BadWalk {
                bad_walk: BadWalk::FindingsWithoutConcern {
                    finding_count: walk.findings().len(),
                    findings: walk.findings().to_vec(),
                },
            })
        }
        TerminalSurface::Missing if !walk.findings().is_empty() => {
            Err(WalkOutputError::MissingTerminalMarker {
                findings_count: walk.findings().len(),
            })
        }
        TerminalSurface::Blocked { reason } => Err(WalkOutputError::CannotComplete {
            marker: "LOOM_BLOCKED",
            reason: reason.clone(),
        }),
        TerminalSurface::Retry { reason } => Err(WalkOutputError::CannotComplete {
            marker: "LOOM_RETRY",
            reason: reason.clone(),
        }),
        TerminalSurface::Waiting => Err(WalkOutputError::InvalidTerminal {
            marker: ExitSignal::Waiting.marker(),
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
        assert_eq!(walk.findings(), []);
        assert_eq!(walk.finding_errors(), []);
    }

    #[test]
    fn exit_adapter_returns_canonical_phase_admitted_terminals() {
        use crate::output::Phase;
        for (wire, expected) in [
            ("commentary\nLOOM_COMPLETE", ExitSignal::Complete),
            ("commentary\nLOOM_NOOP", ExitSignal::Noop),
            ("commentary\nLOOM_WAITING", ExitSignal::Waiting),
        ] {
            assert_eq!(parse_exit_signal(wire, Phase::Loop).unwrap(), expected);
        }
        assert!(parse_exit_signal("LOOM_NOOP", Phase::Review).is_err());
        assert!(parse_exit_signal("LOOM_WAITING", Phase::Review).is_err());
    }

    #[test]
    fn exit_adapter_rejects_legacy_prose_scraping_and_invalid_cardinality() {
        use crate::output::Phase;
        for wire in [
            "the actual reason\nLOOM_BLOCKED",
            "the question\nLOOM_CLARIFY",
            "the reason\nLOOM_RETRY",
            "prefix LOOM_BLOCKED",
            "LOOM_COMPLETE\nLOOM_COMPLETE",
            "LOOM_BLOCKED\nLOOM_COMPLETE",
            "LOOM_COMPLETE\ntrailing prose",
            "LOOM_REVIEW_FLAG: retired\nLOOM_COMPLETE",
            "LOOM_COMPLETE LOOM_COMPLETE",
        ] {
            let failure = parse_exit_signal(wire, Phase::Loop).unwrap_err();
            assert_eq!(failure.context().raw(), wire);
            assert!(!failure.diagnostics().is_empty());
        }
    }

    #[test]
    fn exit_adapter_accepts_typed_multiline_reasons_without_prose_repair() {
        use crate::output::Phase;
        let wire = "commentary\nLOOM_RETRY:\n{\n\"reason\":\"sandbox unlinked\\ntry again\"\n}";
        let ExitSignal::Retry(payload) = parse_exit_signal(wire, Phase::Loop).unwrap() else {
            panic!("expected retry");
        };
        assert_eq!(payload.reason.as_str(), "sandbox unlinked\ntry again");
        assert!(
            parse_exit_signal("LOOM_RETRY: {\"reason\":\"raw\nnewline\"}", Phase::Loop).is_err()
        );
    }

    #[test]
    fn concern_payload_parses_as_json_with_summary_field() {
        let wire = r#"LOOM_CONCERN: {"summary":"scope drift with LOOM_COMPLETE in payload"}"#;
        let ExitSignal::Concern(payload) =
            parse_exit_signal(wire, crate::output::Phase::Review).unwrap()
        else {
            panic!("expected concern");
        };
        assert_eq!(
            payload.summary.as_str(),
            "scope drift with LOOM_COMPLETE in payload"
        );
    }

    #[test]
    fn concern_malformed_payload_routes_to_bad_walk_concern_with_literal_payload() {
        for wire in [
            "LOOM_CONCERN: malformed",
            "LOOM_CONCERN: {}",
            "LOOM_CONCERN: {\"summary\":\"\"}",
            "LOOM_CONCERN: {\"summary\":\"scope\"} suffix",
        ] {
            let failure = parse_exit_signal(wire, crate::output::Phase::Review).unwrap_err();
            assert_eq!(failure.context().raw(), wire);
            assert_eq!(failure.diagnostics()[0].span.bytes, 0..wire.len());
        }
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
            "LOOM_RETRY: {\"reason\":\"sandbox cwd unlinked\"}\n",
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
        assert_eq!(
            missing.label(),
            "(no unique independently decoded terminal)"
        );
    }

    #[test]
    fn legacy_token_reason_payload_retains_protocol_failure() {
        let out = "LOOM_CONCERN: verifier-bypass -- test mocks the agent backend\n";
        let failure = parse_exit_signal(out, crate::output::Phase::Review).unwrap_err();
        assert_eq!(failure.context().raw(), out);
        assert!(failure.context().terminal().is_none());
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
        let walk = WalkOutput::from_stdout(output, scope, validator);
        let [error] = walk.finding_errors() else {
            panic!(
                "expected one malformed finding error, got {:?}",
                walk.finding_errors()
            );
        };
        assert!(parse_walk_output(output, scope, validator).is_err());
        (error.clone(), walk.terminal().clone())
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
    fn backtick_wrapped_finding_is_commentary_not_a_live_record() {
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
        assert!(walk.finding_errors().is_empty());
        assert!(walk.decoded().is_ok());
        assert_eq!(
            walk.terminal(),
            &TerminalSurface::Complete,
            "well-formed terminator survives the per-line error",
        );
    }

    #[test]
    fn parse_walk_output_malformed_findings_preserves_all_errors_and_terminal() {
        let bad_json = format!("{LOOM_FINDING_PREFIX} {{not valid json}}");
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
                bad_walk:
                    BadWalk::Protocol {
                        context,
                        finding_errors: errors,
                        ..
                    },
            }) => {
                assert_eq!(errors.len(), 2);
                assert!(
                    errors
                        .iter()
                        .any(|error| matches!(error, FindingParseError::Json { .. }))
                );
                assert!(
                    errors.iter().any(|error| matches!(
                        error,
                        FindingParseError::TokenVariantMismatch { .. }
                    ))
                );
                assert!(
                    matches!(&context.terminal().unwrap().message, crate::output::Message::Concern(payload) if payload.summary.as_str() == "bad findings")
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
        let output = format!("{no_colon}\n{lowercase}\nLOOM_COMPLETE\n");
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
            Err(WalkOutputError::BadWalk {
                bad_walk:
                    BadWalk::Protocol {
                        context,
                        parsed_findings,
                        ..
                    },
            }) => {
                assert_eq!(parsed_findings.len(), 1);
                assert_eq!(context.raw(), output);
                assert!(context.terminal().is_none());
            }
            other => panic!("expected protocol failure, got {other:?}"),
        }
    }

    #[test]
    fn mint_walk_without_findings_still_requires_a_terminal() {
        let output = "preamble with no findings and no markers\n";
        let walk = WalkOutput::from_stdout(output, DispatchScope::Tree, &AlwaysValid);
        assert!(walk.decoded().is_err());
        assert!(parse_walk_output(output, DispatchScope::Tree, &AlwaysValid).is_err());
    }

    #[test]
    fn direct_clarify_record_is_rejected_by_review_admission() {
        let output = "LOOM_CLARIFY: {\"decisions\":[\"lm-decision\"]}\nLOOM_COMPLETE";
        let walk = WalkOutput::from_stdout(output, DispatchScope::Tree, &AlwaysValid);
        let failure = walk.decoded().as_ref().unwrap_err();
        assert_eq!(failure.context().messages().len(), 2);
        assert!(failure.diagnostics().iter().any(|diagnostic| matches!(
            diagnostic.error,
            crate::output::Error::WrongPhase {
                marker: "LOOM_CLARIFY",
                ..
            }
        )));
        assert!(parse_walk_output(output, DispatchScope::Tree, &AlwaysValid).is_err());
    }

    #[test]
    fn cannot_complete_review_terminals_do_not_parse_as_empty_success() {
        for (output, expected_marker, expected_reason) in [
            (
                "LOOM_RETRY: {\"reason\":\"review logs were truncated\"}\n",
                "LOOM_RETRY",
                "review logs were truncated",
            ),
            (
                "LOOM_BLOCKED: {\"reason\":\"cannot access the workspace\"}\n",
                "LOOM_BLOCKED",
                "cannot access the workspace",
            ),
        ] {
            match parse_walk_output(output, DispatchScope::Tree, &AlwaysValid) {
                Err(WalkOutputError::CannotComplete { marker, reason }) => {
                    assert_eq!(marker, expected_marker);
                    assert_eq!(reason, expected_reason);
                }
                other => panic!("expected CannotComplete for {expected_marker}, got {other:?}"),
            }
        }
    }

    #[test]
    fn blocked_review_terminal_without_reason_is_invalid_not_clean() {
        assert!(matches!(
            parse_walk_output("LOOM_BLOCKED\n", DispatchScope::Tree, &AlwaysValid),
            Err(WalkOutputError::BadWalk {
                bad_walk: BadWalk::Protocol { .. }
            })
        ));
    }

    #[test]
    fn noop_terminal_is_not_a_review_walk_success() {
        assert!(matches!(
            parse_walk_output("LOOM_NOOP\n", DispatchScope::Tree, &AlwaysValid),
            Err(WalkOutputError::BadWalk {
                bad_walk: BadWalk::Protocol { .. }
            })
        ));
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
    fn raw_multiline_evidence_is_rejected_without_repair() {
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
        let walk = WalkOutput::from_stdout(output, DispatchScope::Tree, &AlwaysValid);
        let failure = walk.decoded().as_ref().unwrap_err();
        assert_eq!(failure.context().raw(), output);
        assert!(walk.findings().is_empty());
        assert!(!failure.diagnostics().is_empty());
        assert!(parse_walk_output(output, DispatchScope::Tree, &AlwaysValid).is_err());
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
                    BadWalk::Protocol {
                        context,
                        parsed_findings,
                        ..
                    },
            }) => {
                assert_eq!(context.raw(), output);
                assert_eq!(parsed_findings[0].token, ConcernToken::OrphanIntegration);
            }
            other => panic!("expected protocol failure, got {other:?}"),
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
                format!(r#"{{"kind":"StyleRule","rule_id":"RS-3","subject":"{subject}"}}"#);
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
            ConcernToken::ScopeCreep | ConcernToken::ScopeShortfall => FindingTarget::Criterion {
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
                spec: gate,
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
            bonds: vec![gate],
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
            bonds: vec![gate],
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
            bonds: vec![gate],
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

//! Typed wire-format contract for `loom gate`'s findings / concern
//! surface, per `specs/gate.md` § *Canonical contract location*.
//!
//! This module is the single Rust home for the [`Finding`] record + the
//! closed [`ConcernToken`] enum + [`FindingTarget`] / [`TargetKind`] /
//! [`FindingValidator`] / [`FindingParseError`] + [`BadWalk`] /
//! [`TerminalSurface`] + [`WalkOutput`] / [`WalkOutputError`] +
//! [`ExitSignal`] + the [`parse_walk_output`] /
//! [`WalkOutput::from_stdout`] / [`parse_exit_signal`] parsers + the
//! [`LOOM_FINDING_PREFIX`] constant. Consumers re-export the types via
//! `pub use` and never re-declare them — the `finding_no_duplicate_definitions`
//! walker enforces this workspace-wide.
//!
//! # Field-private `WalkOutput`
//!
//! The silent-loss failure class — production caller constructs
//! [`WalkOutput`] with bogus fields and the typed terminal/finding
//! pipeline is bypassed — is structurally unrepresentable because the
//! struct's fields are private at the `loom-protocol` crate boundary.
//! [`WalkOutput::from_stdout`] is the only construction path; consumers
//! read state through [`WalkOutput::terminal`] / [`WalkOutput::findings`]
//! / [`WalkOutput::finding_errors`] accessors.

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
/// shift bonding without invalidating the fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub token: ConcernToken,
    pub bonds: Vec<SpecLabel>,
    pub target: FindingTarget,
    pub evidence: String,
}

impl Finding {
    /// 12-char lowercase-hex stable identifier from
    /// `blake3(token-wire || 0x1F || canonical_form(target))`.
    /// Stability across rubric runs is the load-bearing property;
    /// `bonds` is deliberately omitted so a bonding shift on re-walk
    /// dedups against the existing fix-up bead instead of re-minting.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let mut input = String::new();
        input.push_str(self.token.as_wire());
        input.push('\u{001F}');
        input.push_str(&self.target.canonical_form());
        let hash = blake3::hash(input.as_bytes());
        let hex = hash.to_hex();
        hex.as_str()[..FINGERPRINT_HEX_LEN].to_owned()
    }
}

/// Length of the [`Finding::fingerprint`] in hex characters. Chosen to
/// fit a bd label and to keep the fingerprint visually scannable
/// alongside bead ids; the only invariant is stability across runs.
const FINGERPRINT_HEX_LEN: usize = 12;

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
    #[serde(rename = "scope-creep")]
    ScopeCreep,
    #[serde(rename = "scope-shortfall")]
    ScopeShortfall,
}

impl ConcernToken {
    /// Canonical wire string used in `LOOM_FINDING:` JSON, bd labels,
    /// and fingerprint input. Matches the leftmost column in
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
            Self::VerifierFailed => "verifier-failed",
            Self::DispatchError => "dispatch-error",
            Self::UnresolvedAnnotation => "unresolved-annotation",
            Self::StubPointing => "stub-pointing",
            Self::MultipleAnnotations => "multiple-annotations",
            Self::UnneededPendingMarker => "unneeded-pending-marker",
            Self::ScopeCreep => "scope-creep",
            Self::ScopeShortfall => "scope-shortfall",
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
            | Self::ScopeCreep
            | Self::ScopeShortfall => TargetKind::Criterion,
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
            | Self::UnneededPendingMarker => TargetKind::Annotation,
            Self::MockDiscipline => TargetKind::TestPath,
            Self::ConcurrencyUntested => TargetKind::LockSite,
            Self::InvariantClash => TargetKind::Invariant,
            Self::TemplateSpecDrift => TargetKind::Template,
        }
    }

    /// Scope class for the token — which dispatch scopes the parse
    /// pipeline admits the token from. Per `specs/gate.md` § *Concern
    /// tokens and target variants* and § *Scope-dependent walk*:
    ///
    /// - `template-spec-drift` / `verifier-failed` / `dispatch-error` /
    ///   `unresolved-annotation` / `stub-pointing` /
    ///   `multiple-annotations` / `unneeded-pending-marker` are emitted
    ///   only at `--tree` scope (deterministic verifier dispatch and
    ///   integrity-gate sources run only there).
    /// - `scope-creep` / `scope-shortfall` are per-bead-only — the
    ///   tree-scope walk never emits them.
    /// - Everything else is admissible at any scope.
    #[must_use]
    pub fn scope_kind(self) -> ScopeKind {
        match self {
            Self::TemplateSpecDrift
            | Self::VerifierFailed
            | Self::DispatchError
            | Self::UnresolvedAnnotation
            | Self::StubPointing
            | Self::MultipleAnnotations
            | Self::UnneededPendingMarker => ScopeKind::TreeOnly,
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
            | Self::InvariantClash => ScopeKind::AnyScope,
        }
    }
}

/// Dispatch scope the parse pipeline ran under — `--bead` / `--diff` /
/// `--files` collapse to [`Self::PerBead`]; `--tree` is [`Self::Tree`].
/// Threaded into [`Finding::parse_payload`], [`WalkOutput::from_stdout`],
/// and [`parse_walk_output`] so token-scope alignment is enforced at the
/// wire boundary per `specs/gate.md` § *Concern tokens and target
/// variants* (criterion `tree_scope_only_tokens_rejected_at_non_tree_scope`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchScope {
    /// `--bead <id>` / `--diff <range>` / `--files <paths>` — the
    /// per-bead walks. Tree-scope-only tokens (verifier / integrity
    /// surfaces, template-spec-drift, etc.) are rejected.
    PerBead,
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
    /// dispatch, integrity-gate surfaces, tree-only rubric checks);
    /// rejected at per-bead scopes.
    TreeOnly,
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
            | (Self::TreeOnly, DispatchScope::Tree) => true,
            (Self::PerBead, DispatchScope::Tree) | (Self::TreeOnly, DispatchScope::PerBead) => {
                false
            }
        }
    }

    /// Stable label for error messages.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::PerBead => "per-bead-only",
            Self::TreeOnly => "tree-only",
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
            Self::Criterion { spec, .. } | Self::Invariant { spec, .. } => Some(spec),
            Self::Contract { .. }
            | Self::StyleRule { .. }
            | Self::Annotation { .. }
            | Self::TestPath { .. }
            | Self::LockSite { .. }
            | Self::Template { .. } => None,
        }
    }

    /// Variant-aware canonical string fed into the fingerprint hash.
    /// Round-trips the identity-bearing fields in a fixed shape so the
    /// same logical finding hashes to the same digest regardless of
    /// how the rubric phrased it.
    #[must_use]
    pub fn canonical_form(&self) -> String {
        match self {
            Self::Criterion { spec, anchor } => format!("criterion:{spec}:{anchor}"),
            Self::Contract { id } => format!("contract:{id}"),
            Self::StyleRule { rule_id } => format!("style:{rule_id}"),
            Self::Annotation { target_string } => format!("annotation:{target_string}"),
            Self::TestPath { path } => format!("test:{path}"),
            Self::LockSite { file, line } => format!("lock:{file}:{line}"),
            Self::Invariant { spec, section, tag } => {
                format!("invariant:{spec}:{section}:{tag}")
            }
            Self::Template { path } => format!("template:{path}"),
        }
    }
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
/// offending line's 1-based number and verbatim text so a re-run prompt
/// has the evidence it needs (per `specs/gate.md` § *Strict
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
    /// line {line_number}: token `{token}` is {scope_kind} but the walk runs at {dispatch_scope} scope — `{raw}`
    TokenScopeMismatch {
        line_number: usize,
        token: &'static str,
        scope_kind: &'static str,
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
        let finding: Finding =
            serde_json::from_str(payload).map_err(|source| FindingParseError::Json {
                line_number,
                raw: raw_line.to_owned(),
                message: source.to_string(),
            })?;

        let expected_kind = finding.token.expected_target_kind();
        let actual_kind = finding.target.kind();
        if expected_kind != actual_kind {
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
            FindingTarget::Contract { .. } | FindingTarget::StyleRule { .. } => true,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BadWalk {
    /// `LOOM_CONCERN:` payload did not parse as
    /// `{"summary": "<non-empty>"}` — invalid JSON, missing
    /// `summary` field, or empty `summary`. The literal post-marker
    /// text is preserved for the recovery prompt, alongside any
    /// `LOOM_FINDING:` lines that streamed cleanly before the bad
    /// terminator.
    Concern {
        payload: String,
        parsed_findings: Vec<Finding>,
    },

    /// Terminator claimed concern but zero `LOOM_FINDING:` lines
    /// streamed during the walk. The parsed summary is preserved
    /// so the recovery prompt can quote it back.
    ConcernWithoutFindings { summary: String },

    /// One or more `LOOM_FINDING:` lines streamed but the
    /// terminator was `LOOM_COMPLETE`. The parsed findings ride
    /// through so the next iteration's prompt can name them
    /// per the pairing-rule table in `specs/gate.md`.
    FindingsWithoutConcern {
        finding_count: usize,
        findings: Vec<Finding>,
    },

    /// One or more `LOOM_FINDING:` lines failed strict validation.
    /// The well-formed terminal surface rides through alongside the
    /// per-line errors so the recovery prompt can name both pieces
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
    /// adjacent prose read by the parser.
    Blocked { reason: String },
    /// `LOOM_CLARIFY` on the final non-empty line; `question` is the
    /// adjacent prose read by the parser.
    Clarify { question: String },
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
    /// Stable label used in `BadWalk::MalformedFinding` recovery
    /// rendering so the agent sees what the terminal looked like
    /// alongside the per-finding errors.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Complete => "LOOM_COMPLETE",
            Self::Noop => "LOOM_NOOP",
            Self::Blocked { .. } => "LOOM_BLOCKED",
            Self::Clarify { .. } => "LOOM_CLARIFY",
            Self::Concern { .. } => "LOOM_CONCERN (well-formed)",
            Self::Malformed { .. } => "LOOM_CONCERN (malformed payload)",
            Self::Missing => "no terminal marker on the final non-empty line",
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

    /// Agent could not proceed; the driver surfaces the reason to the user
    /// without advancing state.
    Blocked { reason: String },

    /// Agent needs human input; the driver applies the `loom:clarify`
    /// label and bails.
    Clarify { question: String },

    /// Review-phase concern. Carries the parsed `summary` field from the
    /// terminal `LOOM_CONCERN: {"summary": "..."}` marker. The summary is
    /// for the verdict log only; per-finding routing is decided on each
    /// streamed `LOOM_FINDING:` line's token per `specs/gate.md` §
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

const COMPLETE: &str = "LOOM_COMPLETE";
const NOOP: &str = "LOOM_NOOP";
const BLOCKED: &str = "LOOM_BLOCKED";
const CLARIFY: &str = "LOOM_CLARIFY";
const CONCERN: &str = "LOOM_CONCERN";

/// Scan the agent's combined output (or the `result` field of the final
/// stream-json line) for an exit signal.
///
/// The parser inspects **only the final non-empty line** of `output`. Any
/// marker emitted earlier in the session is treated as swallowed; multiple
/// markers on the final line likewise collapse to `None` per the
/// mutual-exclusivity rule in `specs/harness.md` § Marker definitions.
///
/// `LOOM_BLOCKED` and `LOOM_CLARIFY` are bare markers — no trailing colon,
/// no trailing payload. The reason / question is read from the text
/// **before** the marker on the final line, falling back to the most recent
/// non-empty line before the final line if the same-line prefix is empty.
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

    if has_multiple_markers(final_line) {
        return None;
    }

    if let Some(idx) = final_line.find(CONCERN) {
        return Some(parse_concern(&final_line[idx + CONCERN.len()..]));
    }
    if let Some(reason) = reason_for(BLOCKED, final_line, prior) {
        return Some(ExitSignal::Blocked { reason });
    }
    if let Some(question) = reason_for(CLARIFY, final_line, prior) {
        return Some(ExitSignal::Clarify { question });
    }
    if final_line.contains(COMPLETE) {
        return Some(ExitSignal::Complete);
    }
    if final_line.contains(NOOP) {
        return Some(ExitSignal::Noop);
    }
    None
}

/// Count distinct marker keywords on `line`. The keywords are matched as
/// substrings; `CONCERN` is a substring of nothing else in the set, and
/// the others are pairwise non-overlapping, so distinct hits map one-to-one
/// to distinct markers.
fn has_multiple_markers(line: &str) -> bool {
    let markers = [COMPLETE, NOOP, BLOCKED, CLARIFY, CONCERN];
    let mut hits = 0;
    for marker in markers {
        if line.contains(marker) {
            hits += 1;
            if hits > 1 {
                return true;
            }
        }
    }
    false
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

fn reason_for(marker: &str, line: &str, prior: &[&str]) -> Option<String> {
    let idx = line.find(marker)?;
    let same_line = line[..idx].trim();
    if !same_line.is_empty() {
        return Some(same_line.to_string());
    }
    for prev in prior.iter().rev() {
        let trimmed = prev.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    Some(String::new())
}

/// Top-level error for [`parse_walk_output`]. Either a per-line
/// validation failure or the terminal-marker enforcement — a walk that
/// emits `LOOM_FINDING:` lines without a terminal marker per
/// `specs/gate.md` § *Findings and Minting*.
#[derive(Debug, Display, Error)]
pub enum WalkOutputError {
    /// per-line parse failure: {0}
    Finding(#[from] FindingParseError),
    /// walk emitted {findings_count} LOOM_FINDING line(s) but no terminal marker (LOOM_COMPLETE / LOOM_CONCERN / LOOM_BLOCKED / LOOM_CLARIFY)
    MissingTerminalMarker { findings_count: usize },
}

/// Typed product the review-phase classifier consumes — a single
/// pre-parsed snapshot of the agent's stdout containing the typed
/// terminal surface, the well-formed [`Finding`] records, and any
/// per-line parse errors.
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
    /// Per-line parse failures for `LOOM_FINDING:` substring matches
    /// that did not pass strict validation. Carries the offending
    /// 1-based line number and verbatim line text so the recovery
    /// prompt can quote it back.
    finding_errors: Vec<FindingParseError>,
}

impl WalkOutput {
    /// Parse the agent's combined stdout into a typed `WalkOutput`.
    /// Runs `LOOM_FINDING:` substring search, strict per-line
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
        for (idx, line) in output.lines().enumerate() {
            let line_number = idx + 1;
            let Some(payload_start) = line.find(LOOM_FINDING_PREFIX) else {
                continue;
            };
            let payload = line[payload_start + LOOM_FINDING_PREFIX.len()..].trim_start();
            match Finding::parse_payload(payload, line_number, line, scope)
                .and_then(|f| f.validate(line_number, line, validator).map(|()| f))
            {
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

    /// Per-line parse failures for `LOOM_FINDING:` substring matches
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
        Some(ExitSignal::Concern { summary }) => TerminalSurface::Concern { summary },
        Some(ExitSignal::BadWalk(BadWalk::Concern { payload, .. })) => {
            TerminalSurface::Malformed { payload }
        }
        Some(ExitSignal::BadWalk(_)) | None => TerminalSurface::Missing,
    }
}

/// Scan `output` for `LOOM_FINDING:` lines, parse and fully-validate
/// each (Layers 1–5 plus the `target.spec ∈ bonds` rule), and enforce
/// the terminal-marker rule: an output that emits one or more findings
/// but no terminal marker (`LOOM_COMPLETE` / `LOOM_CONCERN` /
/// `LOOM_BLOCKED` / `LOOM_CLARIFY`) is rejected with
/// [`WalkOutputError::MissingTerminalMarker`].
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
    let mut findings = Vec::new();
    for (idx, line) in output.lines().enumerate() {
        let line_number = idx + 1;
        let Some(payload_start) = line.find(LOOM_FINDING_PREFIX) else {
            continue;
        };
        let payload = line[payload_start + LOOM_FINDING_PREFIX.len()..].trim_start();
        let finding = Finding::parse_payload(payload, line_number, line, scope)?;
        finding.validate(line_number, line, validator)?;
        findings.push(finding);
    }
    if !findings.is_empty() && parse_exit_signal(output).is_none() {
        return Err(WalkOutputError::MissingTerminalMarker {
            findings_count: findings.len(),
        });
    }
    Ok(findings)
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
    fn fingerprint_is_twelve_lowercase_hex_chars() {
        let fp = sample_finding().fingerprint();
        assert_eq!(fp.len(), 12, "fingerprint width is 12 hex chars");
        assert!(
            fp.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "fingerprint is lowercase hex: {fp}",
        );
    }

    #[test]
    fn fingerprint_is_stable_across_runs_for_same_finding() {
        let a = finding(
            ConcernToken::SpecCoherenceFail,
            vec![spec("gate"), spec("harness")],
            FindingTarget::Criterion {
                spec: spec("gate"),
                anchor: "verifier-honesty".to_owned(),
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
        assert_eq!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn fingerprint_excludes_bonds() {
        let identity = (
            ConcernToken::OrphanIntegration,
            FindingTarget::Contract {
                id: "molecule-lifecycle".to_owned(),
            },
        );
        let single_spec = finding(identity.0, vec![spec("harness")], identity.1.clone(), "");
        let multi_spec = finding(
            identity.0,
            vec![spec("gate"), spec("harness")],
            identity.1,
            "",
        );
        assert_eq!(single_spec.fingerprint(), multi_spec.fingerprint());
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
            }
            .canonical_form(),
            "style:RS-12",
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
            (ConcernToken::VerifierFailed, TargetKind::Annotation),
            (ConcernToken::DispatchError, TargetKind::Annotation),
            (ConcernToken::UnresolvedAnnotation, TargetKind::Annotation),
            (ConcernToken::StubPointing, TargetKind::Annotation),
            (ConcernToken::MultipleAnnotations, TargetKind::Criterion),
            (ConcernToken::UnneededPendingMarker, TargetKind::Annotation),
            (ConcernToken::ScopeCreep, TargetKind::Criterion),
            (ConcernToken::ScopeShortfall, TargetKind::Criterion),
        ] {
            assert_eq!(token.expected_target_kind(), expected, "{token:?}");
        }
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
    fn marker_at_start_with_no_prior_lines_yields_empty_reason() {
        let out = "LOOM_BLOCKED";
        match parse_exit_signal(out) {
            Some(ExitSignal::Blocked { reason }) => assert!(reason.is_empty()),
            other => panic!("expected Blocked with empty reason, got {other:?}"),
        }
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
            r#"{{"token":"{token}","bonds":[{bonds_json}],"target":{target_json},"evidence":"{evidence}"}}"#,
        )
    }

    fn finding_line(token: &str, bonds: &[&str], target_json: &str, evidence: &str) -> String {
        format!(
            "{} {}",
            LOOM_FINDING_PREFIX,
            payload(token, bonds, target_json, evidence)
        )
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
            "preamble\n{line_a}\nintermediate prose\n{line_b}\nLOOM_CONCERN: verifier-bypass -- two findings"
        );
        let findings =
            parse_walk_output(&output, DispatchScope::Tree, &AlwaysValid).expect("parses cleanly");
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].token, ConcernToken::SpecCoherenceFail);
        assert_eq!(findings[0].evidence, "first finding");
        assert_eq!(findings[1].token, ConcernToken::OrphanIntegration);
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
    fn mint_walk_accepts_each_terminal_marker_variant() {
        let line = finding_line(
            "orphan-integration",
            &["harness"],
            r#"{"kind":"Contract","id":"x"}"#,
            "",
        );
        for terminal in ["LOOM_COMPLETE", "LOOM_BLOCKED", "LOOM_CLARIFY"] {
            let output = format!("{line}\nreason for {terminal}\n{terminal}\n");
            parse_walk_output(&output, DispatchScope::Tree, &AlwaysValid)
                .unwrap_or_else(|e| panic!("{terminal} should accept: {e}"));
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
        let output = format!("{line}\nLOOM_CONCERN: orphan-integration -- found one\n");
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
        let valid_terminal = "LOOM_CONCERN: orphan-integration -- summary";

        let line = format!("{LOOM_FINDING_PREFIX} {{not valid json");
        let output = format!("{line}\n{valid_terminal}\n");
        match parse_walk_output(&output, DispatchScope::Tree, &AlwaysValid) {
            Err(WalkOutputError::Finding(FindingParseError::Json {
                line_number, raw, ..
            })) => {
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
        match parse_walk_output(&output, DispatchScope::Tree, &AlwaysValid) {
            Err(WalkOutputError::Finding(FindingParseError::Json {
                line_number, raw, ..
            })) => {
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
        match parse_walk_output(&output, DispatchScope::Tree, &known) {
            Err(WalkOutputError::Finding(FindingParseError::UnknownBondSpec {
                line_number,
                spec: bad,
                raw,
            })) => {
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
        match parse_walk_output(&output, DispatchScope::Tree, &AlwaysValid) {
            Err(WalkOutputError::Finding(FindingParseError::TokenVariantMismatch {
                line_number,
                token,
                expected,
                actual,
                raw,
            })) => {
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
        match parse_walk_output(&output, DispatchScope::Tree, &NothingResolves) {
            Err(WalkOutputError::Finding(FindingParseError::UnresolvedTarget {
                line_number,
                detail,
                raw,
            })) => {
                assert_eq!(line_number, 1);
                assert!(detail.contains("missing-anchor"), "detail: {detail}");
                assert!(raw.contains("missing-anchor"));
            }
            other => panic!("expected UnresolvedTarget, got {other:?}"),
        }
    }

    #[test]
    fn mint_rejects_criterion_target_whose_spec_is_not_in_bonds() {
        let line = finding_line(
            "spec-coherence-fail",
            &["harness"],
            r#"{"kind":"Criterion","spec":"gate","anchor":"verifier-honesty"}"#,
            "criterion belongs to gate but bonds names harness only",
        );
        let output = format!("{line}\nLOOM_CONCERN: spec-coherence-fail -- bad bonds\n");
        match parse_walk_output(&output, DispatchScope::Tree, &AlwaysValid) {
            Err(WalkOutputError::Finding(FindingParseError::TargetSpecNotInBonds {
                line_number,
                spec: missing,
                bonds,
                raw,
            })) => {
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
        let output = format!("{line}\nLOOM_CONCERN: invariant-clash -- bad bonds\n");
        match parse_walk_output(&output, DispatchScope::Tree, &AlwaysValid) {
            Err(WalkOutputError::Finding(FindingParseError::TargetSpecNotInBonds {
                spec: missing,
                ..
            })) => {
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
        let output = format!("{line}\nLOOM_CONCERN: spec-coherence-fail -- ok\n");
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
            ConcernToken::ScopeCreep => FindingTarget::Criterion {
                spec: gate.clone(),
                anchor: "scope-appropriateness".to_owned(),
            },
            ConcernToken::ScopeShortfall => FindingTarget::Criterion {
                spec: gate.clone(),
                anchor: "scope-appropriateness".to_owned(),
            },
        }
    }

    #[test]
    fn every_finding_round_trips_through_wire_format_with_stable_fingerprint() {
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
            ConcernToken::VerifierFailed,
            ConcernToken::DispatchError,
            ConcernToken::UnresolvedAnnotation,
            ConcernToken::StubPointing,
            ConcernToken::MultipleAnnotations,
            ConcernToken::UnneededPendingMarker,
            ConcernToken::ScopeCreep,
            ConcernToken::ScopeShortfall,
        ];
        let terminators = [
            "LOOM_COMPLETE",
            "LOOM_CONCERN: {\"summary\":\"round-trip\"}",
        ];

        for token in tokens {
            let target = canonical_target(token, &gate);
            assert_eq!(
                token.expected_target_kind(),
                target.kind(),
                "canonical pairing self-check for {}",
                token.as_wire(),
            );
            let bonds = match target.spec() {
                Some(s) => vec![s.clone()],
                None => vec![gate.clone()],
            };
            let input = Finding {
                token,
                bonds,
                target,
                evidence: format!("round-trip evidence for {}", token.as_wire()),
            };
            let payload = serde_json::to_string(&input).expect("serialize finding");

            let dispatch_scope = match token.scope_kind() {
                ScopeKind::PerBead => DispatchScope::PerBead,
                ScopeKind::TreeOnly | ScopeKind::AnyScope => DispatchScope::Tree,
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
                    round.fingerprint(),
                    input.fingerprint(),
                    "fingerprint stability for {} with terminator `{terminator}`",
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
    /// per-bead dispatch scope, and per-bead-only tokens
    /// (`scope-creep` / `scope-shortfall`) surface the same error at
    /// tree dispatch scope. Anywhere-admissible tokens pass at both
    /// scopes.
    #[test]
    fn tree_scope_only_tokens_rejected_at_non_tree_scope() {
        let gate = spec("gate");
        let terminator = "LOOM_CONCERN: {\"summary\":\"scope mismatch\"}";

        let tree_only = [
            ConcernToken::TemplateSpecDrift,
            ConcernToken::VerifierFailed,
            ConcernToken::DispatchError,
            ConcernToken::UnresolvedAnnotation,
            ConcernToken::StubPointing,
            ConcernToken::MultipleAnnotations,
            ConcernToken::UnneededPendingMarker,
        ];
        for token in tree_only {
            assert_eq!(token.scope_kind(), ScopeKind::TreeOnly, "{token:?}");

            let finding = Finding {
                token,
                bonds: vec![gate.clone()],
                target: canonical_target(token, &gate),
                evidence: "scope mismatch fixture".to_owned(),
            };
            let payload = serde_json::to_string(&finding).expect("serialize");
            let output = format!("preamble\n{LOOM_FINDING_PREFIX} {payload}\n{terminator}\n");

            match parse_walk_output(&output, DispatchScope::PerBead, &AlwaysValid) {
                Err(WalkOutputError::Finding(FindingParseError::TokenScopeMismatch {
                    token: bad_token,
                    scope_kind,
                    dispatch_scope,
                    ..
                })) => {
                    assert_eq!(bad_token, token.as_wire());
                    assert_eq!(scope_kind, "tree-only");
                    assert_eq!(dispatch_scope, "per-bead");
                }
                other => panic!(
                    "expected TokenScopeMismatch for tree-only token `{}` at per-bead scope, got {other:?}",
                    token.as_wire(),
                ),
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
                bonds: vec![gate.clone()],
                target: canonical_target(token, &gate),
                evidence: "scope mismatch fixture".to_owned(),
            };
            let payload = serde_json::to_string(&finding).expect("serialize");
            let output = format!("preamble\n{LOOM_FINDING_PREFIX} {payload}\n{terminator}\n");

            match parse_walk_output(&output, DispatchScope::Tree, &AlwaysValid) {
                Err(WalkOutputError::Finding(FindingParseError::TokenScopeMismatch {
                    token: bad_token,
                    scope_kind,
                    dispatch_scope,
                    ..
                })) => {
                    assert_eq!(bad_token, token.as_wire());
                    assert_eq!(scope_kind, "per-bead-only");
                    assert_eq!(dispatch_scope, "tree");
                }
                other => panic!(
                    "expected TokenScopeMismatch for per-bead-only token `{}` at tree scope, got {other:?}",
                    token.as_wire(),
                ),
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
            bonds: vec![gate.clone()],
            target: FindingTarget::Criterion {
                spec: gate.clone(),
                anchor: "verifier-honesty".to_owned(),
            },
            evidence: "any-scope token".to_owned(),
        };
        let payload = serde_json::to_string(&any_scope_finding).expect("serialize");
        let output = format!("preamble\n{LOOM_FINDING_PREFIX} {payload}\n{terminator}\n");
        for scope in [DispatchScope::PerBead, DispatchScope::Tree] {
            parse_walk_output(&output, scope, &AlwaysValid).unwrap_or_else(|e| {
                panic!("AnyScope token must parse at {} scope: {e}", scope.label())
            });
        }
    }
}

//! Placeholder tests for `loom gate mint` annotations whose
//! implementation has not landed yet.
//!
//! Each `#[test]` here exists so `loom gate verify`'s integrity walk
//! resolves the matching `[test](mint_*)` annotation in
//! `specs/gate.md` / `specs/templates.md` by leaf name. The body stays
//! empty until the relevant `loom gate mint` slice lands; once it does,
//! the stub is deleted in the same PR that adds the real `#[test]`
//! function under the module it tests.

use std::path::PathBuf;

use loom_gate::{IntegrityFinding, Tier, compose_clarify_options};

#[test]
fn mint_applies_per_spec_default_profile_label_to_created_beads() {}

/// `specs/gate.md` § *Integrity gate — pending modifier* (criterion
/// `mint_emits_drop_marker_option_for_unneeded_pending_marker`): when
/// `UnneededPendingMarker` reaches the mint clarify-options surface,
/// the generated `## Options — …` block leads with a "Drop the `?`"
/// option naming spec, line, tier, and target. The block is the
/// auto-generated payload mint embeds when the stale-marker finding
/// raises `loom:clarify`.
#[test]
fn mint_emits_drop_marker_option_for_unneeded_pending_marker() {
    let finding = IntegrityFinding::UnneededPendingMarker {
        spec: PathBuf::from("specs/gate.md"),
        line: 803,
        tier: Tier::Check,
        target: "true".into(),
    };
    let out = compose_clarify_options(&[finding]);
    assert!(
        out.starts_with("## Options — "),
        "block must start with the options summary heading: {out}",
    );
    assert!(
        out.contains("specs/gate.md:803"),
        "spec:line missing: {out}"
    );
    assert!(
        out.contains('`') && out.contains("true"),
        "target must be named: {out}",
    );
    assert!(
        out.contains("### Option 1 — Drop the `?`"),
        "Option 1 must lead with 'Drop the `?`' language: {out}",
    );
}

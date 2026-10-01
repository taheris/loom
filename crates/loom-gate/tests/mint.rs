//! Clarify options for integrity findings.

use std::path::PathBuf;

use loom_gate::{IntegrityFinding, Tier, compose_clarify_options};

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

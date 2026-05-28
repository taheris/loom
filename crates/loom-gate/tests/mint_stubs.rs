//! Placeholder tests for `loom gate mint` annotations whose
//! implementation has not landed yet.
//!
//! Each `#[test]` here exists so `loom gate verify`'s integrity walk
//! resolves the matching `[test](mint_*)` annotation in
//! `specs/gate.md` / `specs/templates.md` by leaf name. The body stays
//! empty until the relevant `loom gate mint` slice lands; once it does,
//! the stub is deleted in the same PR that adds the real `#[test]`
//! function under the module it tests.

#[test]
fn mint_bead_scope_walks_llm_rubric_only_not_verifiers() {}

#[test]
fn mint_tree_scope_walks_verifiers_and_rubric_emitting_findings_from_both() {}

#[test]
fn mint_idempotent_after_partial_failure_retries_only_unfinished_findings() {}

#[test]
fn mint_applies_per_spec_default_profile_label_to_created_beads() {}

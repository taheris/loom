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
fn mint_refuses_when_loom_inside_env_is_set() {}

#[test]
fn mint_bead_scope_walks_llm_rubric_only_not_verifiers() {}

#[test]
fn mint_tree_scope_walks_verifiers_and_rubric_emitting_findings_from_both() {}

#[test]
fn mint_idempotent_after_partial_failure_retries_only_unfinished_findings() {}

#[test]
fn mint_dry_run_makes_no_bd_writes() {}

#[test]
fn mint_spec_filter_drops_findings_routing_to_other_specs() {}

#[test]
fn mint_bare_invocation_defaults_to_active_molecule_diff() {}

#[test]
fn mint_applies_per_spec_default_profile_label_to_created_beads() {}

#[test]
fn audit_tree_scope_makes_no_bd_writes() {}

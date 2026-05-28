//! Placeholder tests for the `loom gate mint` annotations in
//! `specs/gate.md` and `specs/templates.md`.
//!
//! The spec declares the mint contract ahead of the implementation. Each
//! `#[test]` here exists solely so `loom gate verify`'s integrity walk
//! resolves the matching `[test](mint_*)` annotation by leaf name. The
//! body stays empty until `loom gate mint` lands; once it does, each stub
//! is replaced by a real assertion against the contract from the
//! cited spec section.

#[test]
fn mint_tree_scope_resolves_lead_spec_via_single_tier_query() {}

#[test]
fn mint_tree_scope_per_spec_resolution_does_not_clobber_existing_epics() {}

#[test]
fn mint_refuses_when_loom_inside_env_is_set() {}

#[test]
fn mint_dedup_query_one_open_result_skips_finding() {}

#[test]
fn mint_dedup_query_zero_results_proceeds_to_mint() {}

#[test]
fn mint_dedup_query_multiple_open_results_refuses_as_structural_violation() {}

#[test]
fn mint_dedup_does_not_re_mint_closed_bead_with_same_fingerprint() {}

#[test]
fn mint_dedup_skips_reopened_bead_still_carrying_fingerprint_label() {}

#[test]
fn mint_fingerprint_is_stable_across_rubric_runs_for_same_finding() {}

#[test]
fn mint_creates_fixup_with_parent_epic_and_fingerprint_label() {}

#[test]
fn mint_bonding_lead_is_first_bonds_element_with_open_epic() {}

#[test]
fn mint_fingerprint_excludes_bonds_so_bonding_shifts_do_not_remint() {}

#[test]
fn mint_invariant_clash_finding_creates_fixup_with_clarify_label_and_options_block() {}

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
fn mint_end_of_run_summary_reports_per_finding_outcomes() {}

#[test]
fn mint_applies_per_spec_default_profile_label_to_created_beads() {}

#[test]
fn audit_tree_scope_makes_no_bd_writes() {}

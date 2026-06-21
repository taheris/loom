//! Walk registry — the name → walk function table the binary dispatches
//! against.
//!
//! Each walk is a function in `walk/<name>.rs` that matches [`WalkFn`].
//! [`REGISTRY`] lists them by name; the dispatcher in `main.rs` looks
//! up the named walk, runs it, and emits the [`Verdict`] as a JSON
//! line on stdout per the verifier-runner contract in
//! `specs/gate.md`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use inputs::render_print_inputs;

mod audit_makes_no_bd_writes_outside_mint_module;
mod cli_surface;
mod crate_structure;
mod direct_tools_net_new;
mod event_sink_in_loom_events;
mod finding_no_duplicate_definitions;
mod git_client_encapsulation;
mod inputs;
mod loom_agent_deps;
mod loom_does_not_invoke_podman;
mod loom_events_is_leaf;
mod loom_events_minimal_deps;
mod loom_gate_check_derivation_exists;
mod loom_llm_client_constructors_use_newtypes;
mod loom_llm_client_types_per_schema_kind;
mod loom_llm_deps;
mod loom_llm_error_variant_set_multimodal;
mod loom_llm_has_no_skill_registry_surface;
mod loom_llm_mime_type_no_raw_strings;
mod loom_llm_multimodal_no_provider_wire_types;
mod loom_llm_no_public_genai_types;
mod loom_llm_no_underlying_crate_reexports;
mod loom_llm_public_surface;
mod loom_render_deps;
mod loom_skills_deps;
mod loom_templates_deps;
mod loom_templates_public_partial_constants;
mod loom_templates_public_types;
mod loom_templates_snapshots_no_crate_root_allow;
mod loom_templates_workflow_templates_not_exported;
mod loom_tune_deps;
mod newtype_identifiers;
mod nix_flake_check_excludes_workspace_compile;
mod no_allow_dead_code;
mod no_derive_from_on_newtypes;
mod no_hardcoded_tmp_paths;
mod no_inline_suppression_comment_contract;
mod no_panics_in_production;
mod no_real_clock_outside_system_clock;
mod no_thread_sleep;
mod no_todo_cursor_meta_key;
mod no_tokio_sleep_outside_clock;
mod no_tokio_timeout_outside_clock;
mod no_types_or_error_files;
mod observers_in_loom_llm;
mod phase_verdict_decide_called_from_production;
mod public_contract_crates;
mod renderer_no_insta_dependency;
mod result_hasher_single_call_site;
mod session_trait_does_not_expose_typestate;
mod session_trait_in_loom_events;
mod single_event_channel;
mod surface_conformance;
mod template_context_structs;
mod template_pinning_matrix;
mod template_wire_format_restatement;
mod templates_no_removed_surface;
mod todo_contexts_carry_criterion_status;
mod todo_template_uses_driver_created_work_epic;
mod tune_surface_conformance;
mod util;
mod workspace_deps_pinned;
mod workspace_edition;
mod workspace_lints;

/// Verdict each walk returns. Matches the verifier-runner contract in
/// `specs/gate.md`: a JSON line `{"pass": bool, "evidence": "<msg>"}`
/// on stdout, exit code mirroring `pass`. By convention `evidence` for a
/// `false` verdict carries `<path>:<line> <rule-id>` so reviewers can click
/// into the violation; this struct does not enforce that shape — the rule
/// lives in the spec, the type is the wire envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub pass: bool,
    pub evidence: String,
}

/// Input passed to every walk. `files` is `Some` when the gate set
/// `LOOM_FILES` (the `--files` scope); the walk filters its own scan
/// against the set. `None` means scan the walk's declared scope in full.
#[derive(Debug, Clone, Default)]
pub struct WalkInput {
    pub files: Option<Vec<PathBuf>>,
}

impl WalkInput {
    /// Build from process env. An unset or empty `LOOM_FILES` resolves to
    /// `files: None` so an unfiltered run scans the walk's full scope; any
    /// non-empty value is split on `:` (the colon-separated convention from
    /// the verifier-runner contract).
    pub fn from_env() -> Self {
        let Ok(raw) = std::env::var("LOOM_FILES") else {
            return Self { files: None };
        };
        Self::parse_files(&raw)
    }

    /// Parse a raw `LOOM_FILES` value into a `WalkInput`. Empty input
    /// resolves to `files: None` so the empty-string and unset cases agree.
    pub fn parse_files(raw: &str) -> Self {
        if raw.is_empty() {
            return Self { files: None };
        }
        let files: Vec<PathBuf> = raw.split(':').map(PathBuf::from).collect();
        Self { files: Some(files) }
    }
}

/// Function pointer every walk is registered under. Walks are pure
/// functions of their input — no `&mut self`, no I/O coordination beyond
/// reading the filesystem.
pub type WalkFn = fn(&WalkInput) -> Verdict;

/// One entry in the walk registry — a stable name and the function the
/// dispatcher invokes when that name is passed on argv.
#[derive(Debug, Clone, Copy)]
pub struct Walk {
    pub name: &'static str,
    pub run: WalkFn,
}

/// Static registry of every walk this binary dispatches. Alphabetical
/// so the error-message enumeration reads stably.
pub static REGISTRY: &[Walk] = &[
    Walk {
        name: "audit_makes_no_bd_writes_outside_mint_module",
        run: audit_makes_no_bd_writes_outside_mint_module::run,
    },
    Walk {
        name: "crate_structure_includes_loom_tune",
        run: crate_structure::run,
    },
    Walk {
        name: "direct_tools_net_new",
        run: direct_tools_net_new::run,
    },
    Walk {
        name: "event_sink_in_loom_events",
        run: event_sink_in_loom_events::run,
    },
    Walk {
        name: "finding_no_duplicate_definitions",
        run: finding_no_duplicate_definitions::run,
    },
    Walk {
        name: "git_client_encapsulation",
        run: git_client_encapsulation::run,
    },
    Walk {
        name: "loom_agent_deps",
        run: loom_agent_deps::run,
    },
    Walk {
        name: "loom_does_not_invoke_podman",
        run: loom_does_not_invoke_podman::run,
    },
    Walk {
        name: "loom_events_is_leaf",
        run: loom_events_is_leaf::run,
    },
    Walk {
        name: "loom_events_minimal_deps",
        run: loom_events_minimal_deps::run,
    },
    Walk {
        name: "loom_gate_check_derivation_exists",
        run: loom_gate_check_derivation_exists::run,
    },
    Walk {
        name: "loom_llm_client_constructors_use_newtypes",
        run: loom_llm_client_constructors_use_newtypes::run,
    },
    Walk {
        name: "loom_llm_client_types_per_schema_kind",
        run: loom_llm_client_types_per_schema_kind::run,
    },
    Walk {
        name: "loom_llm_deps",
        run: loom_llm_deps::run,
    },
    Walk {
        name: "loom_llm_error_variant_set_multimodal",
        run: loom_llm_error_variant_set_multimodal::run,
    },
    Walk {
        name: "loom_llm_has_no_skill_registry_surface",
        run: loom_llm_has_no_skill_registry_surface::run,
    },
    Walk {
        name: "loom_llm_mime_type_no_raw_strings",
        run: loom_llm_mime_type_no_raw_strings::run,
    },
    Walk {
        name: "loom_llm_multimodal_no_provider_wire_types",
        run: loom_llm_multimodal_no_provider_wire_types::run,
    },
    Walk {
        name: "loom_llm_no_public_genai_types",
        run: loom_llm_no_public_genai_types::run,
    },
    Walk {
        name: "loom_llm_no_underlying_crate_reexports",
        run: loom_llm_no_underlying_crate_reexports::run,
    },
    Walk {
        name: "loom_llm_public_surface",
        run: loom_llm_public_surface::run,
    },
    Walk {
        name: "loom_render_deps",
        run: loom_render_deps::run,
    },
    Walk {
        name: "loom_skills_deps",
        run: loom_skills_deps::run,
    },
    Walk {
        name: "loom_templates_deps",
        run: loom_templates_deps::run,
    },
    Walk {
        name: "loom_templates_public_partial_constants",
        run: loom_templates_public_partial_constants::run,
    },
    Walk {
        name: "loom_templates_public_types",
        run: loom_templates_public_types::run,
    },
    Walk {
        name: "loom_templates_snapshots_no_crate_root_allow",
        run: loom_templates_snapshots_no_crate_root_allow::run,
    },
    Walk {
        name: "loom_templates_workflow_templates_not_exported",
        run: loom_templates_workflow_templates_not_exported::run,
    },
    Walk {
        name: "loom_tune_deps",
        run: loom_tune_deps::run,
    },
    Walk {
        name: "newtype_identifiers",
        run: newtype_identifiers::run,
    },
    Walk {
        name: "nix_flake_check_excludes_workspace_compile",
        run: nix_flake_check_excludes_workspace_compile::run,
    },
    Walk {
        name: "no_allow_dead_code",
        run: no_allow_dead_code::run,
    },
    Walk {
        name: "no_derive_from_on_newtypes",
        run: no_derive_from_on_newtypes::run,
    },
    Walk {
        name: "no_hardcoded_tmp_paths",
        run: no_hardcoded_tmp_paths::run,
    },
    Walk {
        name: "no_inline_suppression_comment_contract",
        run: no_inline_suppression_comment_contract::run,
    },
    Walk {
        name: "no_panics_in_production",
        run: no_panics_in_production::run,
    },
    Walk {
        name: "no_real_clock_outside_system_clock",
        run: no_real_clock_outside_system_clock::run,
    },
    Walk {
        name: "no_thread_sleep",
        run: no_thread_sleep::run,
    },
    Walk {
        name: "no_todo_cursor_meta_key",
        run: no_todo_cursor_meta_key::run,
    },
    Walk {
        name: "no_tokio_sleep_outside_clock",
        run: no_tokio_sleep_outside_clock::run,
    },
    Walk {
        name: "no_tokio_timeout_outside_clock",
        run: no_tokio_timeout_outside_clock::run,
    },
    Walk {
        name: "no_types_or_error_files",
        run: no_types_or_error_files::run,
    },
    Walk {
        name: "observers_in_loom_llm",
        run: observers_in_loom_llm::run,
    },
    Walk {
        name: "phase_verdict_decide_called_from_production",
        run: phase_verdict_decide_called_from_production::run,
    },
    Walk {
        name: "public_contract_crates",
        run: public_contract_crates::run,
    },
    Walk {
        name: "renderer_no_insta_dependency",
        run: renderer_no_insta_dependency::run,
    },
    Walk {
        name: "result_hasher_single_call_site",
        run: result_hasher_single_call_site::run,
    },
    Walk {
        name: "session_trait_does_not_expose_typestate",
        run: session_trait_does_not_expose_typestate::run,
    },
    Walk {
        name: "session_trait_in_loom_events",
        run: session_trait_in_loom_events::run,
    },
    Walk {
        name: "single_event_channel",
        run: single_event_channel::run,
    },
    Walk {
        name: "surface_conformance",
        run: surface_conformance::run,
    },
    Walk {
        name: "template_context_structs",
        run: template_context_structs::run,
    },
    Walk {
        name: "template_pinning_matrix",
        run: template_pinning_matrix::run,
    },
    Walk {
        name: "template_wire_format_restatement",
        run: template_wire_format_restatement::run,
    },
    Walk {
        name: "templates_no_removed_surface",
        run: templates_no_removed_surface::run,
    },
    Walk {
        name: "todo_contexts_carry_criterion_status",
        run: todo_contexts_carry_criterion_status::run,
    },
    Walk {
        name: "todo_template_uses_driver_created_work_epic",
        run: todo_template_uses_driver_created_work_epic::run,
    },
    Walk {
        name: "tune_surface_conformance",
        run: tune_surface_conformance::run,
    },
    Walk {
        name: "workspace_deps_pinned",
        run: workspace_deps_pinned::run,
    },
    Walk {
        name: "workspace_edition",
        run: workspace_edition::run,
    },
    Walk {
        name: "workspace_lints",
        run: workspace_lints::run,
    },
];

/// Look up a walk by name. Returns `None` for any name not in
/// [`REGISTRY`]; callers render the available set themselves via
/// [`names`].
pub fn lookup(name: &str) -> Option<&'static Walk> {
    REGISTRY.iter().find(|w| w.name == name)
}

/// Names of every walk in [`REGISTRY`], in registration order. Empty
/// vector when no walks are registered.
pub fn names() -> Vec<&'static str> {
    REGISTRY.iter().map(|w| w.name).collect()
}

/// Human-readable rendering of the available-walks set used in error
/// messages. `<none>` when the registry is empty so the message stays
/// readable; otherwise comma-separated.
pub fn names_pretty() -> String {
    let names = names();
    if names.is_empty() {
        "<none>".to_string()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn registry_lookup_finds_known_walks() {
        for name in [
            "audit_makes_no_bd_writes_outside_mint_module",
            "crate_structure_includes_loom_tune",
            "direct_tools_net_new",
            "event_sink_in_loom_events",
            "finding_no_duplicate_definitions",
            "git_client_encapsulation",
            "loom_agent_deps",
            "loom_does_not_invoke_podman",
            "loom_events_is_leaf",
            "loom_events_minimal_deps",
            "loom_gate_check_derivation_exists",
            "loom_llm_client_constructors_use_newtypes",
            "loom_llm_client_types_per_schema_kind",
            "loom_llm_deps",
            "loom_llm_error_variant_set_multimodal",
            "loom_llm_has_no_skill_registry_surface",
            "loom_llm_mime_type_no_raw_strings",
            "loom_llm_multimodal_no_provider_wire_types",
            "loom_llm_no_public_genai_types",
            "loom_llm_no_underlying_crate_reexports",
            "loom_llm_public_surface",
            "loom_render_deps",
            "loom_skills_deps",
            "no_derive_from_on_newtypes",
            "no_todo_cursor_meta_key",
            "no_types_or_error_files",
            "no_allow_dead_code",
            "no_panics_in_production",
            "observers_in_loom_llm",
            "phase_verdict_decide_called_from_production",
            "public_contract_crates",
            "result_hasher_single_call_site",
            "session_trait_does_not_expose_typestate",
            "session_trait_in_loom_events",
            "single_event_channel",
            "surface_conformance",
            "newtype_identifiers",
            "nix_flake_check_excludes_workspace_compile",
            "template_context_structs",
            "template_pinning_matrix",
            "template_wire_format_restatement",
            "templates_no_removed_surface",
            "todo_contexts_carry_criterion_status",
            "todo_template_uses_driver_created_work_epic",
            "tune_surface_conformance",
            "loom_templates_deps",
            "loom_templates_public_partial_constants",
            "loom_templates_public_types",
            "loom_templates_snapshots_no_crate_root_allow",
            "loom_templates_workflow_templates_not_exported",
            "loom_tune_deps",
            "no_hardcoded_tmp_paths",
            "no_inline_suppression_comment_contract",
            "no_thread_sleep",
            "no_tokio_sleep_outside_clock",
            "no_tokio_timeout_outside_clock",
            "no_real_clock_outside_system_clock",
            "renderer_no_insta_dependency",
            "workspace_deps_pinned",
            "workspace_edition",
            "workspace_lints",
        ] {
            assert!(lookup(name).is_some(), "missing walk: {name}");
        }
    }

    #[test]
    fn registry_lookup_returns_none_for_unknown() {
        assert!(lookup("definitely_not_a_walk").is_none());
    }

    #[test]
    fn registry_names_is_alphabetical() {
        let names = names();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
    }

    #[test]
    fn registry_names_pretty_lists_walks() {
        let pretty = names_pretty();
        assert!(pretty.contains("no_thread_sleep"), "got: {pretty}");
        assert!(pretty.contains("git_client_encapsulation"), "got: {pretty}");
    }

    #[test]
    fn verdict_round_trips_through_json() {
        let v = Verdict {
            pass: false,
            evidence: "src/foo.rs:12 NO_GIX".into(),
        };
        let s = serde_json::to_string(&v).unwrap();
        let back: Verdict = serde_json::from_str(&s).unwrap();
        assert_eq!(back, v);
        assert_eq!(s, r#"{"pass":false,"evidence":"src/foo.rs:12 NO_GIX"}"#);
    }

    #[test]
    fn parse_files_none_for_empty_input() {
        assert!(WalkInput::parse_files("").files.is_none());
    }

    #[test]
    fn parse_files_splits_on_colon() {
        let input = WalkInput::parse_files("a.rs:b/c.rs:d.rs");
        let files = input.files.unwrap();
        assert_eq!(
            files,
            vec![
                PathBuf::from("a.rs"),
                PathBuf::from("b/c.rs"),
                PathBuf::from("d.rs"),
            ]
        );
    }

    #[test]
    fn parse_files_single_path_no_colon() {
        let input = WalkInput::parse_files("src/lib.rs");
        let files = input.files.unwrap();
        assert_eq!(files, vec![PathBuf::from("src/lib.rs")]);
    }
}

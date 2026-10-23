//! Walk registry — the name → walk function table the binary dispatches
//! against.
//!
//! Each walk is a function in `walk/<name>.rs` that matches [`WalkFn`].
//! [`REGISTRY`] lists them by name; the dispatcher in `main.rs` looks
//! up the named walk, runs it, and emits the [`Verdict`] as a JSON
//! line on stdout per the verifier-runner contract in
//! `specs/gate.md`.

use std::path::{Path, PathBuf};

use inputs::{
    MEMBER_CRATES, crate_manifests, crate_src, crate_structure_inputs, manifest, template_files,
    template_md_files, types_error_inputs,
};
use util::{all_rs_files, all_rs_files_including_verifiers, rs_files_recursive, src_files};

use serde::{Deserialize, Serialize};

pub use inputs::{Error as InputError, render_print_inputs};

mod agent_backend_trait_contract;
mod agent_output_single_decoder;
mod audit_makes_no_bd_writes_outside_mint_module;
mod cli_surface;
mod clock_audit;
mod crate_structure;
mod direct_tools_net_new;
mod event_sink_in_loom_events;
mod finding_no_duplicate_definitions;
mod git_client_encapsulation;
mod inputs;
mod loom_agent_deps;
mod loom_does_not_invoke_podman;
mod loom_events_is_leaf;
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
mod loom_skill_deps;
mod loom_templates_deps;
mod loom_templates_public_partial_constants;
mod loom_templates_public_types;
mod loom_templates_snapshots_no_crate_root_allow;
mod loom_templates_workflow_templates_not_exported;
mod loom_tune_deps;
mod manifest;
mod newtype_identifiers;
mod no_allow_dead_code;
mod no_derive_from_on_newtypes;
mod no_event_sentinels;
mod no_hardcoded_tmp_paths;
mod no_ignore_for_flake;
mod no_inline_suppression_comment_contract;
mod no_panics_in_production;
mod no_real_clock_outside_system_clock;
mod no_thread_sleep;
mod no_tokio_sleep_outside_clock;
mod no_tokio_timeout_outside_clock;
mod no_types_or_error_files;
mod observers_in_loom_llm;
mod phase_verdict_decide_called_from_production;
mod pre_push_config_marker_wrapper_contract;
mod public_contract_crates;
mod purity;
mod renderer_no_insta_dependency;
mod result_hasher_single_call_site;
mod session_trait_does_not_expose_typestate;
mod shared_proptest_config;
mod single_event_channel;
mod source;
mod spec_package_size;
mod surface_conformance;
mod template_pinning_matrix;
mod template_wire_format_restatement;
mod templates_no_removed_surface;
mod test_nix_surface_contract;
mod todo_contexts_carry_criterion_status;
mod todo_template_uses_driver_created_work_epic;
mod tune_surface_conformance;
mod util;
mod workspace_compile_checks_are_full_test_app_only;
mod workspace_deps_pinned;
mod workspace_edition;
mod workspace_lints;

/// Rule verdict serialized by the standalone dispatcher under `specs/gate.md`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub pass: bool,
    pub evidence: String,
}

/// Input passed to every walk. `files` is `Some` when the gate set
/// `LOOM_FILES` (the `--files` scope); the walk filters its own scan
/// against the set. `None` means scan the walk's declared scope in full.
#[derive(Default)]
pub struct WalkInput<'a> {
    pub files: Option<Vec<PathBuf>>,
    source: Option<&'a dyn source::Source>,
    root: Option<PathBuf>,
    errors: std::cell::RefCell<Vec<String>>,
}

impl<'a> WalkInput<'a> {
    /// Execute standalone analysis against an explicit checkout, without changing CWD.
    pub fn for_root(root: &Path) -> Self {
        Self {
            root: Some(root.to_path_buf()),
            ..Self::default()
        }
    }

    fn reader(&self) -> &dyn source::Source {
        self.source.unwrap_or(&source::Disk)
    }

    fn captured(source: &'a dyn source::Source) -> Self {
        Self {
            source: Some(source),
            ..Self::default()
        }
    }

    fn record<T>(&self, path: &Path, result: std::io::Result<T>) -> Option<T> {
        result
            .map_err(|error| {
                self.errors
                    .borrow_mut()
                    .push(format!("{}: {error}", path.display()));
            })
            .ok()
    }

    fn finish(&self, mut verdict: Verdict) -> Verdict {
        let errors = self.errors.take();
        if !errors.is_empty() {
            verdict.pass = false;
            verdict.evidence.push_str("\nsource errors:\n");
            verdict.evidence.push_str(&errors.join("\n"));
        }
        verdict
    }

    /// Build from process env. An unset or empty `LOOM_FILES` resolves to
    /// `files: None` so an unfiltered run scans the walk's full scope; any
    /// non-empty value is split on `:` (the colon-separated convention from
    /// the verifier-runner contract).
    pub fn from_env() -> Self {
        let Ok(raw) = std::env::var("LOOM_FILES") else {
            return Self::default();
        };
        Self::parse_files(&raw)
    }

    /// Parse a raw `LOOM_FILES` value into a `WalkInput`. Empty input
    /// resolves to `files: None` so the empty-string and unset cases agree.
    pub fn parse_files(raw: &str) -> Self {
        if raw.is_empty() {
            return Self::default();
        }
        let files: Vec<PathBuf> = raw.split(':').map(PathBuf::from).collect();
        Self {
            files: Some(files),
            ..Self::default()
        }
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
    pub inputs: fn(&Path, &WalkInput) -> Vec<PathBuf>,
    register: Register,
}

/// Constructor for a current-snapshot tracked definition.
pub type Register = fn(
    &loom_gate::inventory::snapshot::Snapshot,
    &loom_gate::inventory::Inventory,
) -> Result<loom_gate::provider::Definition, loom_gate::provider::Error>;

/// Construct only actual checked definitions; legacy input metadata is not authority.
///
/// # Errors
/// Missing registrations and duplicate requests fail before construction starts.
pub fn checked_definitions(
    snapshot: &loom_gate::inventory::snapshot::Snapshot,
    inventory: &loom_gate::inventory::Inventory,
    names: &[loom_gate::inventory::Name],
) -> Result<Vec<loom_gate::provider::Definition>, loom_gate::provider::Error> {
    let mut seen = std::collections::BTreeSet::new();
    let mut constructors = Vec::new();
    for name in names {
        if !seen.insert(name) {
            return Err(loom_gate::provider::Error::Duplicate {
                target: name.clone(),
            });
        }
        let register = lookup(name.as_str())
            .map(|walk| walk.register)
            .ok_or_else(|| loom_gate::provider::Error::Missing {
                target: name.clone(),
            })?;
        constructors.push(register);
    }
    purity::require(snapshot)?;
    constructors
        .into_iter()
        .map(|register| register(snapshot, inventory))
        .collect()
}

/// Static registry of every walk this binary dispatches. Alphabetical
/// so the error-message enumeration reads stably.
pub static REGISTRY: &[Walk] = &[
    Walk {
        name: "agent_backend_trait_contract",
        run: agent_backend_trait_contract::run,
        inputs: |root, _input| vec![agent_backend_trait_contract::path(root)],
        register: agent_backend_trait_contract::register,
    },
    Walk {
        name: "agent_output_single_decoder",
        run: agent_output_single_decoder::run,
        inputs: |root, input| agent_output_single_decoder::inputs(input, root),
        register: agent_output_single_decoder::register,
    },
    Walk {
        name: "audit_makes_no_bd_writes_outside_mint_module",
        run: audit_makes_no_bd_writes_outside_mint_module::run,
        inputs: |root, input| src_files(input, root),
        register: audit_makes_no_bd_writes_outside_mint_module::register,
    },
    Walk {
        name: "crate_structure_includes_loom_tune",
        run: crate_structure::run,
        inputs: |root, _input| crate_structure_inputs(root),
        register: crate_structure::register,
    },
    Walk {
        name: "direct_tools_net_new",
        run: direct_tools_net_new::run,
        inputs: |root, input| {
            rs_files_recursive(input, &root.join("crates/loom-agent/src/direct/tool"))
        },
        register: direct_tools_net_new::register,
    },
    Walk {
        name: "event_sink_in_loom_events",
        run: event_sink_in_loom_events::run,
        inputs: |root, input| crate_src(input, root, "loom-events"),
        register: event_sink_in_loom_events::register,
    },
    Walk {
        name: "finding_no_duplicate_definitions",
        run: finding_no_duplicate_definitions::run,
        inputs: |root, input| all_rs_files(input, root),
        register: finding_no_duplicate_definitions::register,
    },
    Walk {
        name: "git_client_encapsulation",
        run: git_client_encapsulation::run,
        inputs: |root, input| src_files(input, root),
        register: git_client_encapsulation::register,
    },
    Walk {
        name: loom_agent_deps::NAME,
        run: loom_agent_deps::run,
        inputs: loom_agent_deps::inputs,
        register: loom_agent_deps::register,
    },
    Walk {
        name: "loom_does_not_invoke_podman",
        run: loom_does_not_invoke_podman::run,
        inputs: |root, input| src_files(input, root),
        register: loom_does_not_invoke_podman::register,
    },
    Walk {
        name: loom_events_is_leaf::NAME,
        run: loom_events_is_leaf::run,
        inputs: loom_events_is_leaf::inputs,
        register: loom_events_is_leaf::register,
    },
    Walk {
        name: "loom_llm_client_constructors_use_newtypes",
        run: loom_llm_client_constructors_use_newtypes::run,
        inputs: |root, input| rs_files_recursive(input, &root.join("crates/loom-llm/src/client")),
        register: loom_llm_client_constructors_use_newtypes::register,
    },
    Walk {
        name: "loom_llm_client_types_per_schema_kind",
        run: loom_llm_client_types_per_schema_kind::run,
        inputs: |root, input| rs_files_recursive(input, &root.join("crates/loom-llm/src/client")),
        register: loom_llm_client_types_per_schema_kind::register,
    },
    Walk {
        name: loom_llm_deps::NAME,
        run: loom_llm_deps::run,
        inputs: loom_llm_deps::inputs,
        register: loom_llm_deps::register,
    },
    Walk {
        name: "loom_llm_error_variant_set_multimodal",
        run: loom_llm_error_variant_set_multimodal::run,
        inputs: |root, _input| vec![root.join("crates/loom-llm/src/client/mod.rs")],
        register: loom_llm_error_variant_set_multimodal::register,
    },
    Walk {
        name: "loom_llm_has_no_skill_registry_surface",
        run: loom_llm_has_no_skill_registry_surface::run,
        inputs: |root, input| {
            let mut out = vec![manifest(root, "loom-llm")];
            out.extend(crate_src(input, root, "loom-llm"));
            out
        },
        register: loom_llm_has_no_skill_registry_surface::register,
    },
    Walk {
        name: "loom_llm_mime_type_no_raw_strings",
        run: loom_llm_mime_type_no_raw_strings::run,
        inputs: |root, _input| vec![root.join("crates/loom-llm/src/request.rs")],
        register: loom_llm_mime_type_no_raw_strings::register,
    },
    Walk {
        name: "loom_llm_multimodal_no_provider_wire_types",
        run: loom_llm_multimodal_no_provider_wire_types::run,
        inputs: |root, input| crate_src(input, root, "loom-llm"),
        register: loom_llm_multimodal_no_provider_wire_types::register,
    },
    Walk {
        name: "loom_llm_no_public_genai_types",
        run: loom_llm_no_public_genai_types::run,
        inputs: |root, input| crate_src(input, root, "loom-llm"),
        register: loom_llm_no_public_genai_types::register,
    },
    Walk {
        name: "loom_llm_no_underlying_crate_reexports",
        run: loom_llm_no_underlying_crate_reexports::run,
        inputs: |root, input| crate_src(input, root, "loom-llm"),
        register: loom_llm_no_underlying_crate_reexports::register,
    },
    Walk {
        name: "loom_llm_public_surface",
        run: loom_llm_public_surface::run,
        inputs: |root, input| crate_src(input, root, "loom-llm"),
        register: loom_llm_public_surface::register,
    },
    Walk {
        name: loom_render_deps::NAME,
        run: loom_render_deps::run,
        inputs: loom_render_deps::inputs,
        register: loom_render_deps::register,
    },
    Walk {
        name: loom_skill_deps::NAME,
        run: loom_skill_deps::run,
        inputs: loom_skill_deps::inputs,
        register: loom_skill_deps::register,
    },
    Walk {
        name: loom_templates_deps::NAME,
        run: loom_templates_deps::run,
        inputs: loom_templates_deps::inputs,
        register: loom_templates_deps::register,
    },
    Walk {
        name: "loom_templates_public_partial_constants",
        run: loom_templates_public_partial_constants::run,
        inputs: |root, input| {
            let mut paths = crate_src(input, root, "loom-templates");
            paths.extend(util::files_recursive(
                input,
                &root.join("crates/loom-templates/templates/partial"),
                Some("md"),
            ));
            paths
        },
        register: loom_templates_public_partial_constants::register,
    },
    Walk {
        name: "loom_templates_public_types",
        run: loom_templates_public_types::run,
        inputs: |root, input| crate_src(input, root, "loom-templates"),
        register: loom_templates_public_types::register,
    },
    Walk {
        name: "loom_templates_snapshots_no_crate_root_allow",
        run: loom_templates_snapshots_no_crate_root_allow::run,
        inputs: |root, _input| vec![root.join("crates/loom-templates/tests/snapshots.rs")],
        register: loom_templates_snapshots_no_crate_root_allow::register,
    },
    Walk {
        name: "loom_templates_workflow_templates_not_exported",
        run: loom_templates_workflow_templates_not_exported::run,
        inputs: |root, input| crate_src(input, root, "loom-templates"),
        register: loom_templates_workflow_templates_not_exported::register,
    },
    Walk {
        name: loom_tune_deps::NAME,
        run: loom_tune_deps::run,
        inputs: loom_tune_deps::inputs,
        register: loom_tune_deps::register,
    },
    Walk {
        name: "newtype_identifiers",
        run: newtype_identifiers::run,
        inputs: |root, input| crate_src(input, root, "loom-events"),
        register: newtype_identifiers::register,
    },
    Walk {
        name: "no_allow_dead_code",
        run: no_allow_dead_code::run,
        inputs: |root, input| src_files(input, root),
        register: no_allow_dead_code::register,
    },
    Walk {
        name: "no_derive_from_on_newtypes",
        run: no_derive_from_on_newtypes::run,
        inputs: |root, input| src_files(input, root),
        register: no_derive_from_on_newtypes::register,
    },
    Walk {
        name: "no_event_sentinels",
        run: no_event_sentinels::run,
        inputs: |root, input| src_files(input, root),
        register: no_event_sentinels::register,
    },
    Walk {
        name: "no_hardcoded_tmp_paths",
        run: no_hardcoded_tmp_paths::run,
        inputs: |root, input| all_rs_files(input, root),
        register: no_hardcoded_tmp_paths::register,
    },
    Walk {
        name: "no_ignore_for_flake",
        run: no_ignore_for_flake::run,
        inputs: |root, input| all_rs_files(input, root),
        register: no_ignore_for_flake::register,
    },
    Walk {
        name: "no_inline_suppression_comment_contract",
        run: no_inline_suppression_comment_contract::run,
        inputs: |root, input| no_inline_suppression_comment_contract::contract_scope(input, root),
        register: no_inline_suppression_comment_contract::register,
    },
    Walk {
        name: "no_panics_in_production",
        run: no_panics_in_production::run,
        inputs: |root, input| src_files(input, root),
        register: no_panics_in_production::register,
    },
    Walk {
        name: "no_real_clock_outside_system_clock",
        run: no_real_clock_outside_system_clock::run,
        inputs: |root, input| all_rs_files_including_verifiers(input, root),
        register: no_real_clock_outside_system_clock::register,
    },
    Walk {
        name: "no_thread_sleep",
        run: no_thread_sleep::run,
        inputs: |root, input| all_rs_files_including_verifiers(input, root),
        register: no_thread_sleep::register,
    },
    Walk {
        name: "no_tokio_sleep_outside_clock",
        run: no_tokio_sleep_outside_clock::run,
        inputs: |root, input| all_rs_files_including_verifiers(input, root),
        register: no_tokio_sleep_outside_clock::register,
    },
    Walk {
        name: "no_tokio_timeout_outside_clock",
        run: no_tokio_timeout_outside_clock::run,
        inputs: |root, input| all_rs_files_including_verifiers(input, root),
        register: no_tokio_timeout_outside_clock::register,
    },
    Walk {
        name: "no_types_or_error_files",
        run: no_types_or_error_files::run,
        inputs: |root, input| types_error_inputs(input, root),
        register: no_types_or_error_files::register,
    },
    Walk {
        name: "observers_in_loom_llm",
        run: observers_in_loom_llm::run,
        inputs: |root, input| src_files(input, root),
        register: observers_in_loom_llm::register,
    },
    Walk {
        name: "phase_verdict_decide_called_from_production",
        run: phase_verdict_decide_called_from_production::run,
        inputs: |root, _input| {
            vec![
                root.join("crates/loom-workflow/src/loop/production.rs"),
                root.join("crates/loom-workflow/src/review/production.rs"),
            ]
        },
        register: phase_verdict_decide_called_from_production::register,
    },
    Walk {
        name: "pre_push_config_marker_wrapper_contract",
        run: pre_push_config_marker_wrapper_contract::run,
        inputs: |root, _input| vec![root.join(".pre-commit-config.yaml")],
        register: pre_push_config_marker_wrapper_contract::register,
    },
    Walk {
        name: "public_contract_crates",
        run: public_contract_crates::run,
        inputs: |root, input| crate_manifests(input, root),
        register: public_contract_crates::register,
    },
    Walk {
        name: "renderer_no_insta_dependency",
        run: renderer_no_insta_dependency::run,
        inputs: |root, input| renderer_no_insta_dependency::paths(input, root),
        register: renderer_no_insta_dependency::register,
    },
    Walk {
        name: "result_hasher_single_call_site",
        run: result_hasher_single_call_site::run,
        inputs: |root, input| crate_src(input, root, "loom-llm"),
        register: result_hasher_single_call_site::register,
    },
    Walk {
        name: "session_trait_does_not_expose_typestate",
        run: session_trait_does_not_expose_typestate::run,
        inputs: |root, input| crate_src(input, root, "loom-events"),
        register: session_trait_does_not_expose_typestate::register,
    },
    Walk {
        name: "shared_proptest_config",
        run: shared_proptest_config::run,
        inputs: |root, input| all_rs_files(input, root),
        register: shared_proptest_config::register,
    },
    Walk {
        name: "single_event_channel",
        run: single_event_channel::run,
        inputs: |root, _input| vec![root.join("crates/loom-render/src/sink/mod.rs")],
        register: single_event_channel::register,
    },
    Walk {
        name: "spec_package_size",
        run: spec_package_size::run,
        inputs: |root, input| {
            let mut paths = vec![root.join("docs/README.md")];
            paths.extend(util::files_recursive(input, &root.join("specs"), None));
            paths
        },
        register: spec_package_size::register,
    },
    Walk {
        name: "surface_conformance",
        run: surface_conformance::run,
        inputs: |root, _input| {
            vec![
                root.join("specs/harness.md"),
                root.join("specs/events.md"),
                root.join("specs/inbox.md"),
                root.join("crates/loom/src/main.rs"),
            ]
        },
        register: surface_conformance::register,
    },
    Walk {
        name: "template_pinning_matrix",
        run: template_pinning_matrix::run,
        inputs: |root, input| {
            let mut out = vec![root.join("specs/templates.md")];
            out.extend(template_files(input, root));
            out
        },
        register: template_pinning_matrix::register,
    },
    Walk {
        name: "template_wire_format_restatement",
        run: template_wire_format_restatement::run,
        inputs: |root, input| template_files(input, root),
        register: template_wire_format_restatement::register,
    },
    Walk {
        name: "templates_no_removed_surface",
        run: templates_no_removed_surface::run,
        inputs: |root, input| template_md_files(input, root),
        register: templates_no_removed_surface::register,
    },
    Walk {
        name: "test_nix_surface_contract",
        run: test_nix_surface_contract::run,
        inputs: |root, _input| {
            vec![
                root.join("flake.nix"),
                root.join("nix/flake/apps.nix"),
                root.join("nix/flake/checks.nix"),
                root.join("nix/flake/lib.nix"),
                root.join("nix/flake/tests.nix"),
                root.join("nix/nextest-overlay.nix"),
                root.join("scripts/full-test.sh"),
                root.join("scripts/required-test.sh"),
                root.join("tests/default.nix"),
                root.join("tests/loom/default.nix"),
                root.join("tests/run-tests.sh"),
                root.join("tests/smoke/loop.sh"),
            ]
        },
        register: test_nix_surface_contract::register,
    },
    Walk {
        name: "todo_contexts_carry_criterion_status",
        run: todo_contexts_carry_criterion_status::run,
        inputs: |root, input| crate_src(input, root, "loom-templates"),
        register: todo_contexts_carry_criterion_status::register,
    },
    Walk {
        name: "todo_template_uses_driver_created_work_epic",
        run: todo_template_uses_driver_created_work_epic::run,
        inputs: |root, _input| vec![root.join("crates/loom-templates/templates/todo.md")],
        register: todo_template_uses_driver_created_work_epic::register,
    },
    Walk {
        name: "tune_surface_conformance",
        run: tune_surface_conformance::run,
        inputs: |root, _input| {
            vec![
                root.join("specs/tuning.md"),
                root.join("crates/loom/src/main.rs"),
            ]
        },
        register: tune_surface_conformance::register,
    },
    Walk {
        name: "workspace_compile_checks_are_full_test_app_only",
        run: workspace_compile_checks_are_full_test_app_only::run,
        inputs: |root, _input| {
            vec![
                root.join("nix/flake/apps.nix"),
                root.join("nix/flake/checks.nix"),
                root.join("nix/workspace.nix"),
                root.join("scripts/full-test.sh"),
            ]
        },
        register: workspace_compile_checks_are_full_test_app_only::register,
    },
    Walk {
        name: "workspace_deps_pinned",
        run: workspace_deps_pinned::run,
        inputs: |root, _input| vec![root.join("Cargo.toml")],
        register: workspace_deps_pinned::register,
    },
    Walk {
        name: "workspace_edition",
        run: workspace_edition::run,
        inputs: |root, _input| {
            let mut out = vec![root.join("Cargo.toml")];
            out.extend(MEMBER_CRATES.iter().map(|c| manifest(root, c)));
            out
        },
        register: workspace_edition::register,
    },
    Walk {
        name: "workspace_lints",
        run: workspace_lints::run,
        inputs: |root, _input| {
            let mut out = vec![root.join("Cargo.toml")];
            out.extend(MEMBER_CRATES.iter().map(|c| manifest(root, c)));
            out
        },
        register: workspace_lints::register,
    },
];

/// Check candidate template sources with the same rules as the standalone verifier.
pub fn template_conformance(input: &WalkInput<'_>) -> [Verdict; 3] {
    [
        template_pinning_matrix::run(input),
        template_wire_format_restatement::run(input),
        templates_no_removed_surface::run(input),
    ]
}

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
    fn embedded_template_conformance_matches_registered_execution() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let input = WalkInput::for_root(&root);
        let expected = [
            "template_pinning_matrix",
            "template_wire_format_restatement",
            "templates_no_removed_surface",
        ]
        .map(|name| (lookup(name).unwrap().run)(&input));
        assert!(expected.iter().all(|verdict| verdict.pass), "{expected:?}");
        assert_eq!(template_conformance(&input), expected);
    }

    #[test]
    fn registry_lookup_finds_known_walks() {
        for name in [
            "agent_backend_trait_contract",
            "audit_makes_no_bd_writes_outside_mint_module",
            "crate_structure_includes_loom_tune",
            "direct_tools_net_new",
            "event_sink_in_loom_events",
            "finding_no_duplicate_definitions",
            "git_client_encapsulation",
            "loom_agent_deps",
            "loom_does_not_invoke_podman",
            "loom_events_is_leaf",
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
            "loom_skill_deps",
            "no_derive_from_on_newtypes",
            "no_types_or_error_files",
            "no_allow_dead_code",
            "no_panics_in_production",
            "observers_in_loom_llm",
            "phase_verdict_decide_called_from_production",
            "pre_push_config_marker_wrapper_contract",
            "public_contract_crates",
            "result_hasher_single_call_site",
            "session_trait_does_not_expose_typestate",
            "single_event_channel",
            "surface_conformance",
            "test_nix_surface_contract",
            "newtype_identifiers",
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
            "workspace_compile_checks_are_full_test_app_only",
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

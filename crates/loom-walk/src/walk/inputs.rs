//! `--print-inputs` responder — per-walk scanned file-set declaration.
//!
//! The gate's `builtin-loom-walk` runner declares an `inputs` query
//! (`cargo run -p loom-walk -- {targets} --print-inputs`); this module is
//! the answering side. For each named walk, [`inputs_for`] returns the set
//! of files the walk reads when it runs, so the gate can scope the walk
//! under `loom gate verify --files` (skip it when the change touches none
//! of its inputs) and the integrity gate can hold the walk to the
//! inputs-protocol contract.
//!
//! Each arm is derived from the matching `walk/<name>.rs` scan logic, not
//! blanket-declared: a walk that reads a `Cargo.toml`, a template tree, or
//! a single named file reports exactly those, never the generic
//! `crates/*/src/**` set. Over-declaration is safe (the walk merely always
//! runs); under-declaration would skip a walk whose input changed, so where
//! a scope is a strict subset of a `util` helper the helper is reported as
//! the conservative superset.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use walkdir::WalkDir;

use super::util::{
    all_rs_files, immediate_children, rel, rs_files_recursive, src_files, workspace_root,
};

/// Single-walk `--print-inputs` document: `{"inputs": ["path", ...]}`. The
/// gate's `parse_inputs_json` reads this form when one walk is queried.
#[derive(Serialize)]
struct SingleDoc {
    inputs: Vec<String>,
}

/// Batch `--print-inputs` document: `{"inputs": {"<walk>": ["path", ...]}}`.
/// The gate's `parse_inputs_batch_json` maps each walk-name key back to its
/// annotation, so a one-spawn query answers for the whole matched group.
#[derive(Serialize)]
struct BatchDoc {
    inputs: BTreeMap<String, Vec<String>>,
}

/// Render the `--print-inputs` response for `names`. One name emits the
/// single-array form; several emit the batch map keyed by walk name (the
/// runner's `{capture_1}` rendered target). Paths are repo-relative and
/// deduplicated so they compare equal to the gate's `--files` scope.
pub fn render_print_inputs(names: &[String]) -> Result<String, serde_json::Error> {
    let root = workspace_root();
    if let [only] = names {
        serde_json::to_string(&SingleDoc {
            inputs: rel_inputs(only, &root),
        })
    } else {
        let inputs = names
            .iter()
            .map(|name| (name.clone(), rel_inputs(name, &root)))
            .collect();
        serde_json::to_string(&BatchDoc { inputs })
    }
}

/// One walk's scanned set as sorted, deduplicated repo-relative strings.
fn rel_inputs(name: &str, root: &Path) -> Vec<String> {
    let mut out: Vec<String> = inputs_for(name, root)
        .iter()
        .map(|p| rel(root, p))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The ten target v1 workspace member crates whose manifests the
/// `workspace_edition` / `workspace_lints` walks inherit-check, mirroring
/// `crate_structure_includes_loom_tune`'s binary + library crate set.
const MEMBER_CRATES: &[&str] = &[
    "loom",
    "loom-events",
    "loom-llm",
    "loom-templates",
    "loom-skills",
    "loom-tune",
    "loom-driver",
    "loom-render",
    "loom-agent",
    "loom-workflow",
];

/// Files this walk scans, as absolute paths under `root`. An unknown name
/// falls back to [`src_files`] — the conservative always-run superset — so
/// a freshly registered walk never silently declares an empty input set.
pub fn inputs_for(name: &str, root: &Path) -> Vec<PathBuf> {
    match name {
        // Whole-tree production-source scans: `narrow_to_loom_files(src_files(..))`.
        "audit_makes_no_bd_writes_outside_mint_module"
        | "git_client_encapsulation"
        | "loom_does_not_invoke_podman"
        | "no_allow_dead_code"
        | "no_derive_from_on_newtypes"
        | "no_inline_suppression_comment_contract"
        | "no_panics_in_production"
        | "no_real_clock_outside_system_clock"
        | "no_thread_sleep"
        | "no_tokio_sleep_outside_clock"
        | "no_tokio_timeout_outside_clock"
        | "observers_in_loom_llm" => src_files(root),

        // Production source + tests: `narrow_to_loom_files(all_rs_files(..))`.
        "finding_no_duplicate_definitions" | "no_hardcoded_tmp_paths" => all_rs_files(root),

        // Single-crate recursive `src/` scans.
        "loom_llm_multimodal_no_provider_wire_types"
        | "loom_llm_no_public_genai_types"
        | "loom_llm_no_underlying_crate_reexports"
        | "loom_llm_public_surface"
        | "result_hasher_single_call_site" => crate_src(root, "loom-llm"),
        "loom_llm_client_constructors_use_newtypes" | "loom_llm_client_types_per_schema_kind" => {
            rs_files_recursive(&root.join("crates/loom-llm/src/client"))
        }
        "loom_llm_has_no_skill_registry_surface" => {
            let mut out = vec![manifest(root, "loom-llm")];
            out.extend(crate_src(root, "loom-llm"));
            out
        }
        "loom_templates_public_partial_constants"
        | "loom_templates_public_types"
        | "loom_templates_workflow_templates_not_exported"
        | "todo_contexts_carry_criterion_status" => crate_src(root, "loom-templates"),
        "session_trait_does_not_expose_typestate" => crate_src(root, "loom-events"),
        "direct_tools_net_new" => {
            rs_files_recursive(&root.join("crates/loom-agent/src/direct/tools"))
        }
        "event_sink_in_loom_events" => crate_src(root, "loom-events"),
        // Scans `crates/loom-driver/src/identifier/`, which may not exist
        // yet; declare the host crate's `src` so the walk is never skipped
        // and re-triggers once the identifier module lands.
        "newtype_identifiers" => crate_src(root, "loom-driver"),

        // Single named source files.
        "loom_llm_error_variant_set_multimodal" => {
            vec![root.join("crates/loom-llm/src/client/mod.rs")]
        }
        "loom_llm_mime_type_no_raw_strings" => {
            vec![root.join("crates/loom-llm/src/request.rs")]
        }
        "no_sync_or_tune_command" => vec![root.join("crates/loom/src/main.rs")],
        "no_todo_cursor_meta_key" => vec![root.join("crates/loom-driver/src/state/db.rs")],
        "single_event_channel" => vec![root.join("crates/loom-render/src/sink/mod.rs")],
        "loom_templates_snapshots_no_crate_root_allow" => {
            vec![root.join("crates/loom-templates/tests/snapshots.rs")]
        }
        "loom_gate_check_derivation_exists" | "nix_flake_check_excludes_workspace_compile" => {
            vec![root.join("nix/flake/checks.nix")]
        }
        "surface_conformance" => vec![
            root.join("specs/harness.md"),
            root.join("crates/loom/src/main.rs"),
        ],
        "phase_verdict_decide_called_from_production" => vec![
            root.join("crates/loom-workflow/src/loop/production.rs"),
            root.join("crates/loom-workflow/src/review/production.rs"),
        ],
        "session_trait_in_loom_events" => {
            let mut out = vec![root.join("crates/loom-events/src/lib.rs")];
            out.extend(rs_files_recursive(&root.join("crates/loom-driver/src")));
            out
        }

        // Manifest-only scans.
        "loom_agent_deps" => vec![manifest(root, "loom-agent")],
        "loom_llm_deps" => vec![manifest(root, "loom-llm")],
        "loom_render_deps" => vec![manifest(root, "loom-render")],
        "loom_skills_deps" => vec![manifest(root, "loom-skills")],
        "loom_templates_deps" => vec![manifest(root, "loom-templates")],
        "loom_tune_deps" => vec![manifest(root, "loom-tune")],
        "loom_events_is_leaf" | "loom_events_minimal_deps" => vec![manifest(root, "loom-events")],
        "public_contract_crates" => ["loom-events", "loom-llm", "loom-templates", "loom-skills"]
            .iter()
            .map(|c| manifest(root, c))
            .collect(),
        "workspace_deps_pinned" => vec![root.join("Cargo.toml")],
        "workspace_edition" | "workspace_lints" => {
            let mut out = vec![root.join("Cargo.toml")];
            out.extend(MEMBER_CRATES.iter().map(|c| manifest(root, c)));
            out
        }

        // Crate-structure sentinels: each member crate's manifest + entry.
        "crate_structure_includes_loom_tune" => crate_structure_inputs(root),
        // RS-5: forbidden central `types.rs` / `error.rs` at any crate-src root.
        "no_types_or_error_files" => types_error_inputs(root),

        // Renderer crate: `Cargo.toml` + every `src/**` Rust file.
        "renderer_no_insta_dependency" => {
            let mut out = vec![manifest(root, "loom-render")];
            out.extend(crate_src(root, "loom-render"));
            out
        }

        // Template-tree scans.
        "template_wire_format_restatement" => template_files(root),
        "templates_no_removed_surface" => template_md_files(root),
        "todo_template_uses_driver_created_work_epic" => {
            vec![root.join("crates/loom-templates/templates/todo.md")]
        }
        "template_context_structs" => {
            let mut out = crate_src(root, "loom-templates");
            out.extend(template_md_files(root));
            out
        }
        "template_pinning_matrix" => {
            let mut out = vec![root.join("specs/templates.md")];
            out.extend(template_files(root));
            out
        }

        // Unknown / newly added walk: conservative always-run superset.
        _ => src_files(root),
    }
}

/// A member crate's recursive `src/` tree.
fn crate_src(root: &Path, krate: &str) -> Vec<PathBuf> {
    rs_files_recursive(&root.join(format!("crates/{krate}/src")))
}

/// A member crate's manifest path.
fn manifest(root: &Path, krate: &str) -> PathBuf {
    root.join(format!("crates/{krate}/Cargo.toml"))
}

/// Every file (any extension) under the loom-templates template tree.
fn template_files(root: &Path) -> Vec<PathBuf> {
    files_under(&root.join("crates/loom-templates/templates"), None)
}

/// Every `.md` file under the loom-templates template tree.
fn template_md_files(root: &Path) -> Vec<PathBuf> {
    files_under(&root.join("crates/loom-templates/templates"), Some("md"))
}

/// Recursive file collection under `dir`, optionally filtered to one
/// extension. Mirrors the `WalkDir` scopes the template walks build inline.
fn files_under(dir: &Path, ext: Option<&str>) -> Vec<PathBuf> {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .filter(|p| match ext {
            Some(want) => p.extension().and_then(|s| s.to_str()) == Some(want),
            None => true,
        })
        .collect()
}

/// `crate_structure_includes_loom_tune`'s scope: each member crate's
/// `Cargo.toml` plus its entry source (`src/main.rs` for the binary,
/// `src/lib.rs` for libraries).
fn crate_structure_inputs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for krate in MEMBER_CRATES {
        out.push(manifest(root, krate));
        let entry = if *krate == "loom" {
            "src/main.rs"
        } else {
            "src/lib.rs"
        };
        out.push(root.join(format!("crates/{krate}/{entry}")));
    }
    out
}

/// `no_types_or_error_files`'s scope: the forbidden `src/types.rs` and
/// `src/error.rs` of every crate, declared by path whether or not they
/// exist on disk so adding one re-triggers the walk.
fn types_error_inputs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for crate_dir in immediate_children(&root.join("crates")) {
        out.push(crate_dir.join("src/types.rs"));
        out.push(crate_dir.join("src/error.rs"));
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::walk::names;

    /// The contract behind "0 `declares no inputs` lines" for the ~77
    /// loom-walk `[check]` verifiers: every registered walk answers
    /// `--print-inputs` with a non-empty set, so the gate never falls back
    /// to the conservative always-run default for a walk loom owns.
    #[test]
    fn every_registered_walk_declares_nonempty_inputs() {
        let root = workspace_root();
        for name in names() {
            let paths = inputs_for(name, &root);
            assert!(
                !paths.is_empty(),
                "walk `{name}` declared an empty input set",
            );
        }
    }

    /// Declared paths must be repo-relative so they compare equal to the
    /// gate's `--files` scope (which carries repo-relative paths).
    #[test]
    fn declared_inputs_are_repo_relative() {
        let root = workspace_root();
        for name in names() {
            for p in inputs_for(name, &root) {
                assert!(
                    p.starts_with(&root) || p.is_relative(),
                    "walk `{name}` declared non-root-relative path {p:?}",
                );
                let rel = rel(&root, &p);
                assert!(
                    !rel.starts_with('/'),
                    "walk `{name}` rel path is absolute: {rel}",
                );
            }
        }
    }

    /// A manifest-only walk reports exactly its one `Cargo.toml`, never the
    /// generic `crates/*/src/**` set — the "do not blanket-declare" rule.
    #[test]
    fn manifest_walk_reports_only_its_manifest() {
        let root = workspace_root();
        assert_eq!(
            inputs_for("loom_llm_deps", &root),
            vec![manifest(&root, "loom-llm")]
        );
    }

    /// A walk reading named spec + source files reports exactly those.
    #[test]
    fn named_file_walk_reports_those_files() {
        let root = workspace_root();
        let got = rel_inputs("surface_conformance", &root);
        assert_eq!(
            got,
            vec![
                "crates/loom/src/main.rs".to_string(),
                "specs/harness.md".to_string(),
            ],
        );
    }

    /// One name emits the array form; the gate's `parse_inputs_json` reads it.
    #[test]
    fn single_name_emits_array_document() {
        let doc = render_print_inputs(&["loom_llm_deps".to_string()]).unwrap();
        assert_eq!(doc, r#"{"inputs":["crates/loom-llm/Cargo.toml"]}"#);
    }

    /// Several names emit the batch map keyed by walk name; the gate's
    /// `parse_inputs_batch_json` maps each key back to its annotation.
    #[test]
    fn multiple_names_emit_batch_document() {
        let doc =
            render_print_inputs(&["loom_llm_deps".to_string(), "loom_render_deps".to_string()])
                .unwrap();
        assert_eq!(
            doc,
            r#"{"inputs":{"loom_llm_deps":["crates/loom-llm/Cargo.toml"],"loom_render_deps":["crates/loom-render/Cargo.toml"]}}"#,
        );
    }
}

//! Input-query rendering for the shared walk registry.

use std::collections::{BTreeMap, BTreeSet};

use displaydoc::Display;
use std::path::{Path, PathBuf};
use thiserror::Error;

use serde::Serialize;

use super::WalkInput;
use super::util::{immediate_children, rel, rs_files_recursive, workspace_root};

/// Invalid input queries cannot produce a partial or conservative response.
#[derive(Debug, Display, Error)]
pub enum Error {
    /// input query contains no walk names
    EmptyQuery,
    /// unknown walk `{name}`
    UnknownWalk { name: String },
    /// repeated walk `{name}`
    RepeatedWalk { name: String },
    /// input discovery failed: {detail}
    Source { detail: String },
    /// failed to serialise walk inputs
    Serialise(#[from] serde_json::Error),
}

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

/// Render single or batched input metadata with sorted repository-relative paths.
///
/// # Errors
/// Rejects empty, unknown or repeated requests and serialization failures.
pub fn render_print_inputs(names: &[String]) -> Result<String, Error> {
    if names.is_empty() {
        return Err(Error::EmptyQuery);
    }
    let mut seen = BTreeSet::new();
    for name in names {
        if super::lookup(name).is_none() {
            return Err(Error::UnknownWalk { name: name.clone() });
        }
        if !seen.insert(name) {
            return Err(Error::RepeatedWalk { name: name.clone() });
        }
    }
    let input = WalkInput::default();
    let root = workspace_root(&input);
    let errors = input.errors.take();
    if !errors.is_empty() {
        return Err(Error::Source {
            detail: errors.join("\n"),
        });
    }
    if let [only] = names {
        Ok(serde_json::to_string(&SingleDoc {
            inputs: rel_inputs(only, &root)?,
        })?)
    } else {
        let inputs = names
            .iter()
            .map(|name| Ok((name.clone(), rel_inputs(name, &root)?)))
            .collect::<Result<_, Error>>()?;
        Ok(serde_json::to_string(&BatchDoc { inputs })?)
    }
}

/// One walk's scanned set as sorted, deduplicated repo-relative strings.
fn rel_inputs(name: &str, root: &Path) -> Result<Vec<String>, Error> {
    let mut out: Vec<String> = inputs_for(name, root)?
        .iter()
        .map(|p| rel(root, p))
        .collect();
    out.sort();
    out.dedup();
    Ok(out)
}

/// The target v1 workspace member crates whose manifests the
/// `workspace_edition` / `workspace_lints` walks inherit-check, mirroring
/// `crate_structure_includes_loom_tune`'s binary + library crate set.
pub(super) const MEMBER_CRATES: &[&str] = &[
    "loom",
    "loom-driver",
    "loom-issues",
    "loom-shared",
    "loom-events",
    "loom-llm",
    "loom-skill",
    "loom-tune",
    "loom-render",
    "loom-agent",
    "loom-direct-runner",
    "loom-gate",
    "loom-protocol",
    "loom-workflow",
    "loom-templates",
    "loom-test-support",
    "loom-walk",
];

/// Obtain input metadata from the same definition used for dispatch.
pub fn inputs_for(name: &str, root: &Path) -> Result<Vec<PathBuf>, Error> {
    let definition = super::lookup(name).ok_or_else(|| Error::UnknownWalk {
        name: name.to_owned(),
    })?;
    let input = WalkInput::default();
    let paths = (definition.inputs)(root, &input);
    let errors = input.errors.take();
    if errors.is_empty() {
        Ok(paths)
    } else {
        Err(Error::Source {
            detail: errors.join("\n"),
        })
    }
}

/// A member crate's recursive `src/` tree.
pub(super) fn crate_src(input: &WalkInput, root: &Path, krate: &str) -> Vec<PathBuf> {
    rs_files_recursive(input, &root.join(format!("crates/{krate}/src")))
}

/// A member crate's manifest path.
pub(super) fn manifest(root: &Path, krate: &str) -> PathBuf {
    root.join(format!("crates/{krate}/Cargo.toml"))
}

/// Every file (any extension) under the loom-templates template tree.
pub(super) fn template_files(input: &WalkInput, root: &Path) -> Vec<PathBuf> {
    files_under(input, &root.join("crates/loom-templates/templates"), None)
}

/// Every `.md` file under the loom-templates template tree.
pub(super) fn template_md_files(input: &WalkInput, root: &Path) -> Vec<PathBuf> {
    files_under(
        input,
        &root.join("crates/loom-templates/templates"),
        Some("md"),
    )
}

/// Recursive file collection under `dir`, optionally filtered to one
/// extension. Mirrors the `WalkDir` scopes the template walks build inline.
fn files_under(input: &WalkInput, dir: &Path, ext: Option<&str>) -> Vec<PathBuf> {
    super::util::files_recursive(input, dir, ext)
}

pub(super) fn crate_manifests(input: &WalkInput, root: &Path) -> Vec<PathBuf> {
    immediate_children(input, &root.join("crates"))
        .into_iter()
        .map(|crate_dir| crate_dir.join("Cargo.toml"))
        .collect()
}

/// `crate_structure_includes_loom_tune`'s scope: the root manifest,
/// each member crate's `Cargo.toml`, and each canonical entry source.
pub(super) fn crate_structure_inputs(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.join("Cargo.toml")];
    for krate in MEMBER_CRATES {
        out.push(manifest(root, krate));
        for entry in crate_entries(krate) {
            out.push(root.join(format!("crates/{krate}/{entry}")));
        }
    }
    out
}

fn crate_entries(krate: &str) -> &'static [&'static str] {
    match krate {
        "loom" | "loom-walk" => &["src/main.rs"],
        "loom-direct-runner" => &["src/lib.rs", "src/main.rs"],
        _ => &["src/lib.rs"],
    }
}

/// `no_types_or_error_files`'s scope: the forbidden `src/types.rs` and
/// `src/error.rs` of every crate, declared by path whether or not they
/// exist on disk so adding one re-triggers the walk.
pub(super) fn types_error_inputs(input: &WalkInput, root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for crate_dir in immediate_children(input, &root.join("crates")) {
        // The native package definition binds this absence-only namespace scope.
        out.push(crate_dir.join("Cargo.toml"));
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
        let root = workspace_root(&WalkInput::default());
        for name in names() {
            let paths = inputs_for(name, &root).unwrap();
            assert!(
                !paths.is_empty(),
                "walk `{name}` declared an empty input set",
            );
        }
    }

    #[test]
    fn unknown_input_query_never_becomes_a_source_tree_fallback() {
        let root = workspace_root(&WalkInput::default());
        assert!(matches!(
            inputs_for("not_registered", &root),
            Err(Error::UnknownWalk { name }) if name == "not_registered"
        ));
    }

    #[test]
    fn mixed_input_query_rejects_unknown_sibling() {
        assert!(matches!(
            render_print_inputs(&["loom_llm_deps".into(), "not_registered".into()]),
            Err(Error::UnknownWalk { name }) if name == "not_registered"
        ));
    }

    #[test]
    fn repeated_input_query_is_not_silently_collapsed() {
        assert!(matches!(
            render_print_inputs(&["loom_llm_deps".into(), "loom_llm_deps".into()]),
            Err(Error::RepeatedWalk { name }) if name == "loom_llm_deps"
        ));
    }

    #[test]
    fn empty_input_query_is_rejected() {
        assert!(matches!(render_print_inputs(&[]), Err(Error::EmptyQuery)));
    }

    /// Declared paths must be repo-relative so they compare equal to the
    /// gate's `--files` scope (which carries repo-relative paths).
    #[test]
    fn declared_inputs_are_repo_relative() {
        let root = workspace_root(&WalkInput::default());
        for name in names() {
            for p in inputs_for(name, &root).unwrap() {
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
        let root = workspace_root(&WalkInput::default());
        assert_eq!(
            inputs_for("loom_llm_deps", &root).unwrap(),
            vec![manifest(&root, "loom-llm")]
        );
    }

    /// A walk reading the pre-push hook config reports exactly that file.
    #[test]
    fn pre_push_config_walk_reports_pre_commit_config() {
        let root = workspace_root(&WalkInput::default());
        assert_eq!(
            rel_inputs("pre_push_config_marker_wrapper_contract", &root).unwrap(),
            vec![".pre-commit-config.yaml".to_string()],
        );
    }

    /// A walk reading named spec + source files reports exactly those.
    #[test]
    fn named_file_walk_reports_those_files() {
        let root = workspace_root(&WalkInput::default());
        let got = rel_inputs("surface_conformance", &root).unwrap();
        assert_eq!(
            got,
            vec![
                "crates/loom/src/main.rs".to_string(),
                "specs/events.md".to_string(),
                "specs/harness.md".to_string(),
                "specs/inbox.md".to_string(),
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

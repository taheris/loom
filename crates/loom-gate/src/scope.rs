//! `cargo metadata`-backed [`TestScope`] for `--files` scope filtering.
//!
//! Construction shells out to `cargo metadata`, parses the workspace's
//! package graph, and precomputes the per-crate scope: the source files of
//! each workspace package unioned with the source files of every package
//! in its transitive dependency closure. [`TestScope::scope_for`] then
//! collapses to a hash lookup on the annotation's first `::`-delimited
//! segment.
//!
//! Emitted paths are workspace-relative so they intersect cleanly against
//! the `--files` paths the gate's CLI surface receives (pre-commit hooks
//! pass paths relative to the repo root). External, non-workspace
//! dependencies (registry crates) are intentionally excluded — pre-commit
//! `--files` inputs never name registry paths, so unioning those files in
//! adds cost without expanding the intersection result.
//!
//! The annotation's first segment is treated as the consumer's crate name
//! (cargo convention: package name with `-` substituted to `_`). The
//! placeholder `crate::` prefix used in inline test fixtures cannot
//! disambiguate which workspace package owns the test and falls through to
//! an empty scope — production annotations must use real crate names.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use displaydoc::Display;
use serde::Deserialize;
use thiserror::Error;
use walkdir::WalkDir;

use crate::annotation::Annotation;
use crate::dispatch::TestScope;

/// Production [`TestScope`] backed by `cargo metadata`.
pub struct CargoMetadataScope {
    crate_scope: HashMap<String, Vec<PathBuf>>,
}

/// Failures surfaced while building a [`CargoMetadataScope`].
#[derive(Debug, Display, Error)]
pub enum ScopeError {
    /// failed to spawn `cargo metadata` for manifest `{manifest}`: {source}
    SpawnCargo {
        manifest: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// `cargo metadata` exited non-zero for manifest `{manifest}`: {stderr}
    CargoMetadataFailed { manifest: PathBuf, stderr: String },
    /// failed to parse `cargo metadata` JSON: {source}
    ParseMetadata {
        #[source]
        source: serde_json::Error,
    },
    /// failed to walk package sources under `{root}`: {source}
    WalkSources {
        root: PathBuf,
        #[source]
        source: walkdir::Error,
    },
}

impl CargoMetadataScope {
    /// Build a scope by invoking `cargo metadata` against the workspace
    /// whose root manifest is `manifest_path`. The current `cargo` on
    /// PATH is used; consumers needing toolchain pinning are expected to
    /// set up `PATH` accordingly before calling.
    pub fn from_manifest(manifest_path: &Path) -> Result<Self, ScopeError> {
        let stdout = run_cargo_metadata(manifest_path)?;
        let metadata: CargoMetadataJson = serde_json::from_slice(&stdout)
            .map_err(|source| ScopeError::ParseMetadata { source })?;
        Self::from_metadata(metadata)
    }

    fn from_metadata(metadata: CargoMetadataJson) -> Result<Self, ScopeError> {
        let CargoMetadataJson {
            packages,
            workspace_members,
            workspace_root,
            resolve,
        } = metadata;

        let pkg_by_id: HashMap<String, MetaPackage> =
            packages.into_iter().map(|p| (p.id.clone(), p)).collect();
        let deps_by_id: HashMap<String, Vec<String>> = resolve
            .nodes
            .into_iter()
            .map(|n| (n.id, n.dependencies))
            .collect();
        let ws_members: HashSet<String> = workspace_members.into_iter().collect();

        let mut pkg_files: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for ws_id in &ws_members {
            let Some(pkg) = pkg_by_id.get(ws_id) else {
                continue;
            };
            let pkg_dir = pkg
                .manifest_path
                .parent()
                .unwrap_or(&pkg.manifest_path)
                .to_path_buf();
            let files = walk_rs_files(&pkg_dir, &workspace_root)?;
            pkg_files.insert(ws_id.clone(), files);
        }

        let mut crate_scope: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for ws_id in &ws_members {
            let Some(pkg) = pkg_by_id.get(ws_id) else {
                continue;
            };
            let closure = transitive_closure(ws_id, &deps_by_id);
            let mut files: BTreeSet<PathBuf> = BTreeSet::new();
            for pid in &closure {
                if let Some(fs) = pkg_files.get(pid) {
                    files.extend(fs.iter().cloned());
                }
            }
            let files_vec: Vec<PathBuf> = files.into_iter().collect();
            for key in crate_keys_for(pkg) {
                crate_scope.entry(key).or_insert_with(|| files_vec.clone());
            }
        }

        Ok(Self { crate_scope })
    }
}

impl TestScope for CargoMetadataScope {
    fn scope_for(&self, annotation: &Annotation) -> Vec<PathBuf> {
        let Some(key) = crate_key_from_target(&annotation.target) else {
            return Vec::new();
        };
        self.crate_scope.get(&key).cloned().unwrap_or_default()
    }
}

fn run_cargo_metadata(manifest_path: &Path) -> Result<Vec<u8>, ScopeError> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version=1"])
        .arg("--manifest-path")
        .arg(manifest_path)
        .output()
        .map_err(|source| ScopeError::SpawnCargo {
            manifest: manifest_path.to_path_buf(),
            source,
        })?;
    if !output.status.success() {
        return Err(ScopeError::CargoMetadataFailed {
            manifest: manifest_path.to_path_buf(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(output.stdout)
}

fn walk_rs_files(dir: &Path, workspace_root: &Path) -> Result<Vec<PathBuf>, ScopeError> {
    let mut out: Vec<PathBuf> = Vec::new();
    for entry in WalkDir::new(dir).follow_links(false) {
        let entry = entry.map_err(|source| ScopeError::WalkSources {
            root: dir.to_path_buf(),
            source,
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        if path
            .components()
            .any(|c| c.as_os_str() == OsStr::new("target"))
        {
            continue;
        }
        let rel = path.strip_prefix(workspace_root).unwrap_or(path);
        out.push(rel.to_path_buf());
    }
    out.sort();
    Ok(out)
}

fn transitive_closure(start: &str, deps: &HashMap<String, Vec<String>>) -> HashSet<String> {
    let mut out: HashSet<String> = HashSet::new();
    let mut frontier: VecDeque<String> = VecDeque::new();
    out.insert(start.to_string());
    frontier.push_back(start.to_string());
    while let Some(id) = frontier.pop_front() {
        let Some(ds) = deps.get(&id) else {
            continue;
        };
        for d in ds {
            if out.insert(d.clone()) {
                frontier.push_back(d.clone());
            }
        }
    }
    out
}

fn crate_keys_for(pkg: &MetaPackage) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let pkg_key = pkg.name.replace('-', "_");
    out.push(pkg_key.clone());
    for tgt in &pkg.targets {
        if !target_is_compiled_crate(&tgt.kind) {
            continue;
        }
        let key = tgt.name.replace('-', "_");
        if key != pkg_key && !out.contains(&key) {
            out.push(key);
        }
    }
    out
}

fn target_is_compiled_crate(kinds: &[String]) -> bool {
    kinds.iter().any(|k| {
        matches!(
            k.as_str(),
            "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro" | "bin"
        )
    })
}

fn crate_key_from_target(target: &str) -> Option<String> {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return None;
    }
    let first = trimmed.split("::").next().unwrap_or(trimmed);
    if first.is_empty() || first == "crate" {
        return None;
    }
    Some(first.to_string())
}

#[derive(Debug, Deserialize)]
struct CargoMetadataJson {
    packages: Vec<MetaPackage>,
    workspace_members: Vec<String>,
    workspace_root: PathBuf,
    resolve: MetaResolve,
}

#[derive(Debug, Deserialize)]
struct MetaPackage {
    id: String,
    name: String,
    manifest_path: PathBuf,
    targets: Vec<MetaTarget>,
}

#[derive(Debug, Deserialize)]
struct MetaTarget {
    name: String,
    kind: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct MetaResolve {
    nodes: Vec<MetaNode>,
}

#[derive(Debug, Deserialize)]
struct MetaNode {
    id: String,
    dependencies: Vec<String>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    use std::fs;

    use crate::annotation::Tier;

    fn ann(target: &str) -> Annotation {
        Annotation {
            tier: Tier::Test,
            target: target.into(),
            source_spec: PathBuf::from("specs/a.md"),
            line: 1,
            criterion_line: 1,
        }
    }

    #[test]
    fn crate_key_from_target_takes_first_double_colon_segment() {
        assert_eq!(
            crate_key_from_target("loom_gate::dispatch::ok"),
            Some("loom_gate".into())
        );
        assert_eq!(
            crate_key_from_target("single_segment"),
            Some("single_segment".into())
        );
    }

    #[test]
    fn crate_key_from_target_drops_crate_placeholder_and_empties() {
        assert_eq!(crate_key_from_target("crate::module::x"), None);
        assert_eq!(crate_key_from_target("  "), None);
        assert_eq!(crate_key_from_target(""), None);
        assert_eq!(crate_key_from_target("::leading"), None);
    }

    #[test]
    fn crate_keys_for_includes_package_name_with_underscore_conversion() {
        let pkg = MetaPackage {
            id: "p".into(),
            name: "loom-gate".into(),
            manifest_path: PathBuf::from("/x/Cargo.toml"),
            targets: vec![MetaTarget {
                name: "loom_gate".into(),
                kind: vec!["lib".into()],
            }],
        };
        let keys = crate_keys_for(&pkg);
        assert_eq!(keys, vec!["loom_gate".to_string()]);
    }

    #[test]
    fn crate_keys_for_indexes_bin_targets_with_distinct_names() {
        let pkg = MetaPackage {
            id: "p".into(),
            name: "loom".into(),
            manifest_path: PathBuf::from("/x/Cargo.toml"),
            targets: vec![
                MetaTarget {
                    name: "loom".into(),
                    kind: vec!["lib".into()],
                },
                MetaTarget {
                    name: "loom_cli".into(),
                    kind: vec!["bin".into()],
                },
            ],
        };
        let keys = crate_keys_for(&pkg);
        assert!(keys.contains(&"loom".to_string()));
        assert!(keys.contains(&"loom_cli".to_string()));
    }

    #[test]
    fn crate_keys_for_skips_non_compiled_targets() {
        let pkg = MetaPackage {
            id: "p".into(),
            name: "x".into(),
            manifest_path: PathBuf::from("/x/Cargo.toml"),
            targets: vec![
                MetaTarget {
                    name: "x".into(),
                    kind: vec!["lib".into()],
                },
                MetaTarget {
                    name: "some_example".into(),
                    kind: vec!["example".into()],
                },
                MetaTarget {
                    name: "a_bench".into(),
                    kind: vec!["bench".into()],
                },
            ],
        };
        let keys = crate_keys_for(&pkg);
        assert_eq!(keys, vec!["x".to_string()]);
    }

    #[test]
    fn transitive_closure_walks_dependency_graph_depth_first() {
        let mut deps: HashMap<String, Vec<String>> = HashMap::new();
        deps.insert("a".into(), vec!["b".into(), "c".into()]);
        deps.insert("b".into(), vec!["d".into()]);
        deps.insert("c".into(), vec!["d".into()]);
        deps.insert("d".into(), vec![]);

        let closure = transitive_closure("a", &deps);
        let mut got: Vec<String> = closure.into_iter().collect();
        got.sort();
        assert_eq!(got, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn transitive_closure_handles_self_referential_cycles() {
        let mut deps: HashMap<String, Vec<String>> = HashMap::new();
        deps.insert("a".into(), vec!["a".into(), "b".into()]);
        deps.insert("b".into(), vec!["a".into()]);
        let closure = transitive_closure("a", &deps);
        let mut got: Vec<String> = closure.into_iter().collect();
        got.sort();
        assert_eq!(got, vec!["a", "b"]);
    }

    #[test]
    fn walk_rs_files_emits_workspace_relative_paths_and_skips_target_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("crate_a/src")).unwrap();
        fs::create_dir_all(root.join("crate_a/target/debug")).unwrap();
        fs::write(root.join("crate_a/src/lib.rs"), "").unwrap();
        fs::write(root.join("crate_a/src/other.rs"), "").unwrap();
        fs::write(root.join("crate_a/target/debug/build.rs"), "").unwrap();
        fs::write(root.join("crate_a/README.md"), "").unwrap();

        let files = walk_rs_files(&root.join("crate_a"), root).unwrap();
        assert_eq!(
            files,
            vec![
                PathBuf::from("crate_a/src/lib.rs"),
                PathBuf::from("crate_a/src/other.rs"),
            ]
        );
    }

    /// Synthetic two-crate fixture: `crate_a` depends on `crate_b`. Build
    /// a real on-disk workspace plus a synthetic `cargo metadata` JSON
    /// document pointing at it, then feed both to `from_metadata` without
    /// invoking `cargo`. This keeps the test deterministic and free of a
    /// `cargo` PATH dependency.
    fn write_two_crate_fixture(root: &Path) {
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crate_a\", \"crate_b\"]\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("crate_a/src")).unwrap();
        fs::create_dir_all(root.join("crate_b/src")).unwrap();
        fs::write(root.join("crate_a/Cargo.toml"), "").unwrap();
        fs::write(root.join("crate_a/src/lib.rs"), "").unwrap();
        fs::write(root.join("crate_a/src/util.rs"), "").unwrap();
        fs::write(root.join("crate_b/Cargo.toml"), "").unwrap();
        fs::write(root.join("crate_b/src/lib.rs"), "").unwrap();
    }

    fn fixture_metadata(root: &Path) -> CargoMetadataJson {
        let a_id = "path+file://crate_a#0.1.0".to_string();
        let b_id = "path+file://crate_b#0.1.0".to_string();
        let pkg = |id: &str, name: &str, dir: &str| MetaPackage {
            id: id.into(),
            name: name.into(),
            manifest_path: root.join(dir).join("Cargo.toml"),
            targets: vec![MetaTarget {
                name: name.replace('-', "_"),
                kind: vec!["lib".into()],
            }],
        };
        CargoMetadataJson {
            packages: vec![
                pkg(&a_id, "crate_a", "crate_a"),
                pkg(&b_id, "crate_b", "crate_b"),
            ],
            workspace_members: vec![a_id.clone(), b_id.clone()],
            workspace_root: root.to_path_buf(),
            resolve: MetaResolve {
                nodes: vec![
                    MetaNode {
                        id: a_id,
                        dependencies: vec![b_id.clone()],
                    },
                    MetaNode {
                        id: b_id,
                        dependencies: vec![],
                    },
                ],
            },
        }
    }

    #[test]
    fn synthetic_fixture_scope_for_owning_crate_includes_dep_crate_files() {
        let dir = tempfile::tempdir().unwrap();
        write_two_crate_fixture(dir.path());
        let scope = CargoMetadataScope::from_metadata(fixture_metadata(dir.path())).unwrap();

        let files = scope.scope_for(&ann("crate_a::tests::happy"));
        assert!(
            files.contains(&PathBuf::from("crate_a/src/lib.rs")),
            "owning crate's lib.rs: {files:?}"
        );
        assert!(
            files.contains(&PathBuf::from("crate_a/src/util.rs")),
            "owning crate's util.rs: {files:?}"
        );
        assert!(
            files.contains(&PathBuf::from("crate_b/src/lib.rs")),
            "dep crate's lib.rs (transitive): {files:?}"
        );
    }

    #[test]
    fn synthetic_fixture_scope_for_leaf_crate_excludes_consumer_files() {
        let dir = tempfile::tempdir().unwrap();
        write_two_crate_fixture(dir.path());
        let scope = CargoMetadataScope::from_metadata(fixture_metadata(dir.path())).unwrap();

        let files = scope.scope_for(&ann("crate_b::tests::ok"));
        assert!(files.contains(&PathBuf::from("crate_b/src/lib.rs")));
        assert!(
            !files.iter().any(|p| p.starts_with("crate_a")),
            "leaf crate must not see its consumer's files: {files:?}"
        );
    }

    #[test]
    fn synthetic_fixture_unknown_crate_returns_empty_scope() {
        let dir = tempfile::tempdir().unwrap();
        write_two_crate_fixture(dir.path());
        let scope = CargoMetadataScope::from_metadata(fixture_metadata(dir.path())).unwrap();

        assert!(scope.scope_for(&ann("nonexistent::tests::x")).is_empty());
        assert!(
            scope.scope_for(&ann("crate::placeholder")).is_empty(),
            "literal `crate::` placeholder cannot disambiguate"
        );
    }
}

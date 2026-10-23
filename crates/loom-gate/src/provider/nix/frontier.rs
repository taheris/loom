use std::collections::{BTreeMap, BTreeSet};

use super::{Error, Name, Nodes, Presence, StorePath};

/// The requested check always executes; available dependency artifacts stop realization traversal.
pub(super) fn require_pinned(
    nodes: &Nodes,
    requested: &StorePath,
    mut presence: impl FnMut(&BTreeMap<Name, StorePath>) -> Result<Presence, Error>,
) -> Result<(), Error> {
    let mut pending = vec![requested.clone()];
    let mut visited = BTreeSet::new();
    while let Some(path) = pending.pop() {
        if !visited.insert(path.clone()) {
            continue;
        }
        let node = nodes.get(&path).ok_or(Error::Root)?;
        for (dependency, required) in &node.dependencies {
            let child = nodes.get(dependency).ok_or_else(|| Error::Dependency {
                derivation: dependency.clone(),
            })?;
            let outputs = required
                .iter()
                .map(|name| {
                    let path = child.outputs.get(name).ok_or(Error::Output)?;
                    Ok((name.clone(), path.clone()))
                })
                .collect::<Result<BTreeMap<_, _>, Error>>()?;
            if matches!(presence(&outputs)?, Presence::Present) {
                continue;
            }
            if child.fixed {
                return Err(Error::ColdInput {
                    derivation: dependency.clone(),
                });
            }
            pending.push(dependency.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::Node;
    use super::*;

    fn path(name: &str) -> StorePath {
        StorePath::try_from(format!("/nix/store/{}-{name}", "a".repeat(32))).unwrap()
    }

    fn name(value: &str) -> Name {
        Name::try_from(value.to_owned()).unwrap()
    }

    fn node(fixed: bool, dependencies: &[&str], output: &str) -> Node {
        Node {
            builder: path("bash").as_str().to_owned(),
            system: name("x86_64-linux"),
            args: Vec::new(),
            env: BTreeMap::new(),
            structured: None,
            fixed,
            sources: BTreeSet::new(),
            dependencies: dependencies
                .iter()
                .map(|dependency| (path(dependency), [name("out")].into()))
                .collect(),
            outputs: [(name("out"), path(output))].into(),
        }
    }

    fn graph() -> Nodes {
        [
            (path("check.drv"), node(false, &["compiler.drv"], "check")),
            (
                path("compiler.drv"),
                node(false, &["archive.drv"], "compiler"),
            ),
            (path("archive.drv"), node(true, &[], "archive")),
        ]
        .into_iter()
        .map(|(path, node)| (path, std::sync::Arc::new(node)))
        .collect()
    }

    fn available(
        paths: &[&str],
    ) -> impl FnMut(&BTreeMap<Name, StorePath>) -> Result<Presence, Error> {
        let paths: BTreeSet<_> = paths.iter().map(|value| path(value)).collect();
        move |outputs| {
            Ok(if outputs.values().all(|output| paths.contains(output)) {
                Presence::Present
            } else {
                Presence::Cold
            })
        }
    }

    #[test]
    fn available_compiler_artifacts_do_not_require_archival_network_sources() {
        require_pinned(&graph(), &path("check.drv"), available(&["compiler"])).unwrap();
    }

    #[test]
    fn missing_compiler_requires_its_cold_fixed_inputs_before_dispatch() {
        assert!(
            matches!(require_pinned(&graph(), &path("check.drv"), available(&[])), Err(Error::ColdInput { derivation }) if derivation == path("archive.drv"))
        );
    }

    #[test]
    fn warm_check_output_does_not_hide_cold_execution_dependencies() {
        assert!(matches!(
            require_pinned(&graph(), &path("check.drv"), available(&["check"])),
            Err(Error::ColdInput { .. })
        ));
    }

    #[test]
    fn only_the_dependency_outputs_used_by_the_check_stop_traversal() {
        let mut graph = graph();
        std::sync::Arc::make_mut(graph.get_mut(&path("compiler.drv")).unwrap())
            .outputs
            .insert(name("doc"), path("compiler-doc"));
        require_pinned(&graph, &path("check.drv"), available(&["compiler"])).unwrap();
        std::sync::Arc::make_mut(graph.get_mut(&path("check.drv")).unwrap())
            .dependencies
            .get_mut(&path("compiler.drv"))
            .unwrap()
            .insert(name("doc"));
        assert!(matches!(
            require_pinned(&graph, &path("check.drv"), available(&["compiler"])),
            Err(Error::ColdInput { .. })
        ));
    }
}

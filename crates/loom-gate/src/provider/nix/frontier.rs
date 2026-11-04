use std::collections::{BTreeMap, BTreeSet};

use super::{Error, Name, Nodes, StorePath};

/// Immutable dependency artifacts and the recipes that may actually execute.
#[derive(Debug, Clone, serde::Serialize)]
pub(super) struct Execution {
    pub(super) nodes: BTreeSet<StorePath>,
    pub(super) inputs: BTreeSet<StorePath>,
    pub(super) cold_inputs: BTreeSet<StorePath>,
}

pub(super) fn capture(
    nodes: &Nodes,
    roots: impl IntoIterator<Item = StorePath>,
    mut present: impl FnMut(&BTreeSet<StorePath>) -> Result<BTreeSet<StorePath>, Error>,
) -> Result<Execution, Error> {
    let mut execution = Execution {
        nodes: BTreeSet::new(),
        inputs: BTreeSet::new(),
        cold_inputs: BTreeSet::new(),
    };
    let mut pending: Vec<_> = roots.into_iter().collect();
    while !pending.is_empty() {
        let mut dependencies = BTreeMap::<StorePath, BTreeSet<Name>>::new();
        for path in std::mem::take(&mut pending) {
            if !execution.nodes.insert(path.clone()) {
                continue;
            }
            let node = nodes.get(&path).ok_or(Error::Root)?;
            if node.fixed {
                continue;
            }
            for (dependency, outputs) in &node.dependencies {
                dependencies
                    .entry(dependency.clone())
                    .or_default()
                    .extend(outputs.iter().cloned());
            }
        }
        let required = dependencies
            .iter()
            .map(|(dependency, names)| {
                let node = nodes.get(dependency).ok_or(Error::Root)?;
                let outputs = names
                    .iter()
                    .map(|name| node.outputs.get(name).cloned().ok_or(Error::Output))
                    .collect::<Result<BTreeSet<_>, _>>()?;
                Ok((dependency.clone(), outputs))
            })
            .collect::<Result<BTreeMap<_, _>, Error>>()?;
        let paths = required.values().flatten().cloned().collect();
        let available = present(&paths)?;
        for (dependency, outputs) in required {
            if outputs.is_subset(&available) {
                execution.inputs.extend(outputs);
            } else if nodes.get(&dependency).ok_or(Error::Root)?.fixed {
                execution.cold_inputs.insert(dependency);
            } else {
                pending.push(dependency);
            }
        }
    }
    Ok(execution)
}

/// The requested check always executes; available dependency artifacts stop realization traversal.
pub(super) fn require_pinned(
    nodes: &Nodes,
    requested: &StorePath,
    present: impl FnMut(&BTreeSet<StorePath>) -> Result<BTreeSet<StorePath>, Error>,
) -> Result<BTreeSet<StorePath>, Error> {
    let execution = capture(nodes, [requested.clone()], present)?;
    if let Some(derivation) = execution.cold_inputs.into_iter().next() {
        return Err(Error::ColdInput { derivation });
    }
    Ok(execution.inputs)
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
            sources: [path("bash")].into(),
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
    ) -> impl FnMut(&BTreeSet<StorePath>) -> Result<BTreeSet<StorePath>, Error> {
        let paths: BTreeSet<_> = paths.iter().map(|value| path(value)).collect();
        move |outputs| Ok(outputs.intersection(&paths).cloned().collect())
    }

    #[test]
    fn captured_artifacts_stop_only_their_own_build_recipes() {
        let mut nodes = graph();
        std::sync::Arc::make_mut(nodes.get_mut(&path("compiler.drv")).unwrap())
            .env
            .insert("__darwinAllowLocalNetworking".to_owned(), "1".to_owned());
        let warm = capture(&nodes, [path("check.drv")], |paths| {
            Ok(paths
                .intersection(&[path("compiler")].into())
                .cloned()
                .collect())
        })
        .unwrap();
        assert_eq!(warm.nodes, [path("check.drv")].into());
        assert_eq!(warm.inputs, [path("compiler")].into());
        super::super::check_execution_graph(&nodes, &path("check.drv"), &warm.nodes).unwrap();
        let cold = capture(&nodes, [path("check.drv")], |paths| {
            Ok(paths
                .intersection(&[path("archive")].into())
                .cloned()
                .collect())
        })
        .unwrap();
        assert!(
            matches!(super::super::check_execution_graph(&nodes, &path("check.drv"), &cold.nodes), Err(Error::Unsafe { derivation }) if derivation == path("compiler.drv"))
        );
    }

    #[test]
    fn cold_transitive_tool_recipes_cannot_gain_host_dependencies() {
        let mut nodes = graph();
        nodes.insert(
            path("tls.drv"),
            std::sync::Arc::new(node(false, &[], "tls")),
        );
        std::sync::Arc::make_mut(nodes.get_mut(&path("compiler.drv")).unwrap())
            .dependencies
            .insert(path("tls.drv"), [name("out")].into());
        std::sync::Arc::make_mut(nodes.get_mut(&path("tls.drv")).unwrap())
            .env
            .insert(
                "__darwinImpureHostDeps".to_owned(),
                "/host/resource".to_owned(),
            );
        let execution = capture(&nodes, [path("check.drv")], available(&["archive"])).unwrap();
        assert!(
            matches!(super::super::check_execution_graph(&nodes, &path("check.drv"), &execution.nodes), Err(Error::Unsafe { derivation }) if derivation == path("tls.drv"))
        );
    }

    #[test]
    fn warm_outputs_never_hide_requested_execution_permissions() {
        let mut nodes = graph();
        std::sync::Arc::make_mut(nodes.get_mut(&path("check.drv")).unwrap())
            .env
            .insert("__noChroot".to_owned(), "1".to_owned());
        let execution = capture(&nodes, [path("check.drv")], |paths| Ok(paths.clone())).unwrap();
        assert!(
            matches!(super::super::check_execution_graph(&nodes, &path("check.drv"), &execution.nodes), Err(Error::Unsafe { derivation }) if derivation == path("check.drv"))
        );
    }

    #[test]
    fn explicit_preparation_roots_remain_checked_when_consumed_as_artifacts() {
        let nodes = graph();
        let execution = capture(&nodes, [path("check.drv"), path("compiler.drv")], |paths| {
            Ok(paths.clone())
        })
        .unwrap();
        assert!(execution.nodes.contains(&path("compiler.drv")));
        assert!(execution.inputs.contains(&path("compiler")));
    }

    #[test]
    fn frontier_discovery_batches_independent_dependency_outputs() {
        let mut nodes = graph();
        nodes.insert(
            path("other.drv"),
            std::sync::Arc::new(node(false, &[], "other")),
        );
        std::sync::Arc::make_mut(nodes.get_mut(&path("check.drv")).unwrap())
            .dependencies
            .insert(path("other.drv"), [name("out")].into());
        let mut queries = Vec::new();
        capture(&nodes, [path("check.drv")], |paths| {
            queries.push(paths.clone());
            Ok(paths.clone())
        })
        .unwrap();
        assert_eq!(queries[0], [path("compiler"), path("other")].into());
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

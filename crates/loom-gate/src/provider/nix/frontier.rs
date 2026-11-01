use std::collections::{BTreeMap, BTreeSet};

use super::{Error, Nodes, StorePath};

/// The requested check always executes; available dependency artifacts stop realization traversal.
pub(super) fn require_pinned(
    nodes: &Nodes,
    requested: &StorePath,
    mut presence: impl FnMut(&BTreeSet<StorePath>) -> Result<BTreeSet<StorePath>, Error>,
) -> Result<(), Error> {
    let mut pending = BTreeSet::from([requested.clone()]);
    let mut visited = BTreeSet::new();
    while !pending.is_empty() {
        let mut dependencies = BTreeMap::<StorePath, BTreeSet<StorePath>>::new();
        for path in std::mem::take(&mut pending) {
            if !visited.insert(path.clone()) {
                continue;
            }
            let node = nodes.get(&path).ok_or(Error::Root)?;
            for (dependency, required) in &node.dependencies {
                let child = nodes.get(dependency).ok_or_else(|| Error::Dependency {
                    derivation: dependency.clone(),
                })?;
                let outputs = dependencies.entry(dependency.clone()).or_default();
                for name in required {
                    outputs.insert(child.outputs.get(name).ok_or(Error::Output)?.clone());
                }
            }
        }
        let outputs = dependencies
            .values()
            .flatten()
            .cloned()
            .collect::<BTreeSet<_>>();
        if outputs.is_empty() {
            continue;
        }
        // One live backend read per reachable frontier, not one process per edge.
        // Do not inspect recipes behind warm outputs or retain availability across runs.
        let present = presence(&outputs)?;
        if !present.is_subset(&outputs) {
            return Err(Error::Result);
        }
        for (dependency, required) in dependencies {
            if required.is_subset(&present) {
                continue;
            }
            if nodes.get(&dependency).ok_or(Error::Root)?.fixed {
                return Err(Error::ColdInput {
                    derivation: dependency,
                });
            }
            pending.insert(dependency);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{Name, Node};
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
    ) -> impl FnMut(&BTreeSet<StorePath>) -> Result<BTreeSet<StorePath>, Error> {
        let paths: BTreeSet<_> = paths.iter().map(|value| path(value)).collect();
        move |outputs| Ok(outputs.intersection(&paths).cloned().collect())
    }

    #[test]
    fn wide_shared_frontier_uses_one_live_read_per_level() {
        let mut nodes = graph();
        let mut compilers = Vec::new();
        for index in 0..64 {
            let compiler = format!("compiler-{index}");
            nodes.insert(
                path(&format!("{compiler}.drv")),
                std::sync::Arc::new(node(false, &["archive.drv"], &compiler)),
            );
            compilers.push(format!("{compiler}.drv"));
        }
        nodes.insert(
            path("check.drv"),
            std::sync::Arc::new(node(
                false,
                &compilers.iter().map(String::as_str).collect::<Vec<_>>(),
                "check",
            )),
        );
        let mut queries = Vec::new();
        require_pinned(&nodes, &path("check.drv"), |outputs| {
            queries.push(outputs.clone());
            Ok(outputs
                .iter()
                .filter(|output| **output == path("archive"))
                .cloned()
                .collect())
        })
        .unwrap();
        assert_eq!(queries.len(), 2);
        assert_eq!(queries[0].len(), 64);
        assert_eq!(queries[1], BTreeSet::from([path("archive")]));
    }

    #[test]
    fn warm_frontier_does_not_query_archival_dependencies() {
        let mut queries = Vec::new();
        require_pinned(&graph(), &path("check.drv"), |outputs| {
            queries.push(outputs.clone());
            Ok(outputs.clone())
        })
        .unwrap();
        assert_eq!(queries, vec![BTreeSet::from([path("compiler")])]);
    }

    #[test]
    fn frontier_availability_is_observed_again_on_each_request() {
        let mut reads = 0;
        let mut presence = |outputs: &BTreeSet<StorePath>| {
            reads += 1;
            Ok(if reads == 1 {
                outputs.clone()
            } else {
                BTreeSet::new()
            })
        };
        require_pinned(&graph(), &path("check.drv"), &mut presence).unwrap();
        assert!(matches!(
            require_pinned(&graph(), &path("check.drv"), &mut presence),
            Err(Error::ColdInput { .. })
        ));
        assert_eq!(reads, 3);
    }

    #[test]
    fn frontier_rejects_backend_errors_and_unrequested_availability() {
        assert!(matches!(
            require_pinned(&graph(), &path("check.drv"), |_| Err(Error::Result)),
            Err(Error::Result)
        ));
        assert!(matches!(
            require_pinned(&graph(), &path("check.drv"), |_| Ok(BTreeSet::from([
                path("unrequested")
            ]))),
            Err(Error::Result)
        ));
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

//! One pure validation pass compares shared native nodes once, without retaining readiness.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::sync::Arc;

use super::{Error, Node, Nodes, StorePath};

pub(super) fn require_nodes<'a>(
    observed: &Nodes,
    captured: impl IntoIterator<Item = &'a Nodes>,
) -> Result<(), Error> {
    let changed = || Error::GraphDrift {
        detail: "native dependency graph changed during batch validation".into(),
    };
    let mut expected = BTreeMap::<&StorePath, &Arc<Node>>::new();
    for nodes in captured {
        for (path, node) in nodes {
            match expected.entry(path) {
                Entry::Vacant(entry) => {
                    entry.insert(node);
                }
                Entry::Occupied(entry) => {
                    if !Arc::ptr_eq(entry.get(), node) && *entry.get() != node {
                        return Err(changed());
                    }
                }
            }
        }
    }
    if observed.len() != expected.len()
        || expected
            .into_iter()
            .any(|(path, node)| observed.get(path) != Some(node))
    {
        return Err(changed());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::Name;
    use std::collections::BTreeSet;

    fn path(name: &str) -> StorePath {
        StorePath::try_from(format!("/nix/store/{}-{name}", "a".repeat(32))).unwrap()
    }

    fn node(command: &str) -> Arc<Node> {
        Arc::new(Node {
            builder: path("bash").as_str().into(),
            system: Name::try_from("x86_64-linux".to_owned()).unwrap(),
            args: vec![command.into()],
            env: BTreeMap::new(),
            structured: None,
            fixed: false,
            sources: BTreeSet::new(),
            dependencies: BTreeMap::new(),
            outputs: BTreeMap::new(),
        })
    }

    #[test]
    fn shared_and_independently_captured_nodes_preserve_complete_batch_identity() {
        let shared = node("compiler");
        let first: Nodes = [
            (path("compiler.drv"), shared.clone()),
            (path("first.drv"), node("first")),
        ]
        .into();
        let second: Nodes = [
            (path("compiler.drv"), shared),
            (path("second.drv"), node("second")),
        ]
        .into();
        let observed: Nodes = [
            (path("compiler.drv"), node("compiler")),
            (path("first.drv"), node("first")),
            (path("second.drv"), node("second")),
        ]
        .into();
        require_nodes(&observed, [&first, &second]).unwrap();
        let independent: Nodes = [(path("compiler.drv"), node("compiler"))].into();
        require_nodes(&observed, [&first, &second, &independent]).unwrap();
    }

    #[test]
    fn conflicting_captures_cannot_hide_a_stale_sibling_in_either_order() {
        let first: Nodes = [(path("compiler.drv"), node("before"))].into();
        let second: Nodes = [(path("compiler.drv"), node("after"))].into();
        assert!(require_nodes(&second, [&first, &second]).is_err());
        assert!(require_nodes(&second, [&second, &first]).is_err());
    }

    #[test]
    fn changed_missing_or_added_nodes_reject_the_whole_batch() {
        let captured: Nodes = [(path("compiler.drv"), node("compiler"))].into();
        let changed: Nodes = [(path("compiler.drv"), node("mutant"))].into();
        let added: Nodes = captured
            .iter()
            .map(|(path, node)| (path.clone(), node.clone()))
            .chain([(path("unexpected.drv"), node("unexpected"))])
            .collect();
        for observed in [&changed, &Nodes::new(), &added] {
            assert!(require_nodes(observed, [&captured]).is_err());
        }
        require_nodes(&Nodes::new(), []).unwrap();
    }
}

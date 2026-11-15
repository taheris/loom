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
    fn native_presence_queries_batch_all_paths_without_losing_boundary_entries() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let tool = directory.path().join("nix");
        std::fs::write(
            &tool,
            loom_test_support::bash_script(
                r#"set -euo pipefail
[[ "$*" == 'path-info --json --json-format 1 --offline '* ]]
shift 5
[[ "${*: -5}" == '--extra-experimental-features nix-command flakes --option accept-flake-config false' ]]
count=$(($# - 5))
set -- "${@:1:count}"
printf '%s\n' "$#" >> "${0%/*}/calls"
printf '{'
separator=
for path in "$@"; do
    printf '%s"%s":{"narSize":0}' "$separator" "$path"
    separator=,
done
printf '}\n'
"#,
            ),
        )
        .unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o700)).unwrap();
        let paths: BTreeSet<_> = (0..2050)
            .map(|index| path(&format!("input-{index}")))
            .collect();
        assert_eq!(super::super::present_paths(&tool, &paths).unwrap(), paths);
        assert_eq!(
            std::fs::read_to_string(directory.path().join("calls")).unwrap(),
            "512\n512\n512\n512\n2\n"
        );
    }

    #[test]
    fn native_graph_tests_retain_debug_and_overflow_assertions() {
        assert!(std::hint::black_box(cfg!(debug_assertions)));
        let overflow = std::panic::catch_unwind(|| std::hint::black_box(u64::MAX) + 1);
        assert!(overflow.is_err());
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

    fn resource(references: &BTreeSet<StorePath>) -> super::super::Resource {
        serde_json::from_value(serde_json::json!({
            "narHash": "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "references": references,
        }))
        .unwrap()
    }

    #[test]
    fn shared_resource_capture_preserves_each_exact_transitive_closure() {
        let shared = path("shared");
        let first = path("first");
        let second = path("second");
        let resources = BTreeMap::from([
            (shared.clone(), resource(&BTreeSet::from([shared.clone()]))),
            (first.clone(), resource(&BTreeSet::from([shared.clone()]))),
            (second.clone(), resource(&BTreeSet::new())),
        ]);
        for sources in [
            BTreeSet::from([first.clone()]),
            BTreeSet::from([second.clone()]),
            BTreeSet::from([first.clone(), second.clone()]),
        ] {
            let captured = super::super::resource_closure(&resources, &sources).unwrap();
            assert_eq!(captured.contains_key(&first), sources.contains(&first));
            assert_eq!(captured.contains_key(&second), sources.contains(&second));
            assert_eq!(captured.contains_key(&shared), sources.contains(&first));
        }
        assert!(
            super::super::resource_closure(&resources, &BTreeSet::new())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn shared_resource_capture_rejects_missing_roots_and_transitive_references() {
        let missing = path("missing");
        let root = path("root");
        let resources =
            BTreeMap::from([(root.clone(), resource(&BTreeSet::from([missing.clone()])))]);
        for sources in [BTreeSet::from([root]), BTreeSet::from([missing])] {
            assert!(matches!(
                super::super::resource_closure(&resources, &sources),
                Err(Error::Resource { .. })
            ));
        }
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

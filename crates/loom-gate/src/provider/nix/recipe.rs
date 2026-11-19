//! Recipe fields are bound to native NAR identities; every freshness pass verifies their contents.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::{Error, Nodes, Resource, StorePath, current, query_paths, read_graphs};

pub(super) type Captured = BTreeMap<StorePath, Resource>;

pub(super) fn capture(tool: &Path, roots: &[StorePath], nodes: &Nodes) -> Result<Captured, Error> {
    let paths = nodes.keys().cloned().collect();
    let captured = read(tool, &paths)?;
    current::require_nodes(&read_graphs(tool, roots)?, [nodes])?;
    require_current(tool, [&captured])?;
    Ok(captured)
}

pub(super) fn require_current<'a>(
    tool: &Path,
    captures: impl IntoIterator<Item = &'a Captured>,
) -> Result<(), Error> {
    require_current_with_resources(tool, captures, std::iter::empty())
}

pub(super) fn require_current_with_resources<'a>(
    tool: &Path,
    recipes: impl IntoIterator<Item = &'a Captured>,
    resources: impl IntoIterator<Item = &'a Captured>,
) -> Result<(), Error> {
    let recipes: Vec<_> = recipes.into_iter().collect();
    let verified = recipes
        .iter()
        .flat_map(|capture| capture.keys().cloned())
        .collect();
    let mut expected = BTreeMap::new();
    for captured in recipes.into_iter().chain(resources) {
        for (path, resource) in captured {
            if expected
                .insert(path, resource)
                .is_some_and(|old| old != resource)
            {
                return Err(changed(path));
            }
        }
    }
    let paths = expected.keys().map(|path| (*path).clone()).collect();
    let observed = read(tool, &paths)?;
    if let Some((path, _)) = expected
        .iter()
        .find(|(path, resource)| observed.get(**path) != Some(**resource))
    {
        return Err(changed(path));
    }
    // Registered hashes alone cannot detect corruption of the actual recipe bytes.
    query_paths(
        tool,
        &["store", "verify", "--offline", "--quiet"],
        &verified,
    )?;
    Ok(())
}

fn read(tool: &Path, paths: &BTreeSet<StorePath>) -> Result<Captured, Error> {
    let observed: BTreeMap<StorePath, Option<Resource>> = serde_json::from_slice(&query_paths(
        tool,
        &["path-info", "--json", "--json-format", "1", "--offline"],
        paths,
    )?)?;
    if observed.len() != paths.len() || observed.keys().any(|path| !paths.contains(path)) {
        return Err(Error::Result);
    }
    observed
        .into_iter()
        .map(|(path, resource)| {
            resource
                .map(|resource| (path.clone(), resource))
                .ok_or(Error::Resource { path })
        })
        .collect()
}

fn changed(path: &StorePath) -> Error {
    Error::GraphDrift {
        detail: format!(
            "native recipe content or references changed: {}",
            path.as_str()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn shared_metadata_read_preserves_resource_conflicts_and_recipe_byte_verification() {
        let private = super::super::private_store::Store::new();
        let directory = tempfile::tempdir().unwrap();
        let tool = directory.path().join("nix");
        loom_test_support::write_executable_bash_script(
            &tool,
            &format!(
                "set -euo pipefail\nprintf '%s\\n' \"$1 $2\" >> \"${{0%/*}}/calls\"\nexec '{}' \"$@\"\n",
                private.tool().display()
            ),
        ).unwrap();
        let expression = format!(
            "(builtins.derivation {{ name = \"shared-metadata-probe\"; system = \"{}\"; builder = \"/bin/sh\"; }}).drvPath",
            crate::runner::result::current_platform()
        );
        let root: StorePath = serde_json::from_slice(
            &super::super::query(
                &tool,
                &["eval", "--json", "--offline", "--expr", &expression],
            )
            .unwrap(),
        )
        .unwrap();
        let captured = read(&tool, &BTreeSet::from([root.clone()])).unwrap();
        std::fs::write(directory.path().join("calls"), "").unwrap();
        require_current_with_resources(&tool, [&captured], [&captured, &captured]).unwrap();
        assert_eq!(
            std::fs::read_to_string(directory.path().join("calls")).unwrap(),
            "path-info --json\nstore verify\n"
        );
        let mut conflicting = captured.clone();
        conflicting.get_mut(&root).unwrap().references.insert(
            StorePath::try_from(format!("/nix/store/{}-unexpected", "a".repeat(32))).unwrap(),
        );
        for resources in [[&captured, &conflicting], [&conflicting, &captured]] {
            assert!(matches!(
                require_current_with_resources(&tool, [&captured], resources),
                Err(Error::GraphDrift { .. })
            ));
        }
        assert!(matches!(
            require_current_with_resources(&tool, [&conflicting], [&conflicting]),
            Err(Error::GraphDrift { .. })
        ));
        std::fs::remove_file(private.physical(&root)).unwrap();
        assert!(require_current_with_resources(&tool, [&captured], [&captured]).is_err());
    }

    #[test]
    fn recipe_freshness_rejects_corruption_even_with_unchanged_registered_hash_and_mtime() {
        let private = super::super::private_store::Store::new();
        let tool = private.tool().to_path_buf();
        let expression = format!(
            "(builtins.derivation {{ name = \"recipe-content-probe\"; system = \"{}\"; builder = \"/bin/sh\"; args = [ \"-c\" \"exit 0\" ]; }}).drvPath",
            crate::runner::result::current_platform()
        );
        let root: StorePath = serde_json::from_slice(
            &super::super::query(
                &tool,
                &["eval", "--json", "--offline", "--expr", &expression],
            )
            .unwrap(),
        )
        .unwrap();
        let nodes = super::super::read_graph(&tool, &root).unwrap();
        let captured = capture(&tool, std::slice::from_ref(&root), &nodes).unwrap();
        require_current(&tool, [&captured, &captured]).unwrap();
        let mut conflicting = captured.clone();
        conflicting.get_mut(&root).unwrap().content = super::super::NarHash::try_from(
            "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_owned(),
        )
        .unwrap();
        assert!(matches!(
            require_current(&tool, [&captured, &conflicting]),
            Err(Error::GraphDrift { .. })
        ));
        let file = private.physical(&root);
        let original = std::fs::read_to_string(&file).unwrap();
        let modified = original.replace("exit 0", "exit 1");
        assert_ne!(original, modified);
        assert_eq!(original.len(), modified.len());
        let mtime = file.metadata().unwrap().modified().unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&file, modified).unwrap();
        std::fs::File::open(&file)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(mtime))
            .unwrap();
        assert_eq!(file.metadata().unwrap().modified().unwrap(), mtime);
        assert_eq!(
            read(&tool, &nodes.keys().cloned().collect()).unwrap(),
            captured
        );
        assert!(matches!(
            require_current(&tool, [&captured]),
            Err(Error::Query { .. })
        ));
        std::fs::write(file, original).unwrap();
        require_current(&tool, [&captured]).unwrap();
    }
}

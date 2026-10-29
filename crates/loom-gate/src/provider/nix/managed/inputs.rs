//! Registration contains only the admitted input closure, never derivation-output expansion.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::num::NonZeroU64;

use serde::Deserialize;

use super::super::{Error, Graph, Node, Resource, StorePath};

#[derive(Deserialize)]
struct Info {
    #[serde(flatten)]
    resource: Resource,
    #[serde(rename = "narSize")]
    size: NonZeroU64,
}

pub(super) struct Registration {
    pub(super) bytes: Vec<u8>,
    pub(super) paths: BTreeSet<StorePath>,
}

pub(super) fn registration(
    graph: &Graph,
    reporter: &Node,
    executor: &super::Executor,
) -> Result<Registration, Error> {
    let mut roots: BTreeSet<_> = graph.resources.keys().cloned().collect();
    roots.insert(StorePath::root(&executor.environment)?);
    if let Some(namespace) = &executor.namespace {
        roots.insert(StorePath::root(&namespace.program)?);
    }
    roots.extend(reporter.sources.iter().cloned());
    for (dependency, outputs) in &reporter.dependencies {
        let node = graph.nodes.get(dependency).ok_or(Error::Root)?;
        for output in outputs {
            roots.insert(node.outputs.get(output).ok_or(Error::Output)?.clone());
        }
    }
    let objects = query(&graph.tool, &roots)?;
    for (path, resource) in &graph.resources {
        if objects
            .get(path)
            .and_then(Option::as_ref)
            .is_none_or(|info| &info.resource != resource)
        {
            return Err(Error::GraphChanged);
        }
    }
    checked(&roots, &objects)
}

#[cfg(all(test, feature = "native-container-tests"))]
pub(super) fn capture(
    tool: &std::path::Path,
    roots: &BTreeSet<StorePath>,
) -> Result<Registration, Error> {
    checked(roots, &query(tool, roots)?)
}

fn query(
    tool: &std::path::Path,
    roots: &BTreeSet<StorePath>,
) -> Result<BTreeMap<StorePath, Option<Info>>, Error> {
    let mut arguments = vec![
        "path-info",
        "--recursive",
        "--json",
        "--json-format",
        "1",
        "--offline",
    ];
    arguments.extend(roots.iter().map(StorePath::as_str));
    Ok(serde_json::from_slice(&super::super::query(
        tool, &arguments,
    )?)?)
}

fn checked(
    roots: &BTreeSet<StorePath>,
    objects: &BTreeMap<StorePath, Option<Info>>,
) -> Result<Registration, Error> {
    Ok(Registration {
        bytes: encode(roots, objects)?,
        paths: objects.keys().cloned().collect(),
    })
}

fn encode(
    roots: &BTreeSet<StorePath>,
    objects: &BTreeMap<StorePath, Option<Info>>,
) -> Result<Vec<u8>, Error> {
    let require = |path: &StorePath| {
        objects
            .get(path)
            .and_then(Option::as_ref)
            .ok_or_else(|| Error::Resource { path: path.clone() })
    };
    let mut pending: Vec<_> = roots.iter().collect();
    let mut reachable = BTreeSet::new();
    while let Some(path) = pending.pop() {
        if reachable.insert(path) {
            pending.extend(require(path)?.resource.references());
        }
    }
    if let Some(path) = objects.keys().find(|path| !reachable.contains(path)) {
        return Err(Error::Resource { path: path.clone() });
    }
    let mut bytes = Vec::new();
    for (path, info) in objects {
        let info = info
            .as_ref()
            .ok_or_else(|| Error::Resource { path: path.clone() })?;
        for reference in info.resource.references() {
            require(reference)?;
        }
        writeln!(
            bytes,
            "{}\n{}\n{}\n\n{}",
            path.as_str(),
            info.resource.content().0,
            info.size,
            info.resource.references().len()
        )?;
        for reference in info.resource.references() {
            writeln!(bytes, "{}", reference.as_str())?;
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> StorePath {
        StorePath::try_from(format!("/nix/store/{}-{name}", "0".repeat(32))).unwrap()
    }

    fn info(references: &[StorePath]) -> Info {
        serde_json::from_value(serde_json::json!({
            "narHash": "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "narSize": 128,
            "references": references,
        }))
        .unwrap()
    }

    #[test]
    fn registration_preserves_recipe_documents_without_realizing_their_outputs() {
        let recipe = path("image.drv");
        let source = path("input");
        let objects = BTreeMap::from([
            (recipe.clone(), Some(info(std::slice::from_ref(&source)))),
            (source, Some(info(&[]))),
        ]);
        let encoded =
            String::from_utf8(encode(&BTreeSet::from([recipe.clone()]), &objects).unwrap())
                .unwrap();
        assert!(encoded.contains(&format!("{}\nsha256-", recipe.as_str())));
        assert_eq!(encoded.matches("sha256-").count(), 2);
        assert!(!encoded.contains("image-output"));
    }

    #[test]
    fn registration_cannot_admit_an_unrelated_ambient_object() {
        let source = path("source");
        let unrelated = path("unrelated");
        let objects = BTreeMap::from([
            (source.clone(), Some(info(&[]))),
            (unrelated, Some(info(&[]))),
        ]);
        assert!(encode(&BTreeSet::from([source]), &objects).is_err());
    }

    #[test]
    fn registration_rejects_a_missing_root_or_unclosed_reference() {
        let source = path("source");
        let missing = path("missing");
        let objects =
            BTreeMap::from([(source.clone(), Some(info(std::slice::from_ref(&missing))))]);
        assert!(encode(&BTreeSet::from([missing]), &objects).is_err());
        assert!(encode(&BTreeSet::from([source]), &objects).is_err());
    }
}

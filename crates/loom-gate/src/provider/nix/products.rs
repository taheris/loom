//! Immutable inputs for an out-of-builder consumer. Neither the producer of the
//! report nor its assertion is a product; those must still execute in their lane.

#[cfg(test)]
#[path = "products_tests.rs"]
mod tests;

use std::collections::BTreeMap;
use std::path::Path;

use super::{Built, Error, Name, Node, Nodes, StorePath, diagnostic, invoke, process};

/// Temporary output links hold newly built products as well as previously
/// available ones until the external consumer finishes reading them.
pub(super) struct Products {
    roots: tempfile::TempDir,
}

impl Products {
    pub(super) fn acquire(tool: &Path, nodes: &Nodes, consumer: &Node) -> Result<Self, Error> {
        let products = Self {
            roots: tempfile::tempdir()?,
        };
        let mut requested = Vec::new();
        let mut expected = BTreeMap::new();
        for (recipe, names) in &consumer.dependencies {
            let producer = nodes.get(recipe).ok_or(Error::Root)?;
            let mut outputs = BTreeMap::new();
            for name in names {
                let path = producer.outputs.get(name).ok_or(Error::Output)?;
                outputs.insert(name.clone(), path.clone());
            }
            if outputs.is_empty() {
                return Err(Error::Output);
            }
            requested.push(format!(
                "{}^{}",
                recipe.as_str(),
                names.iter().map(Name::as_str).collect::<Vec<_>>().join(",")
            ));
            expected.insert(recipe.clone(), outputs);
        }
        if !requested.is_empty() {
            // Nix installs its GC roots while realizing the requested outputs.
            // Pinning only the frontier observed *before* this call leaves
            // newly realized products unprotected after the child exits.
            let link = products.roots.path().join("product");
            let mut args = vec![
                "build",
                "--json",
                "--out-link",
                link.to_str().ok_or(Error::StorePath)?,
                "--offline",
                "--no-substitute",
            ];
            process::policy(None, &mut args);
            args.extend(requested.iter().map(String::as_str));
            let result = invoke(tool, &args)?;
            if !result.status.success() {
                return Err(Error::Report {
                    detail: format!(
                        "native compiler/input preparation failed: {}",
                        diagnostic(&result.stderr)
                    ),
                });
            }
            require_outputs(&result.stdout, expected)?;
        }
        Ok(products)
    }
}

fn require_outputs(
    bytes: &[u8],
    mut pending: BTreeMap<StorePath, BTreeMap<Name, StorePath>>,
) -> Result<(), Error> {
    for built in serde_json::from_slice::<Vec<Built>>(bytes)? {
        if pending.remove(&built.drv_path).as_ref() != Some(&built.outputs) {
            return Err(Error::Result);
        }
    }
    if !pending.is_empty() {
        return Err(Error::Result);
    }
    Ok(())
}

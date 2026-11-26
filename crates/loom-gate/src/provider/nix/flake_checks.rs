//! Build obligations for the root flake in the live publication Gate.
//! Standard flake structure validation is the independent first pre-push hook.
//!
//! Only an exact raw derivation fulfilled by a selected strict *whole check*
//! can be delegated. A fresh report wrapper, artifact, or projection is not
//! that derivation. Anything not delegated retains the normal realization duty.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use displaydoc::Display;
use loom_driver::git::GitOid;
use thiserror::Error;

use super::{Built, Digest, Graph, Name, Platforms, Purpose, StorePath, process};
use crate::inventory::publication::{self, Sources};
use crate::inventory::snapshot::Snapshot;
use crate::provider::Execution;
use crate::provider::plan::selection::{Selection, Stage};
use crate::provider::tracked::{Observation, Observed, Outcome};
use crate::scope::Platform;

/// Pending same-attempt obligations. Not a receipt, and no historical results are accepted.
#[must_use]
pub struct Checks {
    sources: Option<Sources>,
    delegated: BTreeMap<Name, Expected>,
}

struct Expected {
    recipe: StorePath,
    graph: Digest,
    outputs: BTreeSet<StorePath>,
}

type Roots = BTreeSet<StorePath>;

#[derive(serde::Deserialize)]
struct Store {
    url: String,
}

impl Checks {
    /// Account for every raw native flake check. Fallback uses the complete
    /// stock flake check if the exact live namespace or clean range is unknown.
    ///
    /// # Errors
    /// Nix failure, a stale handoff, or invalid selection cannot discharge the flake lane.
    pub(crate) fn start(selection: &Selection<'_>, snapshot: &Snapshot) -> Result<Self, Error> {
        if selection.stage() != Stage::Publication || selection.scope().diff().is_none() {
            return Err(Error::Scope);
        }
        let tool = super::locate_tool()?;
        let root = snapshot.root();
        let sources = selection
            .scope()
            .diff()
            .and_then(|range| bind(snapshot, range));
        let Some(sources) = sources.filter(|_| selection.scope().platform() == Platform::Native)
        else {
            return Self::stock(&tool, root);
        };
        if !same_nix_context(&tool, root) {
            tracing::debug!(
                "stock and checked Nix contexts differ; keep complete stock flake check"
            );
            sources.require_current()?;
            return Self::stock(&tool, root);
        }
        let roots = match raw_roots(&tool, root) {
            Ok(roots) => roots,
            Err(error) => {
                tracing::debug!(%error, "raw flake namespace unresolved; keep complete stock check");
                sources.require_current()?;
                return Self::stock(&tool, root);
            }
        };
        sources.require_current()?;
        let (delegated, residual) = assign(selection, &tool, roots)?;
        // Ordinary flake check requests every output of each applicable root.
        // Retain that build mode (no --rebuild) for roots not proven by Gate.
        realize(&tool, root, &residual)?;
        sources.require_current()?;
        tracing::info!(
            delegated = delegated.len(),
            residual = residual.len(),
            "root flake build duties assigned"
        );
        Ok(Self {
            sources: Some(sources),
            delegated,
        })
    }

    fn stock(tool: &Path, root: &Path) -> Result<Self, Error> {
        let mut command = std::process::Command::new(tool);
        command.current_dir(root).args(["flake", "check"]);
        super::query_output(super::invoke_command(command, Some("flake"))?)?;
        Ok(Self {
            sources: None,
            delegated: BTreeMap::new(),
        })
    }

    /// Close an exact delegated duty with that target's fresh whole-check output.
    ///
    /// # Errors
    /// A failed, skipped, substituted-by-report, or incomplete observation cannot discharge it.
    pub fn observe(&mut self, observed: &Observed) -> Result<(), Error> {
        let Some(expected) = self.delegated.get(observed.target()) else {
            return Ok(());
        };
        if !explains(expected, observed) {
            return Err(Error::NotDischarged {
                target: observed.target().clone(),
                recipe: expected.recipe.clone(),
            });
        }
        self.delegated.remove(observed.target());
        Ok(())
    }

    /// Recheck the exact pushed source after every delegated and residual duty completed.
    ///
    /// # Errors
    /// A missing build or changed checkout leaves flake build coverage unverified.
    pub fn finish(self) -> Result<(), Error> {
        if let Some((target, expected)) = self.delegated.into_iter().next() {
            return Err(Error::NotDischarged {
                target,
                recipe: expected.recipe,
            });
        }
        if let Some(sources) = self.sources {
            sources.require_current()?;
        }
        Ok(())
    }
}

fn bind(snapshot: &Snapshot, range: &str) -> Option<Sources> {
    let (prior, head) = range.split_once("..")?;
    let (prior, head) = (GitOid::new(prior).ok()?, GitOid::new(head).ok()?);
    match Sources::capture(snapshot, &prior, &head) {
        Ok(sources) => Some(sources),
        Err(error) => {
            tracing::debug!(%error, "no exact source binding; keep complete stock flake check");
            None
        }
    }
}

fn same_nix_context(tool: &Path, root: &Path) -> bool {
    let compare = || -> Result<bool, super::Error> {
        let mut args = vec!["config", "show", "--json", "--offline", "--no-substitute"];
        process::policy(None, &mut args);
        let ambient = config_values(&ambient_query(tool, root, &args)?)?;
        let checked = config_values(&super::query(tool, &args)?)?;
        if ambient != checked {
            return Ok(false);
        }
        // An explicit canonical daemon is common inside native test fixtures.
        // Compare the effective backend rather than guessing from the variable.
        let args = ["store", "info", "--json"];
        let stock: Store = serde_json::from_slice(&ambient_query(tool, root, &args)?)?;
        let checked: Store = serde_json::from_slice(&super::query(tool, &args)?)?;
        Ok(matches!(checked.url.as_str(), "local" | "daemon") && stock.url == checked.url)
    };
    compare().unwrap_or(false)
}

fn ambient_query(tool: &Path, root: &Path, args: &[&str]) -> Result<Vec<u8>, super::Error> {
    let mut stock = std::process::Command::new(tool);
    stock
        .current_dir(root)
        .args(args)
        .args(["--extra-experimental-features", "nix-command flakes"]);
    super::query_output(super::invoke_command(stock, args.first().copied())?)
}

fn config_values(json: &[u8]) -> Result<BTreeMap<String, serde_json::Value>, super::Error> {
    let mut settings: BTreeMap<String, serde_json::Value> = serde_json::from_slice(json)?;
    // Pure locked flake resolution doesn't read the legacy Nix search path.
    settings.remove("nix-path");
    // Store aliases such as auto and daemon are compared by effective URL above.
    settings.remove("store");
    for setting in settings.values_mut() {
        *setting = setting.get_mut("value").ok_or(super::Error::Result)?.take();
    }
    Ok(settings)
}

fn raw_roots(tool: &Path, root: &Path) -> Result<Roots, Error> {
    let config = command(tool, root, vec!["config", "show", "--json"])?;
    let platform: Platforms = serde_json::from_slice(&config).map_err(super::Error::from)?;
    // The leading dot selects the exact top-level namespace. The normal
    // installable search also tries packages.<system> and legacyPackages.
    let installable = format!(".#.checks.{}", platform.system.value.as_str());
    let values = command(
        tool,
        root,
        vec![
            "eval",
            "--json",
            "--offline",
            "--no-write-lock-file",
            "--no-update-lock-file",
            "--option",
            "allow-import-from-derivation",
            "false",
            &installable,
            "--apply",
            "builtins.mapAttrs (_: value: value.drvPath)",
        ],
    )?;
    let attributes: BTreeMap<Name, StorePath> =
        serde_json::from_slice(&values).map_err(super::Error::from)?;
    Ok(attributes.into_values().collect())
}

fn assign(
    selection: &Selection<'_>,
    tool: &Path,
    mut residual: Roots,
) -> Result<(BTreeMap<Name, Expected>, Roots), Error> {
    let mut delegated = BTreeMap::new();
    for decision in selection
        .decisions()
        .iter()
        .filter(|d| d.reason().selected())
    {
        let unit = decision.unit();
        let Execution::Nix(graph) = &unit.definition.execution else {
            continue;
        };
        if !plain_root(graph, tool) || !residual.remove(graph.derivation()) {
            continue;
        }
        let outputs = graph.outputs()?.values().cloned().collect();
        delegated.insert(
            unit.target().clone(),
            Expected {
                recipe: graph.derivation().clone(),
                graph: graph.identity().clone(),
                outputs,
            },
        );
    }
    Ok((delegated, residual))
}

fn plain_root(graph: &Graph, tool: &Path) -> bool {
    graph.is_confined_realization()
        && graph.purpose == Purpose::Check
        && graph.cargo.is_none()
        && graph.tool == tool
}

fn explains(expected: &Expected, observed: &Observed) -> bool {
    let mut roots = observed.observations().iter().filter_map(|obs| match obs {
        Observation::ExecutionGraph { digest } => Some(digest),
        _ => None,
    });
    let outputs = observed
        .observations()
        .iter()
        .filter_map(|obs| match obs {
            Observation::Output { path } => Some(path.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    observed.outcome() == &Outcome::Pass
        && roots.next() == Some(&expected.graph)
        && roots.next().is_none()
        && !expected.outputs.is_empty()
        && outputs == expected.outputs
}

fn realize(tool: &Path, root: &Path, paths: &Roots) -> Result<(), Error> {
    if paths.is_empty() {
        return Ok(());
    }
    let installables: Vec<_> = paths.iter().map(|p| format!("{}^*", p.as_str())).collect();
    let mut arguments = vec![
        "build",
        "--offline",
        "--no-substitute",
        "--no-link",
        "--json",
    ];
    arguments.extend(installables.iter().map(String::as_str));
    let output = command(tool, root, arguments)?;
    let built: Vec<Built> = serde_json::from_slice(&output).map_err(super::Error::from)?;
    if built.len() != paths.len()
        || built.iter().any(|b| b.outputs.is_empty())
        || !built
            .into_iter()
            .map(|b| b.drv_path)
            .collect::<BTreeSet<_>>()
            .eq(paths)
    {
        return Err(Error::Residual);
    }
    Ok(())
}

fn command(tool: &Path, root: &Path, mut args: Vec<&str>) -> Result<Vec<u8>, Error> {
    process::policy(None, &mut args);
    let mut command = super::native_command(tool, &args);
    command.current_dir(root);
    Ok(super::query_output(super::invoke_command(
        command,
        args.first().copied(),
    )?)?)
}

#[derive(Debug, Display, Error)]
pub enum Error {
    /// root flake build ownership requires an exact publication selection
    Scope,
    /// native flake check failed: {0}
    Nix(#[from] super::Error),
    /// checked publication plan changed: {0}
    Plan(#[from] crate::provider::Error),
    /// exact root-flake handoff became stale: {0}
    Source(#[from] publication::Error),
    /// root flake realization did not return all requested recipes and outputs
    Residual,
    /// selected check {target:?} did not discharge root flake build {recipe:?}
    NotDischarged { target: Name, recipe: StorePath },
}

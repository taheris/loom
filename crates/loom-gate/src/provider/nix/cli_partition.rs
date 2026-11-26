//! Exact, publication-only native test partition for changes to the existing verifier CLI test.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use loom_driver::git::GitOid;
use nextest_metadata::TestListSummary;
use serde::Deserialize;

use super::{CargoOperation, Error, Graph, Name, Observed, Runtime, StorePath, report};
use crate::inventory::{
    Inventory,
    publication::{Change, Sources},
    snapshot::{ResourcePath, Snapshot},
};
use crate::scope::{Platform, Resolved};

#[cfg(test)]
#[path = "cli_partition_tests.rs"]
mod tests;

const INPUT: &str = "crates/loom-verification/tests/cli.rs";
const SYSTEM_CASE: &str = "loom::integration$native_system::native_system_scenarios_execute_actual_programs_in_canonical_store";

/// Not a verdict. Cannot be constructed from declared paths or caller-supplied case names.
pub struct Partition {
    sources: Sources,
    compiler: StorePath,
    complement: std::sync::Arc<Graph>,
    prior_output: Option<super::pin::Inputs>,
    selected: report::Contract,
    remainder: report::Contract,
}

#[derive(Clone, Copy)]
pub(super) enum Slice {
    Selected,
    Complement,
}

impl Slice {
    pub(super) const fn argument(self) -> &'static str {
        match self {
            Self::Selected => "selected",
            Self::Complement => "complement",
        }
    }
}

#[derive(Deserialize)]
struct Endpoint {
    reporter: StorePath,
    catalogue: StorePath,
    previous_catalogue_output: StorePath,
}

impl Partition {
    pub(in crate::provider) fn admit(
        snapshot: &Snapshot,
        scope: &Resolved,
        full: &Graph,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            cfg!(target_os = "macos")
                && scope.platform() == Platform::Native
                && full.is_workspace_test_suite()
                && full.runtime == Runtime::NativeStore,
            "not the canonical native-store suite"
        );
        let range = scope.diff().ok_or(Error::Result)?;
        let (prior, head) = range.split_once("..").ok_or(Error::Result)?;
        let sources = Sources::capture(snapshot, &GitOid::new(prior)?, &GitOid::new(head)?)?;
        let input = ResourcePath::try_from(std::path::PathBuf::from(INPUT))?;
        let cli_only = cli_only(&sources, &input);
        let system = crate::runner::result::current_platform();
        let selector = format!("nix build .#legacyPackages.{system}.loomCliComplement");
        let inventory = Inventory::for_nix_target(snapshot, Platform::Native, &selector)?;
        let complement = inventory
            .native_graph(&Name::try_from(format!(
                "nix:legacy:flake.nix:{system}:loomCliComplement"
            ))?)
            .ok_or(Error::Result)?;
        anyhow::ensure!(
            complement.is_workspace_test_suite()
                && complement.runtime == Runtime::NativeStore
                && !complement.origins().contains(&input)
                && !complement.evaluation_origins().contains(&input),
            "CLI file is still in the complement closure"
        );
        let full_catalogue = full
            .prepare_catalogue(Name::try_from("cli-partition-full".to_owned())?)?
            .require_verified()?;
        let complement_catalogue = complement
            .prepare_catalogue(Name::try_from("cli-partition-complement".to_owned())?)?
            .require_verified()?;
        let canonical = read_membership(full_catalogue.compiled_directory()?)?;
        let remainder = read_membership(complement_catalogue.compiled_directory()?)?;
        let selected = reconcile(&canonical, &remainder)?;
        let rest = canonical
            .keys()
            .filter(|name| !selected.contains(*name))
            .cloned()
            .collect();
        let prior = cli_only
            .then(|| prove_unchanged(full, complement, &sources, &system, &canonical))
            .and_then(|proof| match proof {
                Ok(pin) => Some(pin),
                Err(error) => {
                    tracing::debug!(%error, "CLI complement is still due");
                    None
                }
            });
        full_catalogue.require_current()?;
        complement_catalogue.require_current()?;
        sources.require_current()?;
        tracing::info!(
            selected = selected.len(),
            omit_complement = prior.is_some(),
            "admitted closed publication test suite"
        );
        let contract = |targets| report::Contract {
            parser: report::Parser::LibtestJson,
            targets,
            tier: report::Lane::Check,
        };
        Ok(Self {
            sources,
            compiler: full.catalogue_derivation().ok_or(Error::Result)?.clone(),
            complement: complement.clone(),
            prior_output: prior,
            selected: contract(selected),
            remainder: contract(rest),
        })
    }

    pub(in crate::provider) fn contains(&self, target: &Name) -> bool {
        self.prior_output.is_none() || self.selected.targets.contains(target)
    }

    pub(in crate::provider) fn require_current(&self) -> Result<(), Error> {
        self.sources
            .require_current()
            .map_err(|e| Error::Catalogue {
                detail: e.to_string(),
            })?;
        self.complement.require_current()
    }

    pub(in crate::provider) fn run(&self, graph: &Graph, target: Name) -> Result<Observed, Error> {
        self.require_current()?;
        let reporter = graph
            .nodes
            .get(graph.reporter.as_ref().ok_or(Error::Result)?)
            .ok_or(Error::Result)?;
        if graph.runtime != Runtime::NativeStore
            || graph.cargo != Some(CargoOperation::Test)
            || graph.catalogue_derivation() != Some(&self.compiler)
            || reporter.env.contains_key("LOOM_PUBLICATION_CLI_SLICE")
        {
            return Err(Error::GraphChanged);
        }
        let result = graph.run_store(target.clone(), &self.selected, Some(Slice::Selected))?;
        self.require_current()?;
        if result.report_members().is_none_or(|members| {
            members.len() != self.selected.targets.len()
                || members.values().any(|member| member.skipped)
        }) {
            return Err(Error::Report {
                detail: "CLI partition did not execute every required case".into(),
            });
        }
        if self.prior_output.is_some() {
            return Ok(result);
        }
        let remainder =
            self.complement
                .run_store(target, &self.remainder, Some(Slice::Complement))?;
        self.require_current()?;
        result
            .union_reports(&remainder)
            .map_err(|error| Error::Report {
                detail: error.to_string(),
            })
    }
}

fn cli_only(sources: &Sources, input: &ResourcePath) -> bool {
    if sources.changes().len() != 1 || sources.changes().get(input) != Some(&Change::Modified) {
        return false;
    }
    let (old, new) = (
        &sources.before().files()[Path::new(INPUT)],
        &sources.after().files()[Path::new(INPUT)],
    );
    old.bytes() != new.bytes() && old.is_executable() == new.is_executable()
}

fn prove_unchanged(
    full: &Graph,
    complement: &Graph,
    sources: &Sources,
    system: &str,
    canonical: &Cases,
) -> anyhow::Result<super::pin::Inputs> {
    let before = evaluate_prior(&full.tool, sources, system)?;
    anyhow::ensure!(
        complement.derivation() == &before.reporter
            && complement.catalogue_derivation() == Some(&before.catalogue),
        "prior and head complement recipes differ"
    );
    // A registered compiler output is immutable input metadata, never a prior verdict.
    let pin = super::pin::Inputs::acquire(
        &full.tool,
        &BTreeSet::from([before.previous_catalogue_output.clone()]),
    )?;
    super::query(
        &full.tool,
        &[
            "store",
            "verify",
            "--no-trust",
            "--offline",
            before.previous_catalogue_output.as_str(),
        ],
    )?;
    let prior = read_membership(Path::new(before.previous_catalogue_output.as_str()))?;
    anyhow::ensure!(
        &prior == canonical,
        "canonical terminal membership or filtering changed"
    );
    Ok(pin)
}

fn evaluate_prior(tool: &Path, sources: &Sources, system: &str) -> anyhow::Result<Endpoint> {
    use std::os::unix::fs::PermissionsExt as _;
    let stage = tempfile::tempdir()?;
    for (relative, entry) in sources.before().files() {
        let destination = stage.path().join(relative);
        std::fs::create_dir_all(destination.parent().ok_or(Error::Result)?)?;
        let mut file = std::fs::File::create_new(&destination)?;
        std::io::Write::write_all(&mut file, entry.bytes())?;
        if entry.is_executable() {
            file.set_permissions(std::fs::Permissions::from_mode(0o755))?;
        }
    }
    let reference = format!(
        "path:{}#.legacyPackages.{system}.loomCliProof",
        stage.path().canonicalize()?.display()
    );
    let mut args = vec![
        "eval",
        "--json",
        "--offline",
        "--no-substitute",
        "--no-update-lock-file",
        "--no-write-lock-file",
        "--option",
        "allow-import-from-derivation",
        "false",
        &reference,
    ];
    super::process::policy(None, &mut args);
    Ok(serde_json::from_slice(&super::query(tool, &args)?)?)
}

#[derive(Debug, PartialEq, Eq)]
struct Membership {
    binary: String,
    ignored: bool,
    runnable: bool,
    default: bool,
}

type Cases = BTreeMap<Name, Membership>;

fn read_membership(directory: &Path) -> anyhow::Result<Cases> {
    let all: TestListSummary =
        serde_json::from_slice(&std::fs::read(directory.join("nextest.json"))?)?;
    let default: TestListSummary =
        serde_json::from_slice(&std::fs::read(directory.join("default.json"))?)?;
    let mut result = BTreeMap::new();
    for (binary, suite) in all.rust_suites {
        let default = default.rust_suites.get(&binary).ok_or(Error::Result)?;
        for (case, summary) in suite.test_cases {
            let name = Name::try_from(format!(
                "{}::{}${case}",
                suite.package_name, suite.binary.binary_name
            ))?;
            let default = default
                .test_cases
                .get(&case)
                .is_some_and(|case| case.filter_match.is_match());
            let runnable = summary.filter_match.is_match()
                && summary.kind == Some(nextest_metadata::RustTestKind::TEST);
            anyhow::ensure!(
                result
                    .insert(
                        name,
                        Membership {
                            binary: binary.to_string(),
                            ignored: summary.ignored,
                            runnable,
                            default
                        }
                    )
                    .is_none(),
                "ambiguous nextest identity"
            );
        }
    }
    Ok(result)
}

fn reconcile(canonical: &Cases, complement: &Cases) -> anyhow::Result<BTreeSet<Name>> {
    let mut selected = BTreeSet::new();
    let mut cli = false;
    let mut system = false;
    for (name, membership) in canonical {
        let is_cli = name.as_str().starts_with("loom-verification::cli$");
        let is_system = name.as_str() == SYSTEM_CASE;
        let observed = complement.get(name);
        anyhow::ensure!(
            (is_cli && observed.is_none()) || (!is_cli && observed == Some(membership)),
            "complement does not reconcile case {name:?}"
        );
        if is_cli || is_system {
            anyhow::ensure!(
                membership.runnable && !membership.ignored,
                "selected case is filtered or ignored"
            );
            selected.insert(name.clone());
            cli |= is_cli;
            system |= is_system;
        }
    }
    anyhow::ensure!(
        cli && system && complement.keys().all(|name| canonical.contains_key(name)),
        "incomplete/extra case partition"
    );
    Ok(selected)
}

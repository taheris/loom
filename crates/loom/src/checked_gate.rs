//! The ordinary CLI loads executable definitions, never authority from input-list JSON.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Context as _;
use loom_gate::inventory::snapshot::{ResourcePath, Snapshot};
use loom_gate::inventory::{Inventory, Name};
use loom_gate::provider::plan::{Plan, property};
use loom_gate::provider::{Definition, Registry, Unit};
use loom_gate::scope::Resolved;
use loom_gate::{Annotation, InputResolver, Tier};

/// Eligible owners retained by selection, not recovered from all annotations.
pub struct Selected<'a> {
    pub unit: &'a Unit,
    pub owners: Vec<Annotation>,
    pub baseline: bool,
}

/// Exact-target inspection is intentionally not an inventory-wide admission.
/// Neither variant is verification evidence or publication authority.
pub enum Admission {
    Whole(Plan, Vec<property::Report>),
    Diagnostic {
        snapshot: Snapshot,
        units: Vec<Unit>,
        properties: Vec<property::Report>,
    },
}

impl Admission {
    /// Compile permission from the same captured configuration as admission.
    pub fn skip_policies(&self) -> anyhow::Result<loom_gate::runner::policy::Policies> {
        self.require_current()?;
        let snapshot = match self {
            Self::Whole(plan, _) => plan.snapshot(),
            Self::Diagnostic { snapshot, .. } => snapshot,
        };
        captured_skip_policies(snapshot)
    }

    pub fn load(workspace: &Path, scope: &Resolved) -> anyhow::Result<Self> {
        let specs = workspace.join("specs");
        let declarations = if specs.try_exists()? {
            loom_gate::annotation::parse(&specs)?.annotations
        } else {
            Vec::new()
        };
        let declarations: Vec<_> = declarations
            .into_iter()
            .filter(|annotation| {
                scope
                    .target()
                    .is_none_or(|target| annotation.target == target)
            })
            .collect();
        structural_integrity(workspace, &declarations)?;
        // Reject already-known unsupported definitions before native discovery. A
        // native-shaped target is only deferred here, never admitted by its text.
        let unsupported_definitions: Vec<_> = declarations
            .iter()
            .filter(|annotation| {
                annotation.tier != Tier::Judge
                    && annotation.tier != Tier::Test
                    && !annotation.pending
                    && annotation.target != loom_gate::provider::PROJECT_CONTEXT_TARGET
                    && logical_walk(&annotation.target).is_none()
                    && !annotation.target.starts_with("nix build ")
                    && !annotation.target.starts_with("nix report ")
            })
            .map(|annotation| unsupported(annotation).to_string())
            .collect();
        if !unsupported_definitions.is_empty() {
            anyhow::bail!("{}", unsupported_definitions.join("\n"));
        }
        let snapshot =
            Snapshot::discover(workspace).context("capture current verification inputs")?;
        require_explicit_inputs(&snapshot, scope)?;
        captured_skip_policies(&snapshot)?;
        let inventory = Inventory::discover(&snapshot)
            .context("discover independent native verification obligations")?;
        let annotations: Vec<_> = declarations
            .into_iter()
            .filter(|annotation| annotation.tier != Tier::Judge)
            .collect();
        let (definitions, properties) = definitions(
            &snapshot,
            &inventory,
            &annotations,
            scope.target().is_none(),
        )?;
        let registered: BTreeSet<_> = definitions
            .iter()
            .map(|definition| definition.description().target().as_str().to_owned())
            .collect();
        let registry = Registry::new(&snapshot, definitions)?;
        if scope.target().is_none() {
            return Ok(Self::Whole(
                registry.admit(&snapshot, &inventory)?,
                properties,
            ));
        }
        let mut owned_contracts = BTreeMap::new();
        let targets: BTreeSet<_> = annotations
            .iter()
            .filter(|annotation| registered.contains(&annotation.target))
            .map(|annotation| &annotation.target)
            .collect();
        for target in targets {
            let target = Name::try_from(target.clone())?;
            let contracts = annotations
                .iter()
                .filter(|annotation| annotation.target == target.as_str())
                .map(|annotation| {
                    let path = annotation
                        .source_spec
                        .strip_prefix(workspace)
                        .unwrap_or(&annotation.source_spec)
                        .to_path_buf();
                    Ok(ResourcePath::try_from(path)?)
                })
                .collect::<anyhow::Result<BTreeSet<_>>>()?;
            owned_contracts.insert(target, contracts);
        }
        let units = registry.resolve_owned(&snapshot, &inventory, &owned_contracts)?;
        snapshot.require_current()?;
        Ok(Self::Diagnostic {
            snapshot,
            units,
            properties,
        })
    }

    pub const fn is_diagnostic(&self) -> bool {
        matches!(self, Self::Diagnostic { .. })
    }

    /// Recheck every admitted sibling, including unselected later lanes, before any effect.
    pub fn require_current(&self) -> anyhow::Result<()> {
        match self {
            Self::Whole(plan, _) => {
                // from_plan validates all siblings without executing any callback.
                InputResolver::from_plan(plan)?;
            }
            Self::Diagnostic { snapshot, .. } => snapshot.require_current()?,
        }
        Ok(())
    }

    /// Mint consumes the same admitted callbacks, never its legacy raw command dispatcher.
    pub fn mint_failures(
        &self,
        scope: &Resolved,
    ) -> anyhow::Result<Vec<loom_workflow::mint::VerifierFailure>> {
        self.require_current()?;
        let mut failures = Vec::new();
        match self {
            Self::Whole(plan, _) => {
                let selection = plan.select(scope)?;
                for invocation in selection.invocations()? {
                    let invocation = invocation?;
                    failures.extend(mint_observation(
                        invocation.observed(),
                        invocation.owners(),
                        invocation.baseline_tier().is_some(),
                    )?);
                }
            }
            Self::Diagnostic { units, .. } => {
                for unit in units {
                    self.require_current()?;
                    failures.extend(mint_observation(&unit.run()?, unit.annotations(), false)?);
                }
            }
        }
        Ok(failures)
    }

    pub fn explain(&self, scope: &Resolved, tiers: &[Tier]) -> anyhow::Result<Option<String>> {
        match self {
            Self::Whole(plan, properties) => {
                #[derive(serde::Serialize)]
                struct Explained<'a, T> {
                    #[serde(flatten)]
                    plan: T,
                    native_properties: &'a [property::Report],
                }
                let selection = plan.select_tiers(scope, tiers)?;
                let explanation = selection.explain()?;
                tracing::debug!(
                    target: "loom::gate::plan",
                    detail = %serde_json::to_string(&Explained {
                        plan: &explanation,
                        native_properties: properties,
                    })?,
                    "complete admitted verification plan"
                );
                Ok(Some(serde_json::to_string(&Explained {
                    plan: explanation.summary(),
                    native_properties: properties,
                })?))
            }
            Self::Diagnostic { properties, .. } if !properties.is_empty() => {
                #[derive(serde::Serialize)]
                struct Classified<'a> {
                    native_properties: &'a [property::Report],
                }
                Ok(Some(serde_json::to_string(&Classified {
                    native_properties: properties,
                })?))
            }
            Self::Diagnostic { .. } => Ok(None),
        }
    }

    pub fn select<'a>(
        &'a self,
        scope: &Resolved,
        tiers: &[Tier],
    ) -> anyhow::Result<Vec<Selected<'a>>> {
        self.require_current()?;
        match self {
            Self::Diagnostic { units, .. } => Ok(units
                .iter()
                .filter(|unit| {
                    unit.annotations()
                        .iter()
                        .any(|owner| tiers.contains(&owner.tier))
                })
                .map(|unit| Selected {
                    unit,
                    baseline: false,
                    owners: unit
                        .annotations()
                        .iter()
                        .filter(|owner| tiers.contains(&owner.tier))
                        .cloned()
                        .collect(),
                })
                .collect()),
            Self::Whole(plan, _) => Ok(plan
                .select_tiers(scope, tiers)?
                .decisions()
                .iter()
                .filter(|decision| decision.reason().selected())
                .map(|decision| Selected {
                    unit: decision.unit(),
                    owners: decision.owners().to_vec(),
                    baseline: decision.baseline_tier().is_some(),
                })
                .collect()),
        }
    }
}

fn require_explicit_inputs(snapshot: &Snapshot, scope: &Resolved) -> anyhow::Result<()> {
    if !scope.is_explicit_files() {
        return Ok(());
    }
    for path in scope.files().unwrap_or_default() {
        let relative = path.strip_prefix(snapshot.root()).with_context(|| {
            format!(
                "explicit verification input `{}` is outside the captured workspace",
                path.display()
            )
        })?;
        let relative = ResourcePath::try_from(relative.to_path_buf())?;
        anyhow::ensure!(
            snapshot
                .files()
                .keys()
                .any(|captured| captured.as_path().starts_with(relative.as_path())),
            "explicit verification input `{}` is absent from the captured workspace; correct the path or use a deletion-aware --diff scope",
            path.display()
        );
    }
    Ok(())
}

fn captured_skip_policies(
    snapshot: &Snapshot,
) -> anyhow::Result<loom_gate::runner::policy::Policies> {
    use loom_driver::config::LoomConfig;
    let configured = LoomConfig::resolve_path(snapshot.root());
    let configured = if configured.is_absolute() {
        configured
    } else {
        snapshot.root().join(configured)
    };
    let relative = configured
        .strip_prefix(snapshot.root())
        .context("skip-policy configuration must be part of the captured workspace")?;
    let path = ResourcePath::try_from(relative.to_path_buf())?;
    let config = if snapshot.files().contains_key(&path) {
        LoomConfig::from_toml_str(snapshot.text(&path)?)?
    } else {
        LoomConfig::default()
    };
    Ok(loom_gate::runner::policy::Policies::compile(&config)?)
}

fn mint_observation(
    observed: &loom_gate::provider::tracked::Observed,
    owners: &[Annotation],
    baseline: bool,
) -> anyhow::Result<Vec<loom_workflow::mint::VerifierFailure>> {
    use loom_gate::provider::tracked::Outcome;
    use loom_workflow::mint::{VerifierFailure, VerifierFailureKind};

    let evidence = serde_json::to_string(observed)?;
    if matches!(
        observed.outcome(),
        Outcome::Skipped { .. } | Outcome::Unavailable { .. } | Outcome::Inapplicable { .. }
    ) || observed
        .verdict()
        .is_some_and(|verdict| verdict.producer_error.is_some())
    {
        anyhow::bail!("checked native verification remains unverified: {evidence}");
    }
    if (baseline || owners.is_empty()) && !matches!(observed.outcome(), Outcome::Pass) {
        anyhow::bail!("independent native baseline failed: {evidence}");
    }
    Ok(owners
        .iter()
        .filter_map(|annotation| {
            let kind = match (annotation.pending, observed.outcome()) {
                (true, Outcome::Pass) => VerifierFailureKind::UnneededPendingMarker,
                (false, Outcome::Fail { .. }) => VerifierFailureKind::Failed,
                _ => return None,
            };
            Some(VerifierFailure {
                annotation: annotation.clone(),
                kind,
                evidence: evidence.clone(),
            })
        })
        .collect())
}

/// Command resolution comes from the actual registry, never argv[0] on PATH.
/// Test bodies and judge links retain their independent structural checks.
fn structural_integrity(workspace: &Path, annotations: &[Annotation]) -> anyhow::Result<()> {
    struct NoCommandProbes;
    impl loom_gate::integrity::PendingCommandExecutor for NoCommandProbes {
        fn executes_zero(&self, _annotation: &Annotation) -> bool {
            false
        }
    }
    let mut findings = loom_gate::integrity::check_atomic_acceptance(annotations);
    let structural: Vec<_> = annotations
        .iter()
        .filter(|annotation| matches!(annotation.tier, Tier::Test | Tier::Judge))
        .cloned()
        .collect();
    if !structural.is_empty() {
        let (tests, stubs) = loom_gate::integrity::scan_workspace_pair(workspace)?;
        findings.extend(loom_gate::integrity::check_forward(
            &structural,
            &[],
            workspace,
            &loom_gate::FsCommandResolver::new(workspace),
            &tests,
            &stubs,
            &NoCommandProbes,
        ));
    }
    if !findings.is_empty() {
        anyhow::bail!(
            "loom gate [integrity]: checked provider structural integrity failed before dispatch:\n{}",
            findings
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    Ok(())
}

fn definitions(
    snapshot: &Snapshot,
    inventory: &Inventory,
    annotations: &[Annotation],
    include_baseline: bool,
) -> anyhow::Result<(Vec<Definition>, Vec<property::Report>)> {
    // Native checks and compiler-enumerated Rust cases have fixed executions.
    // Shell wrappers, build arguments and runner templates are not bridges.
    let native: BTreeMap<_, _> = inventory
        .subjects()
        .values()
        .filter_map(|subject| {
            let binding = subject.nix_target()?;
            if binding.kind() == loom_gate::inventory::nix::Kind::App {
                return Some((subject.name().as_str().to_owned(), subject.name().clone()));
            }
            let namespace = binding.kind().namespace();
            let directory = binding.flake().as_path().parent()?;
            let reference = if directory.as_os_str().is_empty() {
                ".".to_owned()
            } else {
                format!("./{}", directory.display())
            };
            Some((
                format!(
                    "nix build {reference}#{namespace}.{}.{}",
                    binding.system().as_str(),
                    binding.attribute().as_str()
                ),
                subject.name().clone(),
            ))
        })
        .collect();
    let package_aliases: BTreeMap<_, _> = native
        .iter()
        .filter_map(|(target, subject)| {
            inventory.native_graph(subject)?;
            let (reference, qualified) = target.split_once("#packages.")?;
            let (_, attribute) = qualified.split_once('.')?;
            Some((format!("{reference}#{attribute}"), target.clone()))
        })
        .collect();
    let mut aliases = BTreeMap::<String, BTreeSet<Name>>::new();
    let mut targets = BTreeMap::<Name, BTreeSet<Name>>::new();
    let mut native_targets = BTreeSet::new();
    let mut rust_targets = BTreeMap::<Name, bool>::new();
    let mut report_targets = BTreeMap::<Name, bool>::new();
    for annotation in annotations {
        if annotation.target == loom_gate::provider::PROJECT_CONTEXT_TARGET {
            continue;
        }
        if let Some(logical) = logical_walk(&annotation.target) {
            targets
                .entry(Name::try_from(logical.to_owned())?)
                .or_default()
                .insert(Name::try_from(annotation.target.clone())?);
        } else if native.contains_key(&annotation.target) {
            native_targets.insert(annotation.target.clone());
        } else if let Some(canonical) = package_aliases.get(&annotation.target) {
            native_targets.insert(canonical.clone());
            aliases
                .entry(canonical.clone())
                .or_default()
                .insert(Name::try_from(annotation.target.clone())?);
        } else if annotation.tier == Tier::Test {
            rust_targets
                .entry(Name::try_from(annotation.target.clone())?)
                .and_modify(|pending| *pending &= annotation.pending)
                .or_insert(annotation.pending);
        } else {
            report_targets
                .entry(Name::try_from(annotation.target.clone())?)
                .and_modify(|pending| *pending &= annotation.pending)
                .or_insert(annotation.pending);
        }
    }
    let logical: Vec<_> = targets.keys().cloned().collect();
    let checked = loom_walk::walk::checked_definitions(snapshot, inventory, &logical)?;
    let mut definitions = Vec::new();
    for (logical, definition) in logical.into_iter().zip(checked) {
        for target in &targets[&logical] {
            definitions.push(definition.clone().named(target.clone())?);
        }
    }
    if include_baseline
        || annotations
            .iter()
            .any(|annotation| annotation.target == loom_gate::provider::PROJECT_CONTEXT_TARGET)
    {
        definitions.extend(Definition::project_context(snapshot, inventory)?);
    }
    let requested_native = native_targets.clone();
    if include_baseline {
        native_targets.extend(native.keys().cloned());
    }
    let mut reported = BTreeSet::new();
    for (target, subject) in &native {
        if !requested_native.contains(target)
            && definitions
                .iter()
                .any(|definition| definition.native_applicability_covers(subject))
        {
            continue;
        }
        let mut references = if let Some(reference) = target.strip_prefix("nix build ") {
            BTreeSet::from([reference])
        } else {
            BTreeSet::new()
        };
        for (alias, canonical) in &package_aliases {
            if canonical == target {
                references.insert(
                    alias
                        .strip_prefix("nix build ")
                        .context("invalid native package alias")?,
                );
            }
        }
        let requested_report = references.iter().any(|reference| {
            let prefix = format!("nix report {reference}::");
            report_targets
                .keys()
                .any(|name| name.as_str().starts_with(&prefix))
        });
        if !native_targets.contains(target) && !requested_report {
            continue;
        }
        let definition = Definition::hermetic_baseline(
            snapshot,
            inventory,
            Name::try_from(target.clone())?,
            subject,
        )?;
        for case in definition.reported_cases()? {
            for reference in &references {
                let address = Name::try_from(format!(
                    "nix report {reference}::{}",
                    case.description().target().as_str()
                ))?;
                if report_targets.contains_key(&address) {
                    anyhow::ensure!(
                        reported.insert(address.clone()),
                        "ambiguous native report identity `{}`",
                        address.as_str()
                    );
                    definitions.push(case.clone().named(address)?);
                }
            }
        }
        if native_targets.contains(target) {
            if let Some(names) = aliases.get(target) {
                for name in names {
                    definitions.push(definition.clone().named(name.clone())?);
                }
            }
            definitions.push(definition);
        }
    }
    for (target, pending) in &report_targets {
        if !pending && !reported.contains(target) {
            let annotation = annotations
                .iter()
                .find(|annotation| annotation.target == target.as_str())
                .context("missing native report owner")?;
            return Err(unsupported(annotation));
        }
    }
    if include_baseline
        && (!rust_targets.is_empty() || !property::declarations(snapshot)?.is_empty())
    {
        Registry::new(snapshot, definitions.clone())?
            .require_inventory_coverage(snapshot, inventory)?;
    }
    let (rust, properties) = rust_definitions(snapshot, inventory, annotations, &rust_targets)?;
    definitions.extend(rust);
    Ok((definitions, properties))
}

fn rust_definitions(
    snapshot: &Snapshot,
    inventory: &Inventory,
    annotations: &[Annotation],
    requested: &BTreeMap<Name, bool>,
) -> anyhow::Result<(Vec<Definition>, Vec<property::Report>)> {
    use loom_gate::provider::nix::Error;
    let declarations = property::declarations(snapshot)?;
    if requested.is_empty() && declarations.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let suites = Definition::native_rust_suites(snapshot, inventory)?;
    let mut matched = BTreeMap::new();
    let mut definitions = Vec::new();
    let mut properties = Vec::new();
    let mut classified = BTreeSet::new();
    for (index, suite) in suites.iter().enumerate() {
        let catalogue = suite
            .nix_graph()
            .context("missing checked native compiler graph")?
            .prepare_catalogue(suite.description().target().clone())?
            .require_verified()?;
        let mut requests = Vec::new();
        for (declaration, (source, filter)) in declarations.iter().enumerate() {
            if property_owner(inventory, &source.document) == Some(suite.description().origin()) {
                anyhow::ensure!(
                    classified.insert(declaration),
                    "property filter {}:{} has ambiguous native compiler contexts",
                    source.document.display(),
                    source.line
                );
                requests.push(property::Request {
                    source,
                    filter,
                    applicability: property::Applicability::Applicable,
                });
            }
        }
        if !requests.is_empty() {
            properties.push(catalogue.partition(&requests)?.report());
        }
        let cases = catalogue.lookup(&requested.keys().cloned().collect())?;
        let mut targets = BTreeSet::new();
        for (target, pending) in requested {
            let Some(case) = cases.get(target) else {
                continue;
            };
            if *pending && !case.runnable() {
                continue;
            }
            if matched.insert(target.clone(), index).is_some() {
                return Err(Error::AmbiguousCase {
                    target: target.clone(),
                }
                .into());
            }
            targets.insert(target.clone());
        }
        definitions.extend(suite.native_rust_cases(snapshot, inventory, &catalogue, &targets)?);
    }
    for (target, pending) in requested {
        if !pending && !matched.contains_key(target) {
            let annotation = annotations
                .iter()
                .find(|annotation| annotation.target == target.as_str())
                .context("missing native test owner")?;
            return Err(unsupported(annotation));
        }
    }
    anyhow::ensure!(
        classified.len() == declarations.len(),
        "property strategy has no actual native Cargo/Nextest context; construct its compiler catalogue and retry `loom gate verify`"
    );
    Ok((definitions, properties))
}

fn property_owner<'a>(inventory: &'a Inventory, document: &Path) -> Option<&'a ResourcePath> {
    inventory
        .subjects()
        .values()
        .filter_map(|subject| subject.nix_target())
        .map(loom_gate::inventory::nix::Target::flake)
        .filter(|flake| {
            flake
                .as_path()
                .parent()
                .is_some_and(|directory| document.starts_with(directory))
        })
        .max_by_key(|flake| flake.as_path().components().count())
}

fn logical_walk(target: &str) -> Option<&str> {
    loom_walk::selection::walk_name(target)
}

fn unsupported(annotation: &Annotation) -> anyhow::Error {
    anyhow::anyhow!(
        "loom gate [integrity]: {}:{}: annotation [{}]({}) does not resolve to a supported execution: no actual checked definition; input-list JSON and runner commands cannot authorize execution; register a supported native definition and retry `loom gate verify`",
        annotation.source_spec.display(),
        annotation.line,
        annotation.tier,
        annotation.target,
    )
}

#[cfg(test)]
mod tests {
    use super::logical_walk;

    #[test]
    fn registered_walk_aliases_accept_markdown_whitespace_not_extra_arguments() {
        for target in [
            "cargo run -p loom-walk -- template_wire_format_restatement",
            "cargo run -p loom-walk\n  -- template_wire_format_restatement",
            "cargo run -p loom-walk --\n  template_wire_format_restatement",
            "template_wire_format_restatement",
        ] {
            assert_eq!(
                logical_walk(target),
                Some("template_wire_format_restatement")
            );
        }
        for target in [
            "cargo run -p loom-walk -- missing_walk",
            "cargo run -p loom-walk -- template_wire_format_restatement extra",
            "cargo run -p loom-walk -- template_wire_format_restatement; true",
            "cargo run -p loom-walk -- template_wire_format_restatement && true",
            "cargo run --offline -p loom-walk -- template_wire_format_restatement",
        ] {
            assert_eq!(logical_walk(target), None);
        }
    }
}

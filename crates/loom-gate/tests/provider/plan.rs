use std::collections::BTreeSet;

use super::{check, context, definition, fixture, name, path, write};
use loom_gate::inventory::{Duty, Inventory};
use loom_gate::provider::{Definition, Error, Registry};

fn package(dir: &tempfile::TempDir) {
    write(dir.path(), "specs/example/spec.md", "# Example contract\n");
    write(
        dir.path(),
        "docs/README.md",
        "- [example](../specs/example/spec.md)\n",
    );
}

#[test]
fn missing_native_baseline_coverage_rejects_an_otherwise_resolved_document_provider() {
    let dir = fixture();
    package(&dir);
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Heading [check](heading)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(
        &snapshot,
        vec![definition(&snapshot, &inventory, "heading", check)],
    )
    .unwrap();
    let Err(error) = registry.admit(&snapshot, &inventory) else {
        panic!("uncovered native baseline admitted");
    };
    assert!(matches!(
        error,
        Error::Coverage {
            duty: Duty::Build,
            ..
        }
    ));
    let message = error.to_string();
    assert!(message.contains("cargo:provider-fixture:lib:provider_fixture"));
    assert!(message.contains("Cargo.toml"));
    assert!(message.contains("loom gate verify"));
}

#[test]
fn omitted_registered_target_is_reported_with_its_acceptance_source_before_dispatch() {
    let dir = fixture();
    package(&dir);
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(
        &snapshot,
        vec![definition(&snapshot, &inventory, "heading", check)],
    )
    .unwrap();
    let Err(error) = registry.admit(&snapshot, &inventory) else {
        panic!("missing sibling admitted");
    };
    assert!(
        matches!(error, Error::Admission { target, contract, .. } if target == name("first") && contract == path("specs/example/tests.md"))
    );
}

#[test]
fn test_tier_resolves_current_checked_definition_not_a_cargo_input_assertion() {
    let dir = fixture();
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Heading [test](heading)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let definition = definition(&snapshot, &inventory, "heading", check);
    let candidate = definition.description().clone();
    let registry = Registry::new(&snapshot, vec![definition]).unwrap();
    let unit = registry
        .resolve(
            &snapshot,
            &inventory,
            &candidate,
            path("specs/example/tests.md"),
        )
        .unwrap();
    assert_eq!(unit.annotations()[0].tier, loom_gate::Tier::Test);
    assert_eq!(
        unit.run().unwrap().outcome(),
        &loom_gate::provider::tracked::Outcome::Pass
    );
}

fn offered(
    registry_inventory: &Inventory,
    snapshot: &loom_gate::inventory::snapshot::Snapshot,
) -> Definition {
    Definition::tracked(
        snapshot,
        registry_inventory,
        name("accounting"),
        path("src/lib.rs"),
        &registry_inventory
            .subjects()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        &registry_inventory
            .obligations()
            .iter()
            .map(loom_gate::inventory::Obligation::duty)
            .collect(),
        check,
    )
    .unwrap()
}

#[test]
// This checks responsibility accounting, not semantic adequacy of a checker body.
fn complete_explicit_responsibility_joins_all_current_inventory_duties_before_selection() {
    let dir = fixture();
    package(&dir);
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Account current responsibilities [check](accounting)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(&snapshot, vec![offered(&inventory, &snapshot)]).unwrap();
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    assert_eq!(plan.units().len(), 1);
    assert_eq!(
        plan.units()[0].responsibilities().len(),
        inventory.obligations().len()
    );
}

#[test]
fn unannotated_registration_cannot_supply_baseline_coverage() {
    let dir = fixture();
    package(&dir);
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Heading [check](heading)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(
        &snapshot,
        vec![
            offered(&inventory, &snapshot),
            definition(&snapshot, &inventory, "heading", check),
        ],
    )
    .unwrap();
    assert!(matches!(
        registry.admit(&snapshot, &inventory),
        Err(Error::Coverage { .. })
    ));
}

#[test]
fn new_unoffered_document_cannot_inherit_a_previous_inventory_plan() {
    let dir = fixture();
    package(&dir);
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Account current responsibilities [check](accounting)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let old_subjects = inventory
        .subjects()
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    write(dir.path(), "new.md", "# A new document\n");
    let (current, current_inventory) = context(&dir);
    let definition = Definition::tracked(
        &current,
        &current_inventory,
        name("accounting"),
        path("src/lib.rs"),
        &old_subjects,
        &current_inventory
            .obligations()
            .iter()
            .map(loom_gate::inventory::Obligation::duty)
            .collect(),
        check,
    )
    .unwrap();
    let registry = Registry::new(&current, vec![definition]).unwrap();
    assert!(
        matches!(registry.admit(&current, &current_inventory), Err(Error::Coverage { subject, duty: Duty::Document, .. }) if subject == name("resource:new.md"))
    );
    assert!(snapshot.require_current().is_err());
}

#[test]
fn shared_targets_keep_every_canonical_acceptance_owner() {
    let dir = fixture();
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- First contract [check](accounting)\n",
    );
    write(dir.path(), "specs/other/spec.md", "# Other contract\n");
    write(
        dir.path(),
        "specs/other/tests.md",
        "## Success Criteria\n- Second contract [check](accounting)\n",
    );
    write(
        dir.path(),
        "docs/README.md",
        "- [example](../specs/example/spec.md)\n- [other](../specs/other/spec.md)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(&snapshot, vec![offered(&inventory, &snapshot)]).unwrap();
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    assert_eq!(plan.units().len(), 1);
    let unit = &plan.units()[0];
    assert_eq!(unit.annotations().len(), 2);
    for contract in ["specs/example/tests.md", "specs/other/tests.md"] {
        assert!(unit.inputs().iter().any(|input| {
            input.path() == &path(contract)
                && input
                    .origins()
                    .contains(&loom_gate::provider::Origin::Contract(path(contract)))
        }));
    }
}

fn selectable_plan(dir: &tempfile::TempDir) -> loom_gate::provider::plan::Plan {
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Account responsibilities [check](accounting)\n- Heading [test](heading)\n",
    );
    let (snapshot, inventory) = context(dir);
    let registry = Registry::new(
        &snapshot,
        vec![
            offered(&inventory, &snapshot),
            definition(&snapshot, &inventory, "heading", check),
        ],
    )
    .unwrap();
    registry.admit(&snapshot, &inventory).unwrap()
}

#[test]
fn admitted_test_inputs_preserve_the_checked_closure_without_a_cargo_scope_or_helper() {
    let dir = fixture();
    let plan = selectable_plan(&dir);
    let unit = plan
        .units()
        .iter()
        .find(|unit| unit.target() == &name("heading"))
        .unwrap();
    let mut resolver = loom_gate::InputResolver::from_plan(&plan)
        .unwrap()
        .with_inputs_for_test_command("false".into());
    let annotation = &unit.annotations()[0];
    let (inputs, known) = resolver.resolve_with_provenance(annotation);
    assert!(known);
    assert_eq!(
        inputs.paths,
        unit.inputs()
            .iter()
            .map(|input| input.path().as_path().to_path_buf())
            .collect::<Vec<_>>()
    );
    assert!(inputs.paths.contains(&"src/lib.rs".into()));
    assert!(inputs.paths.contains(&"README.md".into()));
    assert!(inputs.paths.contains(&"specs/example/tests.md".into()));
    assert_eq!(
        resolver.probe_input_query(annotation),
        loom_gate::inputs::InputQueryProbe::Honoured
    );
}

#[tokio::test]
async fn ordinary_file_selection_consumes_admitted_test_inputs_before_dispatch() {
    let dir = fixture();
    let plan = selectable_plan(&dir);
    let scope = loom_gate::scope::Request::Files(vec!["unrelated.md".into()])
        .resolve(dir.path())
        .await
        .unwrap();
    let outcomes = plan.run_scoped(&scope).unwrap();
    assert_eq!(
        outcomes
            .iter()
            .map(|result| result.target().clone())
            .collect::<Vec<_>>(),
        vec![name("accounting")]
    );
    let scope = loom_gate::scope::Request::Files(vec!["README.md".into()])
        .resolve(dir.path())
        .await
        .unwrap();
    let outcomes = plan.run_scoped(&scope).unwrap();
    assert_eq!(
        outcomes
            .iter()
            .map(|result| result.target().clone())
            .collect::<Vec<_>>(),
        vec![name("accounting"), name("heading")]
    );
}

#[tokio::test]
async fn an_empty_finite_scope_does_not_execute_an_admitted_plan() {
    let dir = fixture();
    let plan = selectable_plan(&dir);
    let scope = loom_gate::scope::Request::Files(Vec::new())
        .resolve(dir.path())
        .await
        .unwrap();
    assert!(plan.run_scoped(&scope).unwrap().is_empty());
}

#[tokio::test]
async fn exact_target_selection_cannot_bypass_a_stale_unselected_sibling() {
    let dir = fixture();
    let plan = selectable_plan(&dir);
    let scope = loom_gate::scope::Request::Target("heading".into())
        .resolve(dir.path())
        .await
        .unwrap();
    write(
        dir.path(),
        "unrelated.md",
        "# Changed unselected responsibility\n",
    );
    assert!(matches!(plan.run_scoped(&scope), Err(Error::Snapshot(_))));
    assert!(matches!(
        loom_gate::InputResolver::from_plan(&plan),
        Err(Error::Snapshot(_))
    ));
}

#[test]
fn plan_invalidated_by_new_resource_cannot_dispatch_previously_checked_units() {
    let dir = fixture();
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Account responsibilities [check](accounting)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(&snapshot, vec![offered(&inventory, &snapshot)]).unwrap();
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    write(dir.path(), "new.md", "# A new responsibility\n");
    assert!(matches!(plan.run(), Err(Error::Snapshot(_))));
}

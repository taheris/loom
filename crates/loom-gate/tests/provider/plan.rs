#[path = "plan/property.rs"]
mod property;

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
fn fixed_library_context_reader_covers_document_policy_without_fabricating_rust_ownership() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success()
    );
    write(dir.path(), ".gitignore", "/.loom/\n");
    write(dir.path(), "README.md", "# A document\n");
    let snapshot = loom_gate::inventory::snapshot::Snapshot::discover(dir.path()).unwrap();
    let inventory = Inventory::discover(&snapshot).unwrap();
    let definitions = Definition::project_context(&snapshot, &inventory).unwrap();
    assert_eq!(definitions.len(), 1);
    let registry = Registry::new(&snapshot, definitions).unwrap();
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    assert_eq!(plan.units()[0].annotations(), []);
    assert!(
        plan.units()[0]
            .responsibilities()
            .iter()
            .any(|responsibility| responsibility.duty() == Duty::Document)
    );
    assert!(
        !plan.units()[0]
            .responsibilities()
            .iter()
            .any(|responsibility| matches!(
                responsibility.duty(),
                Duty::Build | Duty::Lint | Duty::Test
            ))
    );
    let observed = plan.run().unwrap();
    assert_eq!(
        observed[0].outcome(),
        &loom_gate::provider::tracked::Outcome::Pass
    );
    assert!(observed[0].observations().iter().any(|observation| matches!(
        observation, loom_gate::provider::tracked::Observation::Content { path: read, .. } if read == &path(".gitignore")
    )));
    write(dir.path(), "new-code.rs", "pub fn unowned() {}\n");
    assert!(plan.run().is_err());
    let changed = loom_gate::inventory::snapshot::Snapshot::discover(dir.path()).unwrap();
    assert!(matches!(
        Inventory::discover(&changed),
        Err(loom_gate::inventory::Error::Orphan { .. })
    ));
}

#[test]
fn fixed_context_reader_rejects_invalid_document_data_and_correction_requires_rediscovery() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success()
    );
    write(dir.path(), "README.md", "# Bad\0document\n");
    let snapshot = loom_gate::inventory::snapshot::Snapshot::discover(dir.path()).unwrap();
    let inventory = Inventory::discover(&snapshot).unwrap();
    let registry = Registry::new(
        &snapshot,
        Definition::project_context(&snapshot, &inventory).unwrap(),
    )
    .unwrap();
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    assert!(matches!(
        plan.run().unwrap()[0].outcome(),
        loom_gate::provider::tracked::Outcome::Fail { .. }
    ));
    write(dir.path(), "README.md", "# Corrected\n");
    assert!(plan.run().is_err());
    let snapshot = loom_gate::inventory::snapshot::Snapshot::discover(dir.path()).unwrap();
    let inventory = Inventory::discover(&snapshot).unwrap();
    let registry = Registry::new(
        &snapshot,
        Definition::project_context(&snapshot, &inventory).unwrap(),
    )
    .unwrap();
    assert_eq!(
        registry
            .admit(&snapshot, &inventory)
            .unwrap()
            .run()
            .unwrap()[0]
            .outcome(),
        &loom_gate::provider::tracked::Outcome::Pass
    );
}

#[test]
fn default_document_reader_does_not_reclassify_native_fixture_documents() {
    let dir = fixture();
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname='provider-fixture'\nversion='0.1.0'\nedition='2024'\n[[package.metadata.loom.verify.resource]]\nname='fixture:doc-data'\ndirectory='fixtures'\nrole='test-fixture'\nconsumers=['cargo:provider-fixture:lib:provider_fixture']\n",
    );
    write(dir.path(), "fixtures/malformed.md", "Invalid\0fixture\n");
    let (snapshot, inventory) = context(&dir);
    let definitions = Definition::project_context(&snapshot, &inventory).unwrap();
    assert!(
        definitions[0]
            .description()
            .inputs()
            .iter()
            .all(|input| input.path() != &path("fixtures/malformed.md"))
    );
    assert!(
        inventory
            .obligations()
            .iter()
            .any(
                |obligation| obligation.subject() == &name("fixture:doc-data")
                    && obligation.duty() == Duty::Test
            )
    );
}

#[test]
fn library_checker_description_is_not_a_registration_or_source_trust_flag() {
    let dir = fixture();
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Text [check](loom:project-context)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let definitions = Definition::project_context(&snapshot, &inventory).unwrap();
    let mut value = serde_json::to_value(definitions[0].description()).unwrap();
    value["execution"]["checker"]["trusted"] = serde_json::json!(true);
    assert!(serde_json::from_value::<loom_gate::provider::Candidate>(value).is_err());
    let mut value = serde_json::to_value(definitions[0].description()).unwrap();
    value["execution"]["checker"]["source"] = serde_json::json!("0".repeat(64));
    let forged: loom_gate::provider::Candidate = serde_json::from_value(value).unwrap();
    let registry = Registry::new(&snapshot, definitions).unwrap();
    assert!(matches!(
        registry.resolve(
            &snapshot,
            &inventory,
            &forged,
            path("specs/example/tests.md")
        ),
        Err(Error::Inconsistent { .. })
    ));
    assert!(matches!(
        registry.admit(&snapshot, &inventory),
        Err(Error::Coverage {
            duty: Duty::Build,
            ..
        })
    ));
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

pub fn offered(
    registry_inventory: &Inventory,
    snapshot: &loom_gate::inventory::snapshot::Snapshot,
) -> Definition {
    Definition::tracked(
        snapshot,
        registry_inventory,
        name("accounting"),
        path("src/lib.rs"),
        &registry_inventory
            .obligations()
            .iter()
            .map(|obligation| obligation.subject().clone())
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

#[tokio::test]
async fn native_baseline_without_any_acceptance_annotation_is_selected_and_dispatched() {
    let dir = fixture();
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Accounting [check](accounting)\n",
    );
    super::nix::native_project(&dir, "", true, true);
    let (snapshot, inventory) = context(&dir);
    let native = Definition::hermetic(
        &snapshot,
        &inventory,
        name("native-baseline"),
        &name("nix:check:flake.nix:x86_64-linux:heading"),
        &BTreeSet::from([name("resource:README.md")]),
        &BTreeSet::from([Duty::Document]),
    )
    .unwrap();
    let registry = Registry::new(&snapshot, vec![offered(&inventory, &snapshot), native]).unwrap();
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    let baseline = plan
        .units()
        .iter()
        .find(|unit| unit.target() == &name("native-baseline"))
        .unwrap();
    assert_eq!(baseline.annotations(), []);
    let scope = loom_gate::scope::Request::Files(vec!["README.md".into()])
        .resolve(dir.path())
        .await
        .unwrap();
    let executions = plan.select(&scope).unwrap().run().unwrap();
    let execution = executions
        .iter()
        .find(|execution| execution.observed().target() == &name("native-baseline"))
        .unwrap();
    assert_eq!(execution.owners(), []);
    match execution.observed().outcome() {
        loom_gate::provider::tracked::Outcome::Pass => {
            assert_ne!(execution.observed().observations(), []);
        }
        loom_gate::provider::tracked::Outcome::Unavailable {
            capability: loom_gate::provider::Capability::NixSandbox,
            detail,
        } => assert_ne!(detail, ""),
        outcome => panic!("unexpected native baseline outcome: {outcome:?}"),
    }
}

#[tokio::test]
async fn selected_lanes_precede_lexical_target_order_and_caller_lane_order() {
    // Selection model over actual linked fixture callbacks, not compiler facts.
    let dir = fixture();
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Heading [test](a-test)\n- Accounting [check](z-check)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(
        &snapshot,
        vec![
            offered(&inventory, &snapshot)
                .named(name("z-check"))
                .unwrap(),
            definition(&snapshot, &inventory, "a-test", check),
        ],
    )
    .unwrap();
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    let scope = loom_gate::scope::Request::Tree
        .resolve(dir.path())
        .await
        .unwrap();
    let selection = plan
        .select_tiers(&scope, &[loom_gate::Tier::Test, loom_gate::Tier::Check])
        .unwrap();
    let targets: Vec<_> = selection
        .decisions()
        .iter()
        .filter(|decision| decision.reason().selected())
        .map(|decision| decision.unit().target().as_str())
        .collect();
    assert_eq!(targets, ["z-check", "a-test"]);
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

#[test]
fn pending_targets_are_not_execution_registrations_or_baseline_offers() {
    let dir = fixture();
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Account responsibilities [check](accounting)\n- Not implemented [test?](unregistered)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(&snapshot, vec![offered(&inventory, &snapshot)]).unwrap();
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    assert_eq!(plan.units().len(), 1);
    assert_eq!(plan.units()[0].target(), &name("accounting"));
    assert_eq!(plan.pending().len(), 1);
    assert_eq!(plan.pending()[0].target, "unregistered");
}

#[test]
fn pending_only_callbacks_cannot_supply_independent_coverage() {
    let dir = fixture();
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Not implemented [check?](accounting)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(&snapshot, vec![offered(&inventory, &snapshot)]).unwrap();
    assert!(matches!(
        registry.require_inventory_coverage(&snapshot, &inventory),
        Err(Error::Coverage { .. })
    ));
    assert!(matches!(
        registry.admit(&snapshot, &inventory),
        Err(Error::Coverage { .. })
    ));
}

#[test]
fn registered_pending_targets_keep_real_owners_without_becoming_registration_flags() {
    let dir = fixture();
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Account responsibilities [check](accounting)\n- Implemented but still pending [test?](heading)\n",
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
    registry
        .require_inventory_coverage(&snapshot, &inventory)
        .unwrap();
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    let unit = plan
        .units()
        .iter()
        .find(|unit| unit.target() == &name("heading"))
        .unwrap();
    assert_eq!(unit.annotations().len(), 1);
    assert!(unit.annotations()[0].pending);
    assert_eq!(plan.pending().len(), 1);
    assert!(
        plan.run()
            .unwrap()
            .iter()
            .any(|observed| observed.target() == &name("heading"))
    );
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

fn inspect_runtime(
    context: &mut loom_gate::provider::tracked::Context<'_>,
) -> Result<loom_gate::provider::tracked::Outcome, Error> {
    let paths: Vec<_> = context.paths().cloned().collect();
    for input in paths {
        context.text(&input)?;
    }
    Ok(loom_gate::provider::tracked::Outcome::Pass)
}

#[tokio::test]
async fn independent_runtime_inputs_select_one_property_while_shared_executable_inputs_select_both()
{
    let dir = fixture();
    write(dir.path(), "property-one.md", "# First runtime fixture\n");
    write(dir.path(), "property-two.md", "# Second runtime fixture\n");
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Accounting [check](accounting)\n- One [test](property-one)\n- Two [test](property-two)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let mut definitions = vec![offered(&inventory, &snapshot)];
    for (target, subject) in [
        ("property-one", "resource:property-one.md"),
        ("property-two", "resource:property-two.md"),
    ] {
        definitions.push(
            Definition::tracked(
                &snapshot,
                &inventory,
                name(target),
                path("src/lib.rs"),
                &BTreeSet::from([name(subject)]),
                &BTreeSet::from([Duty::Document]),
                inspect_runtime,
            )
            .unwrap(),
        );
    }
    let registry = Registry::new(&snapshot, definitions).unwrap();
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    for (file, expected) in [
        ("property-one.md", vec![name("property-one")]),
        (
            "src/lib.rs",
            vec![name("property-one"), name("property-two")],
        ),
    ] {
        let scope = loom_gate::scope::Request::Files(vec![file.into()])
            .resolve(dir.path())
            .await
            .unwrap();
        let selected = plan
            .select_at(
                &scope,
                &[loom_gate::Tier::Test],
                loom_gate::provider::plan::selection::Stage::Feedback,
            )
            .unwrap();
        let executions = selected.run().unwrap();
        assert_eq!(
            executions
                .iter()
                .map(|execution| execution.observed().target().clone())
                .collect::<Vec<_>>(),
            expected
        );
        for execution in executions {
            assert_eq!(execution.shared_with(), None);
            assert!(execution.observed().observations().iter().any(|observation| matches!(observation,
                loom_gate::provider::tracked::Observation::Content { path: input, .. } if input.as_path() == std::path::Path::new(&format!("{}.md", execution.observed().target().as_str())))));
        }
    }
}

#[tokio::test]
async fn finite_selection_explains_exact_resources_and_preserves_cross_file_analysis() {
    use loom_gate::provider::plan::selection::Reason;
    let dir = fixture();
    let plan = selectable_plan(&dir);
    let scope = loom_gate::scope::Request::Files(vec!["README.md".into()])
        .resolve(dir.path())
        .await
        .unwrap();
    let selection = plan.select(&scope).unwrap();
    let heading = selection
        .decisions()
        .iter()
        .find(|decision| decision.unit().target() == &name("heading"))
        .unwrap();
    let Reason::Affected { inputs } = heading.reason() else {
        panic!("affected reason missing")
    };
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].path(), &path("README.md"));
    assert!(
        inputs[0]
            .origins()
            .contains(&loom_gate::provider::Origin::Subject(name(
                "resource:README.md"
            )))
    );
    assert!(
        selection
            .analysis_inputs()
            .any(|input| input == &path("src/lib.rs"))
    );
    let explained = serde_json::to_value(selection.explain().unwrap()).unwrap();
    let decisions = explained["decisions"].as_array().unwrap();
    let reported = decisions
        .iter()
        .find(|decision| decision["definition"]["contract"]["target"] == "heading")
        .unwrap();
    assert_eq!(reported["reason"]["reason"], "affected");
    assert_eq!(reported["reason"]["inputs"].as_array().unwrap().len(), 1);
    assert_eq!(reported["execution"]["kind"], "fresh-required");
    let completed = selection.run_report().unwrap();
    let invocations = completed.invocations();
    assert_eq!(invocations.len(), 2);
    let recorded = serde_json::to_value(&completed).unwrap();
    assert!(recorded["invocations"][0]["duration"].is_object());
    let invocation = invocations
        .iter()
        .find(|invocation| invocation.observed().target() == &name("heading"))
        .unwrap();
    assert_eq!(invocation.owners(), heading.unit().annotations());
    assert!(invocation.observed().observations().iter().any(|observation| matches!(observation,
        loom_gate::provider::tracked::Observation::Content { path: input, .. } if input == &path("README.md"))));
}

#[tokio::test]
async fn criterion_deferral_does_not_defer_an_eligible_sibling_sharing_its_execution() {
    use loom_gate::provider::plan::selection::Stage;
    let dir = fixture();
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Accounting [check](accounting)\n- Deferred [test](heading)\n  ```loom-verify\n  defer_until = \"publication\"\n  ```\n- Eligible [test](heading)\n",
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
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    let scope = loom_gate::scope::Request::Files(vec!["README.md".into()])
        .resolve(dir.path())
        .await
        .unwrap();
    let feedback = plan
        .select_at(&scope, &[loom_gate::Tier::Test], Stage::Feedback)
        .unwrap()
        .run()
        .unwrap();
    assert_eq!(feedback.len(), 1);
    assert_eq!(feedback[0].owners().len(), 1);
    let publication = plan
        .select_at(&scope, &[loom_gate::Tier::Test], Stage::Publication)
        .unwrap()
        .run()
        .unwrap();
    assert_eq!(publication.len(), 1);
    assert_eq!(publication[0].owners().len(), 2);
}

#[tokio::test]
async fn publication_deferral_preserves_tree_and_exact_target_diagnostics() {
    use loom_gate::provider::plan::selection::{Reason, Stage};
    let dir = fixture();
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Accounting [check](accounting)\n- Deferred [system](heading)\n  ```loom-verify\n  defer_until = \"publication\"\n  ```\n",
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
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    let scope = loom_gate::scope::Request::Files(vec!["README.md".into()])
        .resolve(dir.path())
        .await
        .unwrap();
    let feedback = plan
        .select_at(&scope, &[loom_gate::Tier::System], Stage::Feedback)
        .unwrap();
    assert!(
        feedback
            .decisions()
            .iter()
            .any(|decision| decision.reason() == &Reason::DeferredUntilPublication)
    );
    assert!(feedback.run().unwrap().is_empty());
    for request in [
        loom_gate::scope::Request::Tree,
        loom_gate::scope::Request::Target("heading".into()),
    ] {
        let scope = request.resolve(dir.path()).await.unwrap();
        assert!(
            plan.select(&scope)
                .unwrap()
                .run()
                .unwrap()
                .iter()
                .any(|invocation| invocation.observed().target() == &name("heading"))
        );
    }
}

#[tokio::test]
async fn admitted_resource_names_are_not_reinterpreted_as_gitignore_patterns() {
    let dir = fixture();
    write(dir.path(), "!fixture.md", "# Literal exclamation\n");
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Account responsibilities [check](accounting)\n- Literal [check](literal)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let literal = Definition::tracked(
        &snapshot,
        &inventory,
        name("literal"),
        path("src/lib.rs"),
        &BTreeSet::from([name("resource:!fixture.md")]),
        &BTreeSet::from([Duty::Document]),
        check,
    )
    .unwrap();
    let registry = Registry::new(&snapshot, vec![offered(&inventory, &snapshot), literal]).unwrap();
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    let scope = loom_gate::scope::Request::Files(vec!["!fixture.md".into()])
        .resolve(dir.path())
        .await
        .unwrap();
    let selected = plan.select(&scope).unwrap();
    assert!(
        selected
            .decisions()
            .iter()
            .find(|decision| decision.unit().target() == &name("literal"))
            .unwrap()
            .reason()
            .selected()
    );
    let annotations: Vec<_> = plan
        .units()
        .iter()
        .flat_map(|unit| unit.annotations().iter().cloned())
        .collect();
    let mut resolver = loom_gate::InputResolver::from_plan(&plan).unwrap();
    assert!(
        scope
            .select(&annotations, &mut resolver)
            .iter()
            .any(|annotation| annotation.target == "literal")
    );
}

static STALE_DISPATCHES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn record_execution(
    context: &mut loom_gate::provider::tracked::Context<'_>,
) -> Result<loom_gate::provider::tracked::Outcome, Error> {
    STALE_DISPATCHES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    check(context)
}

#[tokio::test]
async fn selection_cannot_be_executed_after_an_unselected_context_change() {
    let dir = fixture();
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Accounting [check](accounting)\n- Heading [test](heading)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(
        &snapshot,
        vec![
            offered(&inventory, &snapshot),
            definition(&snapshot, &inventory, "heading", record_execution),
        ],
    )
    .unwrap();
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    let scope = loom_gate::scope::Request::Target("heading".into())
        .resolve(dir.path())
        .await
        .unwrap();
    let selected = plan.select(&scope).unwrap();
    write(
        dir.path(),
        "unrelated.md",
        "# Changed while plan was displayed\n",
    );
    assert!(matches!(selected.explain(), Err(Error::Snapshot(_))));
    assert!(matches!(selected.run(), Err(Error::Snapshot(_))));
    assert_eq!(
        STALE_DISPATCHES.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

static SHARED_DISPATCHES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn record_shared(
    context: &mut loom_gate::provider::tracked::Context<'_>,
) -> Result<loom_gate::provider::tracked::Outcome, Error> {
    SHARED_DISPATCHES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    check(context)
}

#[tokio::test]
async fn equivalent_current_invocations_share_once_without_reusing_across_runs_or_scopes() {
    let dir = fixture();
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Accounting [check](accounting)\n- First [test](first)\n- Second [test](second)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(
        &snapshot,
        vec![
            offered(&inventory, &snapshot),
            definition(&snapshot, &inventory, "first", record_shared),
            definition(&snapshot, &inventory, "second", record_shared),
        ],
    )
    .unwrap();
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    for (index, request) in [
        loom_gate::scope::Request::Tree,
        loom_gate::scope::Request::Files(vec!["README.md".into()]),
    ]
    .into_iter()
    .enumerate()
    {
        let scope = request.resolve(dir.path()).await.unwrap();
        let selection = plan.select_tiers(&scope, &[loom_gate::Tier::Test]).unwrap();
        let explained = serde_json::to_value(selection.explain().unwrap()).unwrap();
        let decisions = explained["decisions"].as_array().unwrap();
        assert_eq!(
            decisions
                .iter()
                .filter(|decision| decision["execution"]["kind"] == "shared-in-run")
                .count(),
            1
        );
        let completed = selection.run_report().unwrap();
        assert_eq!(completed.fresh_invocations(), 1);
        assert_eq!(completed.shared_invocations(), 1);
        let invocations = completed.invocations();
        assert_eq!(invocations.len(), 2);
        assert_eq!(
            invocations
                .iter()
                .filter(|invocation| invocation.shared_with().is_none())
                .count(),
            1
        );
        assert_eq!(
            SHARED_DISPATCHES.load(std::sync::atomic::Ordering::SeqCst),
            index + 1
        );
        assert_eq!(invocations[0].observed().target(), &name("first"));
        assert_eq!(invocations[1].observed().target(), &name("second"));
        assert_eq!(invocations[1].shared_with(), Some(&name("first")));
        for invocation in invocations {
            assert_eq!(invocation.owners().len(), 1);
            assert_eq!(
                invocation.observed().outcome(),
                &loom_gate::provider::tracked::Outcome::Pass
            );
        }
    }
}

static CROSS_CONTRACT_DISPATCHES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn record_cross_contract(
    context: &mut loom_gate::provider::tracked::Context<'_>,
) -> Result<loom_gate::provider::tracked::Outcome, Error> {
    CROSS_CONTRACT_DISPATCHES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    check(context)
}

#[tokio::test]
async fn equivalent_execution_across_acceptance_contracts_shares_but_retains_both_owners() {
    let dir = fixture();
    package(&dir);
    write(
        dir.path(),
        "docs/README.md",
        "- [example](../specs/example/spec.md)\n- [other](../specs/other/spec.md)\n",
    );
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Accounting [check](accounting)\n- First [test](first)\n",
    );
    write(dir.path(), "specs/other/spec.md", "# Other contract\n");
    write(
        dir.path(),
        "specs/other/tests.md",
        "## Success Criteria\n- Second [test](second)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let first = definition(&snapshot, &inventory, "first", record_cross_contract);
    let second = first.clone().named(name("second")).unwrap();
    let registry = Registry::new(
        &snapshot,
        vec![offered(&inventory, &snapshot), first, second],
    )
    .unwrap();
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    for (index, request) in [
        loom_gate::scope::Request::Tree,
        loom_gate::scope::Request::Files(vec!["README.md".into()]),
    ]
    .into_iter()
    .enumerate()
    {
        let scope = request.resolve(dir.path()).await.unwrap();
        let selected = plan.select_tiers(&scope, &[loom_gate::Tier::Test]).unwrap();
        let report = selected.run_report().unwrap();
        assert_eq!(report.fresh_invocations(), 1);
        assert_eq!(report.shared_invocations(), 1);
        assert_eq!(
            CROSS_CONTRACT_DISPATCHES.load(std::sync::atomic::Ordering::SeqCst),
            index + 1
        );
        let invocations = report.invocations();
        assert_eq!(invocations.len(), 2);
        assert_eq!(invocations[0].owners().len(), 1);
        assert_eq!(invocations[1].owners().len(), 1);
        assert_ne!(
            invocations[0].owners()[0].source_spec,
            invocations[1].owners()[0].source_spec
        );
        assert_eq!(invocations[1].shared_with(), Some(&name("first")));
        assert!(
            invocations
                .iter()
                .all(|invocation| invocation.observed().outcome()
                    == &loom_gate::provider::tracked::Outcome::Pass)
        );
    }
    let complete = plan.run().unwrap();
    assert_eq!(complete.len(), 3);
    assert_eq!(
        CROSS_CONTRACT_DISPATCHES.load(std::sync::atomic::Ordering::SeqCst),
        3
    );
    write(
        dir.path(),
        "specs/other/tests.md",
        "## Success Criteria\n- Changed criterion [test](second)\n",
    );
    assert!(matches!(plan.run(), Err(Error::Snapshot(_))));
    assert_eq!(
        CROSS_CONTRACT_DISPATCHES.load(std::sync::atomic::Ordering::SeqCst),
        3
    );
}

static STREAM_DISPATCHES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn record_stream(
    context: &mut loom_gate::provider::tracked::Context<'_>,
) -> Result<loom_gate::provider::tracked::Outcome, Error> {
    STREAM_DISPATCHES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    check(context)
}

#[tokio::test]
async fn execution_stream_stops_permanently_on_stale_context_before_reusing_a_result() {
    let dir = fixture();
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Accounting [check](accounting)\n- First [test](first)\n- Second [test](second)\n",
    );
    let original = std::fs::read(dir.path().join("README.md")).unwrap();
    let (snapshot, inventory) = context(&dir);
    let first = definition(&snapshot, &inventory, "first", record_stream);
    let second = first.clone().named(name("second")).unwrap();
    let registry = Registry::new(
        &snapshot,
        vec![offered(&inventory, &snapshot), first, second],
    )
    .unwrap();
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    let scope = loom_gate::scope::Request::Tree
        .resolve(dir.path())
        .await
        .unwrap();
    let selected = plan.select_tiers(&scope, &[loom_gate::Tier::Test]).unwrap();
    let mut executions = selected.invocations().unwrap();
    assert_eq!(
        STREAM_DISPATCHES.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    assert_eq!(
        executions.next().unwrap().unwrap().observed().target(),
        &name("first")
    );
    write(
        dir.path(),
        "README.md",
        "# Changed after the first callback\n",
    );
    assert!(matches!(executions.next(), Some(Err(Error::Snapshot(_)))));
    std::fs::write(dir.path().join("README.md"), original).unwrap();
    assert!(
        executions.next().is_none(),
        "a failed stream resumed after restoration"
    );
    assert_eq!(
        STREAM_DISPATCHES.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    let mut fresh = selected.invocations().unwrap();
    assert_eq!(fresh.next().unwrap().unwrap().shared_with(), None);
    assert_eq!(
        STREAM_DISPATCHES.load(std::sync::atomic::Ordering::SeqCst),
        2
    );
}

#[tokio::test]
async fn shared_acceptance_owners_keep_one_cold_invocation_and_individual_results() {
    let dir = fixture();
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- First [check](accounting)\n- Second [test](accounting)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(&snapshot, vec![offered(&inventory, &snapshot)]).unwrap();
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    let scope = loom_gate::scope::Request::Tree
        .resolve(dir.path())
        .await
        .unwrap();
    for _ in 0..2 {
        let executions = plan.select(&scope).unwrap().run().unwrap();
        assert_eq!(executions.len(), 1);
        assert_eq!(executions[0].owners().len(), 2);
        assert_eq!(
            executions[0].observed().outcome(),
            &loom_gate::provider::tracked::Outcome::Pass
        );
        assert_ne!(executions[0].observed().observations(), []);
    }
}

#[test]
fn diagnostic_resolution_joins_every_contract_without_claiming_full_coverage() {
    let dir = fixture();
    package(&dir);
    write(
        dir.path(),
        "docs/README.md",
        "- [example](../specs/example/spec.md)\n- [other](../specs/other/spec.md)\n",
    );
    write(dir.path(), "specs/other/spec.md", "# Other\n");
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- First [check](heading)\n",
    );
    write(
        dir.path(),
        "specs/other/tests.md",
        "## Success Criteria\n- Second [check](heading)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(
        &snapshot,
        vec![definition(&snapshot, &inventory, "heading", check)],
    )
    .unwrap();
    let owners = std::collections::BTreeMap::from([(
        name("heading"),
        BTreeSet::from([path("specs/example/tests.md"), path("specs/other/tests.md")]),
    )]);
    let resolved = registry
        .resolve_owned(&snapshot, &inventory, &owners)
        .unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].annotations().len(), 2);
    for owner in &owners[&name("heading")] {
        assert!(resolved[0].inputs().iter().any(|input| {
            input.path() == owner
                && input
                    .origins()
                    .contains(&loom_gate::provider::Origin::Contract(owner.clone()))
        }));
    }
    assert!(matches!(
        registry.admit(&snapshot, &inventory),
        Err(Error::Coverage { .. })
    ));
    assert_eq!(
        resolved[0].run().unwrap().outcome(),
        &loom_gate::provider::tracked::Outcome::Pass
    );
    write(dir.path(), "specs/other/tests.md", "## Changed owner\n");
    assert!(resolved[0].run().is_err());
}

#[test]
fn empty_owner_set_cannot_silently_omit_a_requested_diagnostic_target() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(
        &snapshot,
        vec![definition(&snapshot, &inventory, "first", check)],
    )
    .unwrap();
    let owners = std::collections::BTreeMap::from([(name("first"), BTreeSet::new())]);
    assert!(matches!(
        registry.resolve_owned(&snapshot, &inventory, &owners),
        Err(Error::Owners { target }) if target == name("first")
    ));
}

#[test]
fn invalid_second_owner_rejects_the_entire_diagnostic_resolution() {
    let dir = fixture();
    write(
        dir.path(),
        "unrelated.md",
        "# Not an owning acceptance contract\n",
    );
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(
        &snapshot,
        vec![definition(&snapshot, &inventory, "first", check)],
    )
    .unwrap();
    let owners = std::collections::BTreeMap::from([(
        name("first"),
        BTreeSet::from([path("specs/example/tests.md"), path("unrelated.md")]),
    )]);
    assert!(matches!(
        registry.resolve_owned(&snapshot, &inventory, &owners),
        Err(Error::Admission { contract, .. }) if contract == path("unrelated.md")
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

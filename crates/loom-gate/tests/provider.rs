#![expect(
    clippy::unwrap_used,
    reason = "Native repository fixture setup must fail loudly rather than manufacture a checked provider or inventory."
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use loom_gate::inventory::snapshot::{ResourcePath, Snapshot};
use loom_gate::inventory::{Duty, Inventory, Name};
use loom_gate::provider::tracked::{Context, Observation, Outcome};
use loom_gate::provider::{Candidate, Definition, Discovery, Error, Mapping, Origin, Registry};
use tempfile::TempDir;

#[cfg(unix)]
#[path = "provider/effect.rs"]
mod effect;
#[path = "provider/identity.rs"]
mod identity;
#[path = "provider/nix.rs"]
mod nix;
#[path = "provider/plan.rs"]
mod plan;
#[path = "provider/query.rs"]
mod query;

fn name(value: &str) -> Name {
    value.to_owned().try_into().unwrap()
}

fn path(value: &str) -> ResourcePath {
    PathBuf::from(value).try_into().unwrap()
}

fn write(root: &Path, relative: &str, text: &str) {
    let file = root.join(relative);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, text).unwrap();
}

fn fixture() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        Command::new("git")
            .arg("init")
            .arg(dir.path())
            .output()
            .unwrap()
            .status
            .success()
    );
    write(dir.path(), ".gitignore", "/target/\n");
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname = 'provider-fixture'\nversion = '0.1.0'\nedition = '2024'\n",
    );
    write(dir.path(), "src/lib.rs", "pub fn checker() {}\n");
    write(dir.path(), "README.md", "# Provider example\n");
    write(dir.path(), "unrelated.md", "# Other document\n");
    write(dir.path(), "specs/example/spec.md", "# Example contract\n");
    write(
        dir.path(),
        "docs/README.md",
        "- [example](../specs/example/spec.md)\n",
    );
    write(
        dir.path(),
        "specs/example/tests.md",
        "# Example\n\n## Success Criteria\n\n- Heading [check](heading)\n- First [check](first)\n- Second [check](second)\n",
    );
    assert!(
        Command::new("cargo")
            .args(["generate-lockfile", "--offline"])
            .current_dir(dir.path())
            .output()
            .unwrap()
            .status
            .success()
    );
    dir
}

fn check(context: &mut Context<'_>) -> Result<Outcome, Error> {
    for member in context.members(&name("resource:README.md"))? {
        if !context.text(member)?.starts_with("# ") {
            return Ok(Outcome::Fail {
                detail: "document needs a heading".into(),
            });
        }
    }
    context.exists(&path("optional.md"))?;
    Ok(Outcome::Pass)
}

fn definition<F>(snapshot: &Snapshot, inventory: &Inventory, target: &str, check: F) -> Definition
where
    F: for<'a> Fn(&mut Context<'a>) -> Result<Outcome, Error> + Send + Sync + 'static,
{
    Definition::tracked(
        snapshot,
        inventory,
        name(target),
        path("src/lib.rs"),
        &BTreeSet::from([name("resource:README.md")]),
        &BTreeSet::from([Duty::Document]),
        check,
    )
    .unwrap()
}

fn contracts(targets: &[&str]) -> BTreeMap<Name, ResourcePath> {
    targets
        .iter()
        .map(|target| (name(target), path("specs/example/tests.md")))
        .collect()
}

fn context(dir: &TempDir) -> (Snapshot, Inventory) {
    let snapshot = Snapshot::discover(dir.path()).unwrap();
    let inventory = Inventory::discover(&snapshot).unwrap();
    (snapshot, inventory)
}

#[test]
fn generated_single_target_query_resolves_and_collects_real_reads_cold() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(
        &snapshot,
        vec![definition(&snapshot, &inventory, "heading", check)],
    )
    .unwrap();
    let json = registry.query(&[name("heading")]).unwrap();
    let units = registry
        .resolve_query(&snapshot, &inventory, &json, &contracts(&["heading"]))
        .unwrap();
    assert_eq!(units.len(), 1);
    let observed = units[0].run().unwrap();
    assert_eq!(observed.target(), &name("heading"));
    assert_eq!(observed.outcome(), &Outcome::Pass);
    assert!(observed.observations().iter().any(|observation| matches!(observation, Observation::Content { path: resource, .. } if resource == &path("README.md"))));
    assert!(observed.observations().iter().any(|observation| matches!(observation, Observation::Membership { subject, paths } if subject == &name("resource:README.md") && paths == &BTreeSet::from([path("README.md")]))));
    assert!(observed.observations().iter().any(|observation| matches!(observation, Observation::Existence { path: resource, present: false } if resource == &path("optional.md"))));
}

#[test]
fn query_describes_exact_responsibility_code_inputs_and_result_contract() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let definition = definition(&snapshot, &inventory, "heading", check);
    let candidate = definition.description();
    assert_eq!(candidate.discovery(), Discovery::CollectTracked);
    assert_eq!(candidate.mapping(), Mapping::PerTarget);
    assert!(candidate.capabilities().is_empty());
    let offered = candidate.responsibilities().first().unwrap();
    assert_eq!(offered.subject(), &name("resource:README.md"));
    assert_eq!(offered.duty(), Duty::Document);
    assert_eq!(offered.responsible(), offered.subject());
    assert!(candidate.inputs().iter().any(|input| {
        input.path() == &path("src/lib.rs")
            && input
                .origins()
                .contains(&Origin::Definition(path("src/lib.rs")))
    }));
    assert!(
        candidate
            .inputs()
            .iter()
            .any(|input| input.path() == &path("Cargo.lock"))
    );
    assert!(
        !candidate
            .inputs()
            .iter()
            .any(|input| input.path() == &path("unrelated.md"))
    );
}

#[test]
fn resolution_includes_owning_spec_contract_as_an_input_with_origin() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let definition = definition(&snapshot, &inventory, "heading", check);
    let candidate = definition.description().clone();
    let registry = Registry::new(&snapshot, vec![definition]).unwrap();
    let contract = path("specs/example/tests.md");
    let unit = registry
        .resolve(&snapshot, &inventory, &candidate, contract.clone())
        .unwrap();
    assert!(unit.inputs().iter().any(|input| {
        input.path() == &contract
            && input
                .origins()
                .contains(&Origin::Contract(contract.clone()))
    }));
}

#[test]
fn batched_queries_retain_complete_per_target_attribution() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(
        &snapshot,
        vec![
            definition(&snapshot, &inventory, "first", check),
            definition(&snapshot, &inventory, "second", check),
        ],
    )
    .unwrap();
    let json = registry.query(&[name("second"), name("first")]).unwrap();
    let units = registry
        .resolve_query(
            &snapshot,
            &inventory,
            &json,
            &contracts(&["first", "second"]),
        )
        .unwrap();
    assert_eq!(
        units
            .iter()
            .map(|unit| unit.target().as_str())
            .collect::<Vec<_>>(),
        ["second", "first"]
    );
}

#[test]
fn query_cannot_omit_a_requested_sibling() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(
        &snapshot,
        vec![
            definition(&snapshot, &inventory, "first", check),
            definition(&snapshot, &inventory, "second", check),
        ],
    )
    .unwrap();
    let json = registry.query(&[name("first")]).unwrap();
    assert!(
        matches!(registry.resolve_query(&snapshot, &inventory, &json, &contracts(&["first", "second"])), Err(Error::MissingResponse { target }) if target == name("second"))
    );
}

#[test]
fn duplicate_registration_is_not_last_writer_wins() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let definition = definition(&snapshot, &inventory, "heading", check);
    assert!(matches!(
        Registry::new(&snapshot, vec![definition.clone(), definition]),
        Err(Error::Duplicate { .. })
    ));
}

#[test]
fn duplicate_query_targets_are_not_silently_deduplicated() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(
        &snapshot,
        vec![definition(&snapshot, &inventory, "heading", check)],
    )
    .unwrap();
    assert!(matches!(
        registry.query(&[name("heading"), name("heading")]),
        Err(Error::Duplicate { .. })
    ));
}

#[test]
fn duplicate_response_targets_do_not_construct_a_partial_plan() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let definition = definition(&snapshot, &inventory, "heading", check);
    let candidate = serde_json::to_value(definition.description()).unwrap();
    let json = serde_json::json!({"units": [candidate.clone(), candidate]}).to_string();
    let registry = Registry::new(&snapshot, vec![definition]).unwrap();
    assert!(matches!(
        registry.resolve_query(&snapshot, &inventory, &json, &contracts(&["heading"])),
        Err(Error::Duplicate { .. })
    ));
}

#[test]
fn unrequested_response_targets_fail_instead_of_extending_scope() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(
        &snapshot,
        vec![definition(&snapshot, &inventory, "heading", check)],
    )
    .unwrap();
    let json = registry.query(&[name("heading")]).unwrap();
    assert!(matches!(
        registry.resolve_query(&snapshot, &inventory, &json, &BTreeMap::new()),
        Err(Error::Unexpected { .. })
    ));
}

#[test]
fn provider_candidates_use_closed_variants_before_admission() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let definition = definition(&snapshot, &inventory, "heading", check);
    let rust = definition.description();
    let registry = Registry::new(&snapshot, vec![definition.clone()]).unwrap();
    let mut nix = serde_json::to_value(rust).unwrap();
    nix["kind"] = "hermetic-nix".into();
    nix["execution"] = serde_json::json!({
        "flake": "flake.nix", "attribute": "checks.x86_64-linux.heading",
        "system": "x86_64-linux", "graph": nix["contract"]["definition"],
        "derivation": "/nix/store/00000000000000000000000000000000-heading.drv",
        "source": "/nix/store/00000000000000000000000000000000-source",
        "executor": "/nix/store/00000000000000000000000000000000-nix",
        "binary": "bin/nix",
        "artifact": nix["contract"]["definition"],
    });
    let parsed = Candidate::parse(&nix.to_string()).unwrap();
    assert!(matches!(parsed, Candidate::HermeticNix { .. }));
    assert!(matches!(
        registry.resolve(
            &snapshot,
            &inventory,
            &parsed,
            path("specs/example/tests.md")
        ),
        Err(Error::Inconsistent { .. })
    ));
    let mut effect = nix;
    effect["kind"] = "supported-effect".into();
    effect["execution"] = serde_json::json!({
        "effect": "container-engine-info", "executable": "/usr/bin/podman",
        "artifact": effect["contract"]["definition"],
        "adapter": "/usr/bin/provider", "adapter_artifact": effect["contract"]["definition"],
        "endpoint": "/run/podman/podman.sock", "system": "x86_64-linux"
    });
    let parsed = Candidate::parse(&effect.to_string()).unwrap();
    assert!(matches!(parsed, Candidate::SupportedEffect { .. }));
    assert!(matches!(
        registry.resolve(
            &snapshot,
            &inventory,
            &parsed,
            path("specs/example/tests.md")
        ),
        Err(Error::Inconsistent { .. })
    ));
    effect["execution"] = "arbitrary-command".into();
    assert!(Candidate::parse(&effect.to_string()).is_err());
    effect["execution"] = "container-engine-info".into();
    effect["trusted"] = true.into();
    assert!(Candidate::parse(&effect.to_string()).is_err());
    let missing = Registry::new(&snapshot, vec![]).unwrap();
    assert!(matches!(
        missing.resolve(&snapshot, &inventory, rust, path("specs/example/tests.md")),
        Err(Error::Missing { .. })
    ));
}

#[test]
fn candidate_parser_rejects_unknown_and_incompatible_families() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let definition = definition(&snapshot, &inventory, "heading", check);
    let value = serde_json::to_value(definition.description()).unwrap();
    for kind in ["arbitrary", "effectful", "hermetic-nix", "supported-effect"] {
        let mut candidate = value.clone();
        candidate["kind"] = kind.into();
        assert!(Candidate::parse(&candidate.to_string()).is_err(), "{kind}");
    }
}

#[test]
fn trust_and_hermetic_flags_cannot_extend_a_candidate_contract() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let definition = definition(&snapshot, &inventory, "heading", check);
    for flag in ["trusted", "hermetic", "complete"] {
        let mut value = serde_json::to_value(definition.description()).unwrap();
        value[flag] = true.into();
        assert!(Candidate::parse(&value.to_string()).is_err());
        value.as_object_mut().unwrap().remove(flag);
        value["contract"][flag] = true.into();
        assert!(Candidate::parse(&value.to_string()).is_err());
    }
}

#[test]
fn raw_input_lists_do_not_register_or_admit_a_provider() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let registry = Registry::new(&snapshot, vec![]).unwrap();
    assert!(
        registry
            .resolve_query(
                &snapshot,
                &inventory,
                r#"{"inputs":["README.md"]}"#,
                &contracts(&["heading"])
            )
            .is_err()
    );
}

#[test]
fn parsed_descriptions_cannot_supply_missing_actual_definitions() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let definition = definition(&snapshot, &inventory, "heading", check);
    let parsed =
        Candidate::parse(&serde_json::to_string(definition.description()).unwrap()).unwrap();
    let registry = Registry::new(&snapshot, vec![]).unwrap();
    assert!(matches!(
        registry.resolve(
            &snapshot,
            &inventory,
            &parsed,
            path("specs/example/tests.md")
        ),
        Err(Error::Missing { .. })
    ));
}

#[test]
fn duplicate_responsibility_metadata_is_not_silently_normalized() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let definition = definition(&snapshot, &inventory, "heading", check);
    let mut value = serde_json::to_value(definition.description()).unwrap();
    let responsibility = value["contract"]["responsibilities"][0].clone();
    value["contract"]["responsibilities"] =
        serde_json::json!([responsibility.clone(), responsibility]);
    assert!(Candidate::parse(&value.to_string()).is_err());
}

#[test]
fn smaller_self_reported_inputs_fail_current_definition_resolution() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let definition = definition(&snapshot, &inventory, "heading", check);
    let mut value = serde_json::to_value(definition.description()).unwrap();
    value["contract"]["inputs"] = serde_json::json!([]);
    let candidate = Candidate::parse(&value.to_string()).unwrap();
    let registry = Registry::new(&snapshot, vec![definition]).unwrap();
    let error = registry
        .resolve(
            &snapshot,
            &inventory,
            &candidate,
            path("specs/example/tests.md"),
        )
        .err()
        .unwrap();
    assert!(matches!(error, Error::Inconsistent { .. }));
    let feedback = error.to_string();
    assert!(
        feedback.contains("heading")
            && feedback.contains("src/lib.rs")
            && feedback.contains("loom gate verify")
    );
}

#[test]
fn mismatched_inventory_cannot_admit_an_execution() {
    let dir = fixture();
    let (before, inventory) = context(&dir);
    write(dir.path(), "README.md", "# Changed\n");
    let current = Snapshot::discover(dir.path()).unwrap();
    assert!(matches!(
        Definition::tracked(
            &current,
            &inventory,
            name("heading"),
            path("src/lib.rs"),
            &BTreeSet::from([name("resource:README.md")]),
            &BTreeSet::from([Duty::Document]),
            check
        ),
        Err(Error::Inventory)
    ));
    assert!(before.require_current().is_err());
}

#[test]
fn membership_addition_invalidates_an_already_resolved_unit() {
    let dir = fixture();
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
    write(dir.path(), "optional.md", "# Newly present\n");
    assert!(matches!(unit.run(), Err(Error::Snapshot(_))));
}

#[test]
fn deleted_document_is_an_invalid_registration_not_an_empty_analysis_scope() {
    let dir = fixture();
    std::fs::remove_file(dir.path().join("README.md")).unwrap();
    let (snapshot, inventory) = context(&dir);
    let Err(error) = Definition::tracked(
        &snapshot,
        &inventory,
        name("heading"),
        path("src/lib.rs"),
        &BTreeSet::from([name("resource:README.md")]),
        &BTreeSet::from([Duty::Document]),
        check,
    ) else {
        panic!("an absent subject must not register");
    };
    let detail = format!("{:#}", anyhow::Error::new(error));
    assert!(detail.contains("heading"), "{detail}");
    assert!(detail.contains("src/lib.rs"), "{detail}");
    assert!(detail.contains("resource:README.md"), "{detail}");
    assert!(detail.contains("loom gate verify"), "{detail}");
}

#[test]
fn renamed_analysis_subject_requires_corrected_registration() {
    let dir = fixture();
    std::fs::rename(dir.path().join("README.md"), dir.path().join("renamed.md")).unwrap();
    let (snapshot, inventory) = context(&dir);
    assert!(
        Definition::tracked(
            &snapshot,
            &inventory,
            name("heading"),
            path("src/lib.rs"),
            &BTreeSet::from([name("resource:README.md")]),
            &BTreeSet::from([Duty::Document]),
            check
        )
        .is_err()
    );
    assert!(
        Definition::tracked(
            &snapshot,
            &inventory,
            name("heading"),
            path("src/lib.rs"),
            &BTreeSet::from([name("resource:renamed.md")]),
            &BTreeSet::from([Duty::Document]),
            check
        )
        .is_ok()
    );
}

fn ignored_outside_read(context: &mut Context<'_>) -> Result<Outcome, Error> {
    match context.text(&path("unrelated.md")) {
        Err(Error::Outside { .. }) => Ok(Outcome::Pass),
        Err(error) => Err(error),
        Ok(_) => Ok(Outcome::Fail {
            detail: "outside read was incorrectly admitted".into(),
        }),
    }
}

#[test]
fn a_checker_cannot_swallow_an_outside_read_and_report_pass() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let definition = definition(&snapshot, &inventory, "heading", ignored_outside_read);
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
    assert!(matches!(unit.run(), Err(Error::Outside { .. })));
}

#[test]
fn an_actual_verifier_failure_remains_distinct_from_admission_failure() {
    let dir = fixture();
    write(dir.path(), "README.md", "No heading\n");
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
    assert!(matches!(
        unit.run().unwrap().outcome(),
        Outcome::Fail { .. }
    ));
}

#[test]
fn a_different_named_callable_changes_the_execution_definition() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let first = definition(&snapshot, &inventory, "heading", check);
    let second = definition(&snapshot, &inventory, "heading", ignored_outside_read);
    assert_ne!(
        first.description().definition(),
        second.description().definition()
    );
    let registry = Registry::new(&snapshot, vec![second]).unwrap();
    assert!(matches!(
        registry.resolve(
            &snapshot,
            &inventory,
            first.description(),
            path("specs/example/tests.md")
        ),
        Err(Error::Inconsistent { .. })
    ));
}

#[test]
fn erased_callable_identity_is_rejected_at_registration() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let pointer: fn(&mut Context<'_>) -> Result<Outcome, Error> = check;
    assert!(matches!(
        Definition::tracked(
            &snapshot,
            &inventory,
            name("heading"),
            path("src/lib.rs"),
            &BTreeSet::from([name("resource:README.md")]),
            &BTreeSet::from([Duty::Document]),
            pointer
        ),
        Err(Error::Callable { .. })
    ));
}

#[test]
fn unrelated_spec_source_cannot_supply_owning_annotation_attribution() {
    let dir = fixture();
    write(dir.path(), "specs/other/spec.md", "# Other contract\n");
    write(
        dir.path(),
        "docs/README.md",
        "- [example](../specs/example/spec.md)\n- [other](../specs/other/spec.md)\n",
    );
    write(
        dir.path(),
        "specs/other/tests.md",
        "# Other\n\n## Success Criteria\n\n- Other [check](other)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let definition = definition(&snapshot, &inventory, "heading", check);
    let candidate = definition.description().clone();
    let registry = Registry::new(&snapshot, vec![definition]).unwrap();
    assert!(matches!(
        registry.resolve(
            &snapshot,
            &inventory,
            &candidate,
            path("specs/other/tests.md")
        ),
        Err(Error::Annotation { .. })
    ));
}

#[test]
fn bootstrap_support_annotations_cannot_supply_acceptance_authority() {
    let dir = fixture();
    std::fs::remove_file(dir.path().join("specs/example/spec.md")).unwrap();
    std::fs::remove_file(dir.path().join("specs/example/tests.md")).unwrap();
    write(
        dir.path(),
        "specs/example.md",
        "## Success Criteria\n- Heading [check](heading)\n",
    );
    write(
        dir.path(),
        "docs/README.md",
        "- [example](../specs/example.md)\n",
    );
    write(
        dir.path(),
        "specs/example/support.md",
        "## Success Criteria\n- Pretend authority [check](heading)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let definition = definition(&snapshot, &inventory, "heading", check);
    let candidate = definition.description().clone();
    let registry = Registry::new(&snapshot, vec![definition]).unwrap();
    let unit = registry
        .resolve(&snapshot, &inventory, &candidate, path("specs/example.md"))
        .unwrap();
    assert!(unit.inputs().iter().any(|input| {
        input.path() == &path("specs/example.md")
            && input
                .origins()
                .contains(&Origin::Contract(path("specs/example.md")))
    }));
    assert!(matches!(
        registry.resolve(
            &snapshot,
            &inventory,
            &candidate,
            path("specs/example/support.md")
        ),
        Err(Error::Contract { .. })
    ));
}

#[test]
fn corrected_registration_reenters_the_same_query_resolution_path() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    assert!(
        Definition::tracked(
            &snapshot,
            &inventory,
            name("heading"),
            path("missing.rs"),
            &BTreeSet::from([name("resource:README.md")]),
            &BTreeSet::from([Duty::Document]),
            check
        )
        .is_err()
    );
    let registry = Registry::new(
        &snapshot,
        vec![definition(&snapshot, &inventory, "heading", check)],
    )
    .unwrap();
    let json = registry.query(&[name("heading")]).unwrap();
    assert!(
        registry
            .resolve_query(&snapshot, &inventory, &json, &contracts(&["heading"]))
            .unwrap()[0]
            .run()
            .is_ok()
    );
}

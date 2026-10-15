use super::{context, contracts, fixture, name, path, write};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use loom_gate::inventory::Duty;
use loom_gate::provider::nix::{Error as NixError, StorePath};
use loom_gate::provider::tracked::{Observation, Outcome};
use loom_gate::provider::{Candidate, Capability, Definition, Discovery, Error, Mapping, Registry};
use tempfile::TempDir;

fn bash() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| directory.join("bash"))
        .find(|path| path.is_file())
        .unwrap()
        .canonicalize()
        .unwrap()
}

fn native_project(dir: &TempDir, options: &str, source: bool, pinned: bool) {
    let tool = bash();
    let root: PathBuf = tool.components().take(4).collect();
    let builder = if pinned {
        format!(
            "builtins.appendContext {} {{ {} = {{ path = true; }}; }}",
            serde_json::to_string(tool.to_str().unwrap()).unwrap(),
            serde_json::to_string(root.to_str().unwrap()).unwrap()
        )
    } else {
        serde_json::to_string(tool.to_str().unwrap()).unwrap()
    };
    write(
        dir.path(),
        "flake.nix",
        &format!(
            r#"{{
  outputs = {{ self }}: {{
    checks.x86_64-linux.heading = builtins.derivation {{
      name = "loom-provider-heading";
      system = "x86_64-linux";
      builder = {builder};
      args = [ "-c" "read -r heading < \"$src/README.md\"; [[ \"$heading\" == '# '* ]] && echo success > \"$out\"" ];
      {source}
      {options}
    }};
  }};
}}
"#,
            source = if source { "src = self;" } else { "" }
        ),
    );
}

fn definition(
    dir: &TempDir,
) -> Result<
    (
        loom_gate::inventory::snapshot::Snapshot,
        loom_gate::inventory::Inventory,
        Definition,
    ),
    Error,
> {
    let (snapshot, inventory) = context(dir);
    let definition = Definition::hermetic(
        &snapshot,
        &inventory,
        name("heading"),
        &name("nix:check:flake.nix:x86_64-linux:heading"),
        &BTreeSet::from([name("resource:README.md")]),
        &BTreeSet::from([Duty::Document]),
    )?;
    Ok((snapshot, inventory, definition))
}

#[expect(
    clippy::panic,
    reason = "This integration-test helper must fail loudly if construction errors omit target/producer context; it never supplies production fallback values."
)]
fn failure(dir: &TempDir) -> NixError {
    match definition(dir) {
        Err(Error::Construction {
            target,
            producer,
            source,
        }) => {
            assert_eq!(target, name("heading"));
            assert_eq!(producer, path("flake.nix"));
            match *source {
                Error::Nix(error) => error,
                error => panic!("unexpected construction error: {error:?}"),
            }
        }
        _ => panic!("native construction must fail with target and producer context"),
    }
}

#[test]
fn lazy_discovery_does_not_turn_a_failed_native_definition_into_a_unit() {
    let dir = fixture();
    write(
        dir.path(),
        "flake.nix",
        "{ outputs = { self }: { checks.x86_64-linux.heading = throw \"invalid native execution\"; }; }\n",
    );
    assert!(matches!(
        failure(&dir),
        NixError::Query { detail } if detail.contains("invalid native execution")
    ));
}

#[test]
fn declared_check_names_do_not_admit_non_derivation_values() {
    let dir = fixture();
    write(
        dir.path(),
        "flake.nix",
        "{ outputs = { self }: { checks.x86_64-linux.heading = 7; }; }\n",
    );
    assert!(matches!(failure(&dir), NixError::Query { .. }));
}

#[test]
fn native_graph_description_resolves_to_the_actual_store_execution() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    let (snapshot, inventory, definition) = definition(&dir).unwrap();
    let binding = inventory
        .subject(&name("nix:check:flake.nix:x86_64-linux:heading"))
        .unwrap()
        .nix_target()
        .unwrap();
    assert_eq!(binding.flake(), &path("flake.nix"));
    assert_eq!(binding.attribute(), &name("heading"));
    let registry = Registry::new(&snapshot, vec![definition]).unwrap();
    let query = registry.query(&[name("heading")]).unwrap();
    let json: serde_json::Value = serde_json::from_str(&query).unwrap();
    let execution = &json["units"][0]["execution"];
    let tool = Path::new(execution["executor"].as_str().unwrap())
        .join(execution["binary"].as_str().unwrap());
    let artifact = blake3::hash(&std::fs::read(tool).unwrap())
        .to_hex()
        .to_string();
    assert_eq!(execution["artifact"].as_str().unwrap(), artifact);
    let units = registry
        .resolve_query(&snapshot, &inventory, &query, &contracts(&["heading"]))
        .unwrap();
    let unit = &units[0];
    assert!(matches!(unit.description(), Candidate::HermeticNix { .. }));
    assert_eq!(unit.description().discovery(), Discovery::ConstructedGraph);
    assert_eq!(unit.description().mapping(), Mapping::DerivationOutput);
    assert_eq!(
        unit.description().capabilities(),
        BTreeSet::from([Capability::NixSandbox])
    );
    let graph = unit.nix_graph().unwrap();
    assert!(graph.dependencies().contains(graph.source()));
    assert!(Path::new(graph.derivation().as_str()).is_file());
    assert_eq!(
        std::fs::read(Path::new(graph.source().as_str()).join("README.md")).unwrap(),
        snapshot.files()[&path("README.md")]
    );
    let observed = unit.run().unwrap();
    assert!(observed.observations().iter().any(|observation| matches!(observation, Observation::ExecutionGraph { digest } if digest == graph.identity())));
    match observed.outcome() {
        Outcome::Pass => assert!(
            observed
                .observations()
                .iter()
                .any(|observation| matches!(observation, Observation::Output { .. }))
        ),
        Outcome::Unavailable { capability, detail } => {
            assert_eq!(*capability, Capability::NixSandbox);
            assert!(detail.contains("sandbox") || detail.contains("Operation not permitted"));
            assert!(!detail.contains("--no-sandbox"));
            assert!(
                !observed
                    .observations()
                    .iter()
                    .any(|observation| matches!(observation, Observation::Output { .. }))
            );
        }
        outcome => panic!("unexpected native execution outcome: {outcome:?}"),
    }
}

#[test]
fn native_graph_source_omission_fails_before_dispatch_and_correction_reenters_admission() {
    let dir = fixture();
    native_project(&dir, "", false, true);
    assert!(matches!(failure(&dir), NixError::Source { .. }));
    native_project(&dir, "", true, true);
    let (snapshot, inventory, definition) = definition(&dir).unwrap();
    let registry = Registry::new(&snapshot, vec![definition]).unwrap();
    let query = registry.query(&[name("heading")]).unwrap();
    assert_eq!(
        registry
            .resolve_query(&snapshot, &inventory, &query, &contracts(&["heading"]))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn raw_native_builder_paths_are_not_pinned_tool_inputs() {
    let dir = fixture();
    native_project(&dir, "", true, false);
    assert!(matches!(failure(&dir), NixError::Builder { .. }));
}

#[test]
fn native_unsandboxed_flags_are_invalid_definitions_not_supported_effects() {
    let dir = fixture();
    native_project(&dir, "__noChroot = true;", true, true);
    assert!(matches!(failure(&dir), NixError::Unsafe { .. }));
}

#[test]
fn native_local_network_exceptions_cannot_enter_the_deterministic_lane() {
    let dir = fixture();
    native_project(&dir, "__darwinAllowLocalNetworking = true;", true, true);
    assert!(matches!(failure(&dir), NixError::Unsafe { .. }));
}

#[test]
fn native_impure_host_dependencies_cannot_enter_the_deterministic_lane() {
    let dir = fixture();
    native_project(
        &dir,
        "__darwinImpureHostDeps = [ \"/host/resource\" ];",
        true,
        true,
    );
    assert!(matches!(failure(&dir), NixError::Unsafe { .. }));
}

#[test]
fn native_builder_traversal_is_not_an_immutable_resource_binding() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    let source = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(
        dir.path(),
        "flake.nix",
        &source.replace("/bin/bash", "/bin/../bin/bash"),
    );
    assert!(matches!(failure(&dir), NixError::StorePath));
}

#[test]
fn native_impure_flags_are_invalid_even_with_complete_staged_sources() {
    let dir = fixture();
    native_project(&dir, "__impure = true;", true, true);
    match failure(&dir) {
        NixError::Unsafe { .. } => {}
        NixError::Query { detail } => assert!(detail.contains("impure"), "{detail}"),
        error => panic!("unexpected impure construction error: {error:?}"),
    }
}

#[test]
fn fixed_output_checks_cannot_acquire_network_in_the_deterministic_lane() {
    let dir = fixture();
    native_project(
        &dir,
        "outputHashAlgo = \"sha256\"; outputHashMode = \"flat\"; outputHash = \"0000000000000000000000000000000000000000000000000000000000000000\";",
        true,
        true,
    );
    assert!(matches!(failure(&dir), NixError::FixedCheck));
}

#[test]
fn a_new_native_check_creates_obligations_without_any_provider_registration() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    let (_, inventory) = context(&dir);
    assert!(
        inventory
            .obligations()
            .iter()
            .any(|obligation| obligation.subject()
                == &name("nix:check:flake.nix:x86_64-linux:heading")
                && obligation.duty() == Duty::Check)
    );
}

#[test]
fn claimed_smaller_native_inputs_cannot_replace_the_constructed_source() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    let (snapshot, inventory, definition) = definition(&dir).unwrap();
    let mut json = serde_json::to_value(definition.description()).unwrap();
    json["contract"]["inputs"] = serde_json::json!([]);
    let candidate = Candidate::parse(&json.to_string()).unwrap();
    let registry = Registry::new(&snapshot, vec![definition]).unwrap();
    assert!(matches!(
        registry.resolve(
            &snapshot,
            &inventory,
            &candidate,
            path("specs/example/tests.md")
        ),
        Err(Error::Inconsistent { .. })
    ));
}

#[test]
fn native_input_additions_invalidate_resolved_units_and_their_source_identity() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    let (snapshot, inventory, first) = definition(&dir).unwrap();
    let old = first.description().clone();
    let registry = Registry::new(&snapshot, vec![first]).unwrap();
    let unit = registry
        .resolve(&snapshot, &inventory, &old, path("specs/example/tests.md"))
        .unwrap();
    write(dir.path(), "new.md", "# New document\n");
    assert!(matches!(unit.run(), Err(Error::Snapshot(_))));
    let (_, _, new) = definition(&dir).unwrap();
    assert_ne!(old.definition(), new.description().definition());
}

#[cfg(unix)]
#[test]
fn native_staged_source_preserves_executable_file_identity() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = fixture();
    native_project(&dir, "", true, true);
    std::fs::set_permissions(
        dir.path().join("README.md"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let (snapshot, inventory, definition) = definition(&dir).unwrap();
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
    let staged = Path::new(unit.nix_graph().unwrap().source().as_str()).join("README.md");
    assert_ne!(
        std::fs::metadata(staged).unwrap().permissions().mode() & 0o111,
        0
    );
}

#[cfg(unix)]
#[test]
fn executable_mode_drift_invalidates_native_units_and_source_identity() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = fixture();
    native_project(&dir, "", true, true);
    std::fs::set_permissions(
        dir.path().join("README.md"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let (snapshot, inventory, first) = definition(&dir).unwrap();
    let old = first.description().clone();
    let registry = Registry::new(&snapshot, vec![first]).unwrap();
    let unit = registry
        .resolve(&snapshot, &inventory, &old, path("specs/example/tests.md"))
        .unwrap();
    std::fs::set_permissions(
        dir.path().join("README.md"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(matches!(unit.run(), Err(Error::Snapshot(_))));
    let (_, _, new) = definition(&dir).unwrap();
    assert_ne!(old.snapshot(), new.description().snapshot());
    assert_ne!(old.definition(), new.description().definition());
}

#[test]
fn native_target_deletion_requires_correcting_its_exact_binding() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    let source = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(
        dir.path(),
        "flake.nix",
        &source.replace("checks.x86_64-linux.heading", "checks.x86_64-linux.renamed"),
    );
    assert!(matches!(definition(&dir), Err(Error::Subject(_))));
}

#[test]
fn native_output_system_cannot_disagree_with_its_derivation() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    let source = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(
        dir.path(),
        "flake.nix",
        &source.replace("system = \"x86_64-linux\"", "system = \"aarch64-linux\""),
    );
    assert!(matches!(failure(&dir), NixError::System { .. }));
}

#[test]
fn valid_unsupported_platform_is_inapplicable_not_an_invalid_contract_or_pass() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    let source = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(
        dir.path(),
        "flake.nix",
        &source.replace("x86_64-linux", "unsupported-loom-platform"),
    );
    let (snapshot, inventory) = context(&dir);
    let definition = Definition::hermetic(
        &snapshot,
        &inventory,
        name("heading"),
        &name("nix:check:flake.nix:unsupported-loom-platform:heading"),
        &BTreeSet::from([name("resource:README.md")]),
        &BTreeSet::from([Duty::Document]),
    )
    .unwrap();
    let registry = Registry::new(&snapshot, vec![definition]).unwrap();
    let units = registry
        .resolve_query(
            &snapshot,
            &inventory,
            &registry.query(&[name("heading")]).unwrap(),
            &contracts(&["heading"]),
        )
        .unwrap();
    assert!(
        matches!(units[0].run().unwrap().outcome(), Outcome::Inapplicable { system } if system == &name("unsupported-loom-platform"))
    );
}

#[test]
fn batched_native_queries_retain_exact_target_and_graph_attribution() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    let (snapshot, inventory, first) = definition(&dir).unwrap();
    let second = Definition::hermetic(
        &snapshot,
        &inventory,
        name("second"),
        &name("nix:check:flake.nix:x86_64-linux:heading"),
        &BTreeSet::from([name("resource:README.md")]),
        &BTreeSet::from([Duty::Document]),
    )
    .unwrap();
    let registry = Registry::new(&snapshot, vec![first, second]).unwrap();
    let units = registry
        .resolve_query(
            &snapshot,
            &inventory,
            &registry.query(&[name("heading"), name("second")]).unwrap(),
            &contracts(&["heading", "second"]),
        )
        .unwrap();
    assert_eq!(units[0].target(), &name("heading"));
    assert_eq!(units[1].target(), &name("second"));
    assert_eq!(
        units[0].nix_graph().unwrap().identity(),
        units[1].nix_graph().unwrap().identity()
    );
    assert_ne!(
        units[0].description().definition(),
        units[1].description().definition()
    );
}

#[test]
fn arbitrary_host_paths_never_parse_as_native_store_resources() {
    let directory = tempfile::tempdir().unwrap();
    let host = directory.path().join("source");
    for input in [
        host.to_str().unwrap(),
        "/nix/store/a",
        "/nix/store/00000000000000000000000000000000-source/../secret",
        "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaé-source",
    ] {
        assert!(StorePath::try_from(input.to_owned()).is_err(), "{input}");
    }
}

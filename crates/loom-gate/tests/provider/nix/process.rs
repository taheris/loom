//! Ordinary process execution never impersonates a hermetic Nix result.

use super::*;

fn process_project(dir: &TempDir) -> String {
    native_project(dir, "", true, true);
    let platform = use_host_system(dir);
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(
        dir.path(),
        "flake.nix",
        &flake.replace(
            "resources = [ source ];",
            "runtime = \"native-process\"; resources = [ source ];",
        ),
    );
    platform
}

fn nondeterministic_project(dir: &TempDir, artifact: bool) -> (String, &'static str) {
    let platform = process_project(dir);
    let mut flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    flake = flake
        .replace("echo success", r#"echo \"\${out^^}\""#)
        .replace(
            "name = \"loom-provider-heading\";",
            &format!(
                "name = \"loom-provider-heading\"; NONCE = {:?};",
                dir.path().file_name().unwrap().to_str().unwrap()
            ),
        );
    let namespace = if artifact {
        flake = flake
            .replace("checks.", "packages.")
            .replace("checked.check {", "checked.artifact {");
        "package"
    } else {
        "check"
    };
    write(dir.path(), "flake.nix", &flake);
    (platform, namespace)
}

#[test]
fn build_only_artifacts_reuse_outputs_but_reject_stale_sources() {
    let dir = fixture();
    let (platform, namespace) = nondeterministic_project(&dir, true);
    let (snapshot, inventory) = context(&dir);
    let subject = name(&format!("nix:{namespace}:flake.nix:{platform}:heading"));
    let definition =
        Definition::hermetic_baseline(&snapshot, &inventory, name("heading"), &subject).unwrap();
    let registry = Registry::new(&snapshot, vec![definition.clone()]).unwrap();
    let unit = registry
        .resolve(
            &snapshot,
            &inventory,
            definition.description(),
            path("specs/example/tests.md"),
        )
        .unwrap();
    for _ in 0..2 {
        assert!(matches!(unit.run().unwrap().outcome(), Outcome::Pass));
    }
    let mut forged = serde_json::to_value(definition.description()).unwrap();
    assert_eq!(forged["execution"]["purpose"], "artifact");
    forged["execution"]["purpose"] = "check".into();
    let forged = Candidate::parse(&forged.to_string()).unwrap();
    assert!(
        registry
            .resolve(
                &snapshot,
                &inventory,
                &forged,
                path("specs/example/tests.md")
            )
            .is_err()
    );
    write(dir.path(), "README.md", "invalid source\n");
    assert!(unit.run().is_err());
    let (snapshot, inventory) = context(&dir);
    let definition =
        Definition::hermetic_baseline(&snapshot, &inventory, name("heading"), &subject).unwrap();
    let registry = Registry::new(&snapshot, vec![definition.clone()]).unwrap();
    let unit = registry
        .resolve(
            &snapshot,
            &inventory,
            definition.description(),
            path("specs/example/tests.md"),
        )
        .unwrap();
    assert!(matches!(
        unit.run().unwrap().outcome(),
        Outcome::Fail { .. }
    ));
}

#[test]
fn behavioral_checks_still_execute_when_their_output_is_present() {
    let dir = fixture();
    let (platform, namespace) = nondeterministic_project(&dir, false);
    let (snapshot, inventory) = context(&dir);
    let subject = name(&format!("nix:{namespace}:flake.nix:{platform}:heading"));
    let definition =
        Definition::hermetic_baseline(&snapshot, &inventory, name("heading"), &subject).unwrap();
    let registry = Registry::new(&snapshot, vec![definition.clone()]).unwrap();
    let unit = registry
        .resolve(
            &snapshot,
            &inventory,
            definition.description(),
            path("specs/example/tests.md"),
        )
        .unwrap();
    assert!(matches!(unit.run().unwrap().outcome(), Outcome::Pass));
    assert!(matches!(
        unit.run().unwrap().outcome(),
        Outcome::Fail { .. }
    ));
}

#[test]
fn a_check_namespace_cannot_opt_into_artifact_reuse() {
    let dir = fixture();
    let _platform = process_project(&dir);
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(
        dir.path(),
        "flake.nix",
        &flake.replace("checked.check {", "checked.artifact {"),
    );
    let snapshot = loom_gate::inventory::snapshot::Snapshot::discover(dir.path()).unwrap();
    assert!(loom_gate::inventory::Inventory::discover(&snapshot).is_err());
}

#[test]
fn native_process_runs_real_recipe_without_claiming_sandbox_confinement() {
    let dir = fixture();
    let platform = process_project(&dir);
    let (snapshot, inventory) = context(&dir);
    let subject = name(&format!("nix:check:flake.nix:{platform}:heading"));
    let definition =
        Definition::hermetic_baseline(&snapshot, &inventory, name("heading"), &subject).unwrap();
    assert!(matches!(
        definition.description(),
        Candidate::NativeProcess { .. }
    ));
    let description = serde_json::to_value(definition.description()).unwrap();
    let local = description["execution"]["process"]
        .get("local-agent")
        .is_some();
    assert_eq!(
        definition
            .description()
            .capabilities()
            .contains(&Capability::NixSandbox),
        !local,
        "outside the agent context execution must still require the sandbox",
    );
    let registry = Registry::new(&snapshot, vec![definition.clone()]).unwrap();
    let unit = registry
        .resolve(
            &snapshot,
            &inventory,
            definition.description(),
            path("specs/example/tests.md"),
        )
        .unwrap();
    for _ in 0..2 {
        assert!(matches!(unit.run().unwrap().outcome(), Outcome::Pass));
    }
    let mut forged = serde_json::to_value(definition.description()).unwrap();
    forged["kind"] = "hermetic-nix".into();
    let forged = Candidate::parse(&forged.to_string()).unwrap();
    assert!(
        registry
            .resolve(
                &snapshot,
                &inventory,
                &forged,
                path("specs/example/tests.md")
            )
            .is_err()
    );
}

#[test]
fn native_process_failure_is_real_and_stale_source_cannot_reuse_a_pass() {
    let dir = fixture();
    let platform = process_project(&dir);
    let (snapshot, inventory) = context(&dir);
    let subject = name(&format!("nix:check:flake.nix:{platform}:heading"));
    let definition =
        Definition::hermetic_baseline(&snapshot, &inventory, name("heading"), &subject).unwrap();
    let registry = Registry::new(&snapshot, vec![definition.clone()]).unwrap();
    let unit = registry
        .resolve(
            &snapshot,
            &inventory,
            definition.description(),
            path("specs/example/tests.md"),
        )
        .unwrap();
    assert!(matches!(unit.run().unwrap().outcome(), Outcome::Pass));
    write(dir.path(), "README.md", "not a heading\n");
    assert!(unit.run().is_err());
    let (snapshot, inventory) = context(&dir);
    let definition =
        Definition::hermetic_baseline(&snapshot, &inventory, name("heading"), &subject).unwrap();
    let registry = Registry::new(&snapshot, vec![definition.clone()]).unwrap();
    let unit = registry
        .resolve(
            &snapshot,
            &inventory,
            definition.description(),
            path("specs/example/tests.md"),
        )
        .unwrap();
    assert!(matches!(
        unit.run().unwrap().outcome(),
        Outcome::Fail { .. }
    ));
}

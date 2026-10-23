use super::{fixture, inventory, name, path, strategy, write};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use loom_gate::inventory::snapshot::Snapshot;
use loom_gate::inventory::{Duty, Error, Inventory, Role};

fn lock(root: &Path) {
    let output = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn workspace(extra: &str) -> tempfile::TempDir {
    let dir = fixture("", "pub fn root() {}\n");
    write(
        dir.path(),
        "Cargo.toml",
        &format!(
            "[workspace]\nresolver='3'\nmembers=['checker','normal','builder','transitive','devonly','unrelated']\n{extra}"
        ),
    );
    std::fs::remove_file(dir.path().join("src/lib.rs")).unwrap();
    for package in [
        "checker",
        "normal",
        "builder",
        "transitive",
        "devonly",
        "unrelated",
    ] {
        write(
            dir.path(),
            &format!("{package}/Cargo.toml"),
            &format!("[package]\nname='{package}'\nversion='0.1.0'\nedition='2024'\n"),
        );
        write(
            dir.path(),
            &format!("{package}/src/lib.rs"),
            "pub fn value() {}\n",
        );
    }
    lock(dir.path());
    dir
}

#[test]
fn standard_project_configuration_has_real_context_consumers_not_compiler_claims() {
    let dir = fixture("", "pub fn value() {}\n");
    write(dir.path(), ".beads/config.yaml", "sync: true\n");
    write(dir.path(), ".beads/metadata.json", "{}\n");
    write(dir.path(), ".envrc", "use flake\n");
    write(dir.path(), "wrix.toml", "[profiles]\n");
    let inventory = inventory(dir.path());
    for resource in [
        ".beads/config.yaml",
        ".beads/metadata.json",
        ".envrc",
        "wrix.toml",
    ] {
        let target = name(&format!("resource:{resource}"));
        assert_eq!(
            inventory.subject(&target).unwrap().role(),
            Role::Configuration
        );
        assert!(
            inventory
                .obligations()
                .iter()
                .any(|obligation| obligation.subject() == &target
                    && obligation.responsible() == &name("project:context")
                    && obligation.duty() == Duty::Check)
        );
        assert!(
            inventory
                .obligations()
                .iter()
                .filter(|obligation| obligation.subject() == &target)
                .all(|obligation| !matches!(
                    obligation.duty(),
                    Duty::Build | Duty::Lint | Duty::Test
                ))
        );
    }
}

#[test]
fn native_fixture_configuration_keeps_its_consumer_instead_of_becoming_project_data() {
    let dir = fixture("", "pub fn value() {}\n");
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname='fixture'\nversion='0.1.0'\nedition='2024'\n[[package.metadata.loom.verify.resource]]\nname='cargo:fixture:resource:policy-fixture'\ndirectory='.beads'\nrole='test-fixture'\nconsumers=['cargo:fixture:lib:fixture']\n",
    );
    write(
        dir.path(),
        ".beads/config.yaml",
        "not valid configuration: [\n",
    );
    lock(dir.path());
    let inventory = inventory(dir.path());
    assert_eq!(
        inventory
            .subject(&name("cargo:fixture:resource:policy-fixture"))
            .unwrap()
            .role(),
        Role::TestFixture
    );
    assert!(
        !inventory
            .subject(&name("project:context"))
            .unwrap()
            .members()
            .contains(&path(".beads/config.yaml"))
    );
    assert!(inventory.obligations().iter().any(|obligation| {
        obligation.subject() == &name("cargo:fixture:resource:policy-fixture")
            && obligation.duty() == Duty::Test
            && obligation.responsible() == &name("cargo:fixture:lib:fixture")
    }));
}

#[test]
fn unknown_configuration_without_an_actual_reader_cannot_invent_cargo_ownership() {
    let dir = fixture("", "pub fn value() {}\n");
    write(dir.path(), "unowned-settings.yaml", "setting: true\n");
    let snapshot = Snapshot::discover(dir.path()).unwrap();
    assert!(matches!(
        Inventory::discover(&snapshot),
        Err(Error::Responsibility { .. })
    ));
}

#[test]
fn extensionless_native_cargo_configuration_remains_a_compiler_input() {
    let dir = fixture("", "pub fn value() {}\n");
    write(dir.path(), ".cargo/config", "[build]\njobs=2\n");
    let inventory = inventory(dir.path());
    assert!(
        inventory
            .checker_subjects(&path("src/lib.rs"))
            .unwrap()
            .contains(&name("resource:.cargo/config"))
    );
}

#[test]
fn fixture_membership_cannot_make_a_checker_production_code() {
    let dir = fixture("", "pub fn value() {}\n");
    write(
        dir.path(),
        "tests/fixtures/checker.rs",
        "intentionally invalid Rust",
    );
    let inventory = inventory(dir.path());
    assert!(matches!(
        inventory.checker_subjects(&path("tests/fixtures/checker.rs")),
        Err(Error::Checker { .. })
    ));
}

#[test]
fn native_include_accounts_real_non_rust_compiler_input_without_a_file_registry() {
    let dir = fixture(
        "",
        "pub const SCRIPT: &str = include_str!(\"../tools/helper.sh\");\n",
    );
    write(dir.path(), "tools/helper.sh", "set -euo pipefail\n");
    let inventory = inventory(dir.path());
    assert!(
        inventory
            .subject(&name("cargo:fixture:lib:fixture"))
            .unwrap()
            .members()
            .contains(&path("tools/helper.sh"))
    );
}

#[test]
fn workspace_resource_uses_current_directory_members_and_discovered_test_consumer() {
    let declaration = "[[workspace.metadata.loom.verify.resource]]\nname='workspace:cases'\ndirectory='cases'\nrole='test-fixture'\nconsumers=['cargo:checker:test:cases']\n";
    let dir = workspace(declaration);
    write(
        dir.path(),
        "checker/tests/cases.rs",
        "#[test] fn cases() {}\n",
    );
    write(dir.path(), "cases/first.rs", "intentionally invalid Rust");
    let before = inventory(dir.path());
    write(dir.path(), "cases/second.rs", "another invalid fixture");
    let after_add = inventory(dir.path());
    assert_eq!(
        after_add
            .subject(&name("workspace:cases"))
            .unwrap()
            .members()
            .len(),
        2
    );
    assert_ne!(before.snapshot(), after_add.snapshot());
    std::fs::rename(
        dir.path().join("cases/first.rs"),
        dir.path().join("cases/renamed.rs"),
    )
    .unwrap();
    std::fs::remove_file(dir.path().join("cases/second.rs")).unwrap();
    let after = inventory(dir.path());
    assert_eq!(
        after.subject(&name("workspace:cases")).unwrap().members(),
        &BTreeSet::from([path("cases/renamed.rs")])
    );
    let obligations: Vec<_> = after
        .obligations()
        .iter()
        .filter(|obligation| obligation.subject() == &name("workspace:cases"))
        .collect();
    assert_eq!(obligations.len(), 1);
    assert_eq!(
        obligations[0].responsible(),
        &name("cargo:checker:test:cases")
    );
    assert_eq!(obligations[0].duty(), Duty::Test);
}

#[test]
fn workspace_resource_cannot_reference_a_nonexistent_consumer() {
    let dir = workspace(
        "[[workspace.metadata.loom.verify.resource]]\nname='workspace:cases'\ndirectory='cases'\nrole='test-fixture'\nconsumers=['cargo:absent:test:cases']\n",
    );
    write(dir.path(), "cases/invalid.rs", "invalid Rust");
    assert!(matches!(
        Inventory::discover(&Snapshot::discover(dir.path()).unwrap()),
        Err(Error::Unknown { .. })
    ));
}

#[test]
fn workspace_resource_without_a_consumer_blocks_instead_of_dropping_obligations() {
    let dir = workspace(
        "[[workspace.metadata.loom.verify.resource]]\nname='workspace:cases'\ndirectory='cases'\nrole='test-fixture'\n",
    );
    write(dir.path(), "cases/invalid.rs", "invalid Rust");
    assert!(matches!(
        Inventory::discover(&Snapshot::discover(dir.path()).unwrap()),
        Err(Error::Responsibility { .. })
    ));
}

#[test]
fn malformed_workspace_resource_retains_manifest_error_context() {
    let dir = workspace("[workspace.metadata.loom.verify]\ntrusted=true\n");
    let error = Inventory::discover(&Snapshot::discover(dir.path()).unwrap()).unwrap_err();
    assert!(matches!(error, Error::Definition { .. }));
    assert!(error.to_string().contains("Cargo.toml"));
}

#[test]
fn checker_closure_keeps_normal_build_transitive_sources_but_not_test_only_packages() {
    let dir = workspace("");
    write(
        dir.path(),
        "checker/Cargo.toml",
        "[package]\nname='checker'\nversion='0.1.0'\nedition='2024'\n[dependencies]\nrenamed={package='normal',path='../normal'}\n[build-dependencies]\nbuilder={path='../builder'}\n[dev-dependencies]\ndevonly={path='../devonly'}\n",
    );
    write(
        dir.path(),
        "normal/Cargo.toml",
        "[package]\nname='normal'\nversion='0.1.0'\nedition='2024'\n[dependencies]\ntransitive={path='../transitive'}\n",
    );
    write(dir.path(), "checker/build.rs", "fn main() {}\n");
    write(dir.path(), "checker/src/main.rs", "fn main() {}\n");
    write(dir.path(), "normal/src/main.rs", "fn main() {}\n");
    write(dir.path(), "normal/build.rs", "fn main() {}\n");
    write(
        dir.path(),
        "checker/tests/unrelated.rs",
        "#[test] fn unrelated() {}\n",
    );
    write(
        dir.path(),
        "checker/tests/fixtures/invalid.rs",
        "not source",
    );
    write(
        dir.path(),
        "checker/examples/unrelated.rs",
        "fn main() {}\n",
    );
    write(
        dir.path(),
        "normal/src/lib.rs",
        "pub const DATA: &[u8] = include_bytes!(\"../native.dat\");\n",
    );
    write(dir.path(), "normal/native.dat", "native input");
    write(dir.path(), ".cargo/config.toml", "[build]\njobs=2\n");
    write(
        dir.path(),
        "clippy.toml",
        "too-many-arguments-threshold=8\n",
    );
    write(
        dir.path(),
        "unrelated/clippy.toml",
        "too-many-arguments-threshold=9\n",
    );
    lock(dir.path());
    let inventory = inventory(dir.path());
    let closure = inventory
        .checker_subjects(&path("checker/src/main.rs"))
        .unwrap();
    assert_eq!(
        closure,
        BTreeSet::from([
            name("cargo:checker:lib:checker"),
            name("cargo:checker:bin:checker"),
            name("cargo:checker:custom-build:build-script-build"),
            name("cargo:normal:lib:normal"),
            name("cargo:normal:custom-build:build-script-build"),
            name("cargo:builder:lib:builder"),
            name("cargo:transitive:lib:transitive"),
            name("resource:Cargo.toml"),
            name("resource:Cargo.lock"),
            name("resource:.cargo/config.toml"),
            name("resource:clippy.toml"),
            name("resource:checker/Cargo.toml"),
            name("resource:normal/Cargo.toml"),
            name("resource:builder/Cargo.toml"),
            name("resource:transitive/Cargo.toml"),
        ])
    );
    assert!(closure.iter().any(|name| {
        inventory
            .subject(name)
            .unwrap()
            .members()
            .contains(&path("normal/native.dat"))
    }));
}

#[test]
fn external_path_dependencies_cannot_be_silently_omitted_from_checker_compilation() {
    let external = tempfile::tempdir().unwrap();
    write(
        external.path(),
        "Cargo.toml",
        "[package]\nname='external'\nversion='0.1.0'\nedition='2024'\n",
    );
    write(external.path(), "src/lib.rs", "pub fn external() {}\n");
    let dir = fixture(
        &format!(
            "[dependencies]\nexternal={{path='{}'}}\n",
            external.path().display()
        ),
        "pub fn value() {}\n",
    );
    let inventory = inventory(dir.path());
    assert!(matches!(
        inventory.checker_subjects(&path("src/lib.rs")),
        Err(Error::Outside { .. })
    ));
}

#[test]
fn checker_compilation_graph_survives_spec_owned_role_resolution() {
    let dir = fixture("", "pub fn value() {}\n");
    strategy(dir.path(), "cargo:fixture:lib:fixture", "test-fixture");
    let inventory = inventory(dir.path());
    assert!(
        inventory
            .checker_subjects(&path("src/lib.rs"))
            .unwrap()
            .contains(&name("cargo:fixture:lib:fixture"))
    );
}

#[test]
fn unknown_checker_path_is_not_a_whole_workspace_compilation_request() {
    let dir = fixture("", "pub fn value() {}\n");
    let inventory = inventory(dir.path());
    assert!(matches!(
        inventory.checker_subjects(&path("absent.rs")),
        Err(Error::Checker { .. })
    ));
}

#[test]
fn nix_fixture_consumers_retain_evaluation_and_check_without_cargo_test() {
    let dir = fixture("", "pub fn value() {}\n");
    write(
        dir.path(),
        "flake.nix",
        "{ outputs = { self }: { checks.x86_64-linux.native = throw \"must not force execution\"; }; }\n",
    );
    strategy(dir.path(), "nix:flake:flake.nix", "test-fixture");
    let inventory = inventory(dir.path());
    let duties: BTreeSet<_> = inventory
        .obligations()
        .iter()
        .filter(|obligation| obligation.subject() == &name("nix:flake:flake.nix"))
        .map(loom_gate::inventory::Obligation::duty)
        .collect();
    assert_eq!(duties, BTreeSet::from([Duty::Evaluate, Duty::Check]));
}

#[test]
fn generated_resource_cannot_fabricate_a_producer() {
    let dir = fixture(
        "[[package.metadata.loom.verify.resource]]\nname='generated:data'\ndirectory='generated'\nrole='generated'\nconsumers=['cargo:fixture:lib:fixture']\nproducer='cargo:fixture:custom-build:absent'\n",
        "pub fn value() {}\n",
    );
    write(dir.path(), "generated/data.bin", "generated");
    assert!(matches!(
        Inventory::discover(&Snapshot::discover(dir.path()).unwrap()),
        Err(Error::Unknown { .. })
    ));
}

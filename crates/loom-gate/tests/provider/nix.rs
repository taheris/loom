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

pub fn native_project(dir: &TempDir, options: &str, source: bool, pinned: bool) {
    write(
        dir.path(),
        "verification.nix",
        include_str!("../../../../nix/verification.nix"),
    );
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
  outputs = {{ self }}: let
    checked = import ./verification.nix;
    source = checked.resource {{
      root = ./.;
      filter = path: type: type == "directory" || path == toString (./. + "/README.md");
    }};
  in {{
    checks.x86_64-linux.heading = checked.check {{
      resources = [ source ];
      build = sources: builtins.derivation {{
      name = "loom-provider-heading";
      system = "x86_64-linux";
      builder = {builder};
      args = [ "-c" "read -r heading < \"$src/README.md\"; [[ \"$heading\" == '# '* ]] && echo success > \"$out\"" ];
      {source}
      {options}
      }};
    }};
  }};
}}
"#,
            source = if source {
                "src = builtins.head sources;"
            } else {
                ""
            }
        ),
    );
}

#[test]
fn readonly_derivation_sources_retain_complete_native_graph_identity() {
    let dir = fixture();
    let platform = loom_gate::runner::result::current_platform();
    native_project(
        &dir,
        &format!(
            "recipe = builtins.unsafeDiscardOutputDependency self.packages.{platform}.recipe-data.drvPath;"
        ),
        true,
        true,
    );
    let original = std::fs::read_to_string(dir.path().join("flake.nix"))
        .unwrap()
        .replace("x86_64-linux", &platform);
    let recipe = format!(
        r#"
    packages.{platform}.recipe-data = builtins.derivation {{
      name = "readonly-recipe-data";
      __noChroot = true;
      inherit (self.checks.{platform}.heading) builder system;
      src = source.path;
      args = [ "-c" "printf metadata > \"$out\"" ];
    }};
"#
    );
    write(
        dir.path(),
        "flake.nix",
        &original.replace(
            &format!("    checks.{platform}.heading"),
            &(recipe + &format!("    checks.{platform}.heading")),
        ),
    );
    let (snapshot, inventory) = context(&dir);
    let subject = name(&format!("nix:check:flake.nix:{platform}:heading"));
    let first =
        Definition::hermetic_baseline(&snapshot, &inventory, name("first"), &subject).unwrap();
    let second =
        Definition::hermetic_baseline(&snapshot, &inventory, name("second"), &subject).unwrap();
    let graph = first.nix_graph().unwrap();
    assert!(
        graph
            .dependencies()
            .iter()
            .any(|path| path.as_str().ends_with("-readonly-recipe-data.drv"))
    );
    assert_eq!(graph.identity(), second.nix_graph().unwrap().identity());
}

#[test]
fn native_metadata_discovery_cannot_execute_import_from_derivation() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    let platform = loom_gate::runner::result::current_platform();
    let original = std::fs::read_to_string(dir.path().join("flake.nix"))
        .unwrap()
        .replace("x86_64-linux", &platform);
    let nonce = serde_json::to_string(dir.path().to_str().unwrap()).unwrap();
    let probe = format!(
        r#"
    packages.{platform}.probe = builtins.derivation {{
      name = "metadata-must-not-build"; nonce = {nonce};
      inherit (self.checks.{platform}.heading) builder system;
      args = [ "-c" "printf '{{}}' > \"$out\"" ];
    }};
    checks.{platform}.hidden = import self.packages.{platform}.probe;
"#
    );
    write(
        dir.path(),
        "flake.nix",
        &original.replace(
            &format!("    checks.{platform}.heading"),
            &(probe + &format!("    checks.{platform}.heading")),
        ),
    );
    let output = std::process::Command::new("nix")
        .args([
            "eval",
            "--raw",
            &format!(
                "path:{}#packages.{platform}.probe.outPath",
                dir.path().display()
            ),
            "--offline",
            "--no-write-lock-file",
            "--option",
            "allow-import-from-derivation",
            "false",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let artifact = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    assert!(!artifact.exists());
    let snapshot = loom_gate::inventory::snapshot::Snapshot::discover(dir.path()).unwrap();
    match loom_gate::inventory::Inventory::discover(&snapshot) {
        Ok(inventory) => assert!(
            inventory
                .obligations()
                .iter()
                .any(|obligation| obligation.subject().as_str()
                    == format!("nix:check:flake.nix:{platform}:hidden"))
        ),
        Err(loom_gate::inventory::Error::Discovery { detail, .. }) => {
            assert!(detail.contains("allow-import-from-derivation"), "{detail}");
        }
        result => assert!(result.is_ok(), "{result:?}"),
    }
    assert!(
        !artifact.exists(),
        "metadata discovery executed an import-from-derivation producer"
    );
}

#[test]
fn foreign_native_baseline_observes_applicability_without_constructing_foreign_recipes() {
    let dir = fixture();
    write(
        dir.path(),
        "flake.lock",
        r#"{"nodes":{"root":{}},"root":"root","version":7}"#,
    );
    write(
        dir.path(),
        "flake.nix",
        "{ outputs = { self }: { checks.unsupported-loom-platform.unforced = throw \"foreign recipe must not be evaluated\"; packages.unsupported-loom-platform.peer = throw \"peer recipe must not be evaluated\"; }; }\n",
    );
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Platform observation [system](platform-observation)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let subject = name("nix:check:flake.nix:unsupported-loom-platform:unforced");
    let definition = Definition::hermetic_baseline(
        &snapshot,
        &inventory,
        name("platform-observation"),
        &subject,
    )
    .unwrap();
    assert!(definition.nix_graph().is_none());
    assert!(
        definition
            .description()
            .responsibilities()
            .iter()
            .any(
                |responsibility| responsibility.subject().as_str() == "resource:flake.lock"
                    && responsibility.responsible() == &subject
                    && responsibility.duty() == Duty::Evaluate
            )
    );
    assert!(
        definition
            .description()
            .inputs()
            .iter()
            .any(|input| input.path().as_path() == std::path::Path::new("flake.lock"))
    );
    assert!(
        definition
            .description()
            .responsibilities()
            .iter()
            .any(
                |responsibility| responsibility.subject().as_str() == "nix:flake.nix"
                    && responsibility.duty() == Duty::Evaluate
            )
    );
    assert!(definition.native_applicability_covers(&name(
        "nix:package:flake.nix:unsupported-loom-platform:peer"
    )));
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
    assert_eq!(unit.baseline_tier(), loom_gate::Tier::System);
    let observed = unit.run().unwrap();
    assert!(
        matches!(observed.outcome(), Outcome::Inapplicable { system } if system.as_str() == "unsupported-loom-platform")
    );
    assert!(
        observed
            .observations()
            .iter()
            .any(|observation| matches!(observation, Observation::NativePlatform { .. }))
    );
    assert!(
        !observed
            .observations()
            .iter()
            .any(|observation| matches!(observation, Observation::FreshExecution { .. }))
    );
    let mut forged = serde_json::to_value(candidate).unwrap();
    forged["execution"]["checker"]["library"]["native-platform"]["system"] =
        serde_json::json!(loom_gate::runner::result::current_platform());
    let forged = Candidate::parse(&serde_json::to_string(&forged).unwrap()).unwrap();
    assert!(matches!(
        registry.resolve(
            &snapshot,
            &inventory,
            &forged,
            path("specs/example/tests.md")
        ),
        Err(Error::Inconsistent { .. })
    ));
}

#[test]
fn foreign_applicability_cannot_cover_sources_with_applicable_consumers() {
    let dir = fixture();
    let platform = loom_gate::runner::result::current_platform();
    write(
        dir.path(),
        "flake.nix",
        &format!(
            "{{ outputs = {{ self }}: {{ checks.{platform}.active = throw \"applicable construction must reject this\"; checks.unsupported-loom-platform.foreign = throw \"foreign body stays unforced\"; }}; }}\n"
        ),
    );
    let (snapshot, inventory) = context(&dir);
    let definition = Definition::hermetic_baseline(
        &snapshot,
        &inventory,
        name("foreign"),
        &name("nix:check:flake.nix:unsupported-loom-platform:foreign"),
    )
    .unwrap();
    assert!(
        !definition
            .description()
            .responsibilities()
            .iter()
            .any(|responsibility| responsibility.subject().as_str() == "nix:flake.nix")
    );
    assert!(
        Definition::hermetic_baseline(
            &snapshot,
            &inventory,
            name("active"),
            &name(&format!("nix:check:flake.nix:{platform}:active"))
        )
        .is_err()
    );
}

#[test]
fn native_app_baseline_checks_its_program_artifact_without_authorizing_app_execution() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    let platform = loom_gate::runner::result::current_platform();
    let tool = bash();
    let root: PathBuf = tool.components().take(4).collect();
    let root = serde_json::to_string(root.to_str().unwrap()).unwrap();
    let app = format!(
        r#"
    apps.{platform}.tool = checked.app {{
      pkgs = {{ bash = builtins.appendContext {root} {{ {root} = {{ path = true; }}; }}; stdenv.hostPlatform.system = "{platform}"; }};
      package = self.checks.{platform}.heading;
      name = "tool";
    }};
"#
    );
    let original = std::fs::read_to_string(dir.path().join("flake.nix"))
        .unwrap()
        .replace("x86_64-linux", &platform);
    let flake = original.replace(
        &format!("    checks.{platform}.heading"),
        &(app + &format!("    checks.{platform}.heading")),
    );
    write(dir.path(), "flake.nix", &flake);
    let (snapshot, inventory) = context(&dir);
    let subject = name(&format!("nix:app:flake.nix:{platform}:tool"));
    let definition =
        Definition::hermetic_baseline(&snapshot, &inventory, name("app-artifact"), &subject)
            .unwrap();
    assert!(
        definition
            .description()
            .responsibilities()
            .iter()
            .any(|responsibility| responsibility.subject() == &subject
                && responsibility.duty() == Duty::Check)
    );
    assert!(matches!(
        Definition::hermetic(
            &snapshot,
            &inventory,
            name("run-app"),
            &subject,
            &BTreeSet::from([subject.clone()]),
            &BTreeSet::from([Duty::Check])
        ),
        Err(Error::Nix(NixError::App { .. }))
    ));
    // Bash startup hooks cannot turn the fixed artifact recipe into app execution.
    let verification = std::fs::read_to_string(dir.path().join("verification.nix")).unwrap();
    write(
        dir.path(),
        "verification.nix",
        &verification.replace(
            "LOOM_APP_PROGRAM = program;",
            "LOOM_APP_PROGRAM = program; BASH_ENV = program;",
        ),
    );
    let injected = loom_gate::inventory::snapshot::Snapshot::discover(dir.path()).unwrap();
    assert!(
        matches!(loom_gate::inventory::Inventory::discover(&injected), Err(loom_gate::inventory::Error::NativeResources { source, .. }) if matches!(source.as_ref(), NixError::Binding { source, .. } if matches!(source.as_ref(), NixError::AppDefinition)))
    );
    write(dir.path(), "verification.nix", &verification);
    // The actual app value cannot replace the checked artifact with a host command.
    write(
        dir.path(),
        "flake.nix",
        &flake
            .replace(" = checked.app {", " = (checked.app {")
            .replace(
                "      name = \"tool\";\n    };",
                "      name = \"tool\";\n    }) // { program = \"/bin/sh\"; };",
            ),
    );
    let changed = loom_gate::inventory::snapshot::Snapshot::discover(dir.path()).unwrap();
    assert!(
        matches!(loom_gate::inventory::Inventory::discover(&changed), Err(loom_gate::inventory::Error::NativeResources { source, .. }) if matches!(source.as_ref(), NixError::Binding { source, .. } if matches!(source.as_ref(), NixError::StorePath)))
    );
}

#[test]
fn removing_package_constructor_metadata_cannot_remove_native_obligations() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    let platform = loom_gate::runner::result::current_platform();
    let flake = std::fs::read_to_string(dir.path().join("flake.nix"))
        .unwrap()
        .replace("x86_64-linux", &platform);
    write(dir.path(), "flake.nix", &flake.replace(&format!("    checks.{platform}.heading"), &format!("    packages.{platform}.artifact = builtins.removeAttrs self.checks.{platform}.heading [ \"loom\" ];\n    checks.{platform}.heading")));
    let (snapshot, inventory) = context(&dir);
    let subject = name(&format!("nix:package:flake.nix:{platform}:artifact"));
    for duty in [Duty::Evaluate, Duty::Check] {
        assert!(
            inventory
                .obligations()
                .iter()
                .any(|obligation| obligation.subject() == &subject && obligation.duty() == duty)
        );
    }
    assert!(
        Definition::hermetic_baseline(&snapshot, &inventory, subject.clone(), &subject).is_err()
    );
}

#[test]
fn native_dependency_provenance_is_transitive_but_never_shared_with_unrelated_roots() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    let platform = loom_gate::runner::result::current_platform();
    let wrappers = format!(
        r#"
    checks.{platform}.alpha = checked.check {{
      resources = [];
      evaluation = [ (checked.resource {{ root = ./.; origin = "verification.nix"; }}) ];
      build = _: builtins.derivation {{
        name = "transitive-reader";
        inherit (self.checks.{platform}.heading) system builder;
        dependency = self.checks.{platform}.heading;
        args = [ "-c" "echo alpha > $out" ];
      }};
    }};
    checks.{platform}.beta = checked.check {{
      resources = [];
      evaluation = [ (checked.resource {{ root = ./.; origin = "verification.nix"; }}) ];
      build = _: builtins.derivation {{
        name = "unrelated-reader";
        inherit (self.checks.{platform}.heading) system builder;
        args = [ "-c" "echo beta > $out" ];
      }};
    }};
"#
    );
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(
        dir.path(),
        "flake.nix",
        &flake
            .replace(
                "    checks.x86_64-linux.heading",
                &(wrappers + "    checks.x86_64-linux.heading"),
            )
            .replace("x86_64-linux", &platform),
    );
    let (snapshot, inventory) = context(&dir);
    for (attribute, reads) in [("alpha", true), ("beta", false)] {
        let subject = name(&format!("nix:check:flake.nix:{platform}:{attribute}"));
        let definition =
            Definition::hermetic_baseline(&snapshot, &inventory, subject.clone(), &subject)
                .unwrap();
        assert_eq!(
            definition
                .description()
                .inputs()
                .iter()
                .any(|input| input.path() == &path("README.md")),
            reads
        );
    }
}

#[test]
fn explicit_native_rust_inputs_preserve_cargo_and_orphan_obligations() {
    use loom_gate::inventory::snapshot::Snapshot;
    use loom_gate::inventory::{Inventory, Role};
    let dir = fixture();
    native_project(
        &dir,
        "tool = builtins.elemAt sources 1; cargo_source = builtins.elemAt sources 2;",
        true,
        true,
    );
    write(dir.path(), "support/probe.rs", "fn main() {}\n");
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap()
        .replace("resources = [ source ];", "resources = [ source (checked.resource { root = ./.; origin = \"support/probe.rs\"; }) (checked.resource { root = ./.; origin = \"src/lib.rs\"; }) ];")
        .replace("read -r heading", "read -r ignored < \\\"$tool\\\"; read -r local_source < \\\"$cargo_source\\\"; read -r heading")
        .replace("x86_64-linux", &loom_gate::runner::result::current_platform());
    write(dir.path(), "flake.nix", &flake);
    let (snapshot, inventory) = context(&dir);
    let resource = inventory
        .subjects()
        .values()
        .find(|subject| subject.members().contains(&path("support/probe.rs")))
        .unwrap();
    assert_eq!(resource.role(), Role::RuntimeResource);
    let native = name(&format!(
        "nix:check:flake.nix:{}:heading",
        loom_gate::runner::result::current_platform()
    ));
    Definition::hermetic_baseline(&snapshot, &inventory, native.clone(), &native).unwrap();
    assert!(
        inventory
            .obligations()
            .iter()
            .any(|obligation| obligation.subject() == resource.name()
                && obligation.duty() == Duty::Check)
    );
    for subject in inventory
        .subjects()
        .values()
        .filter(|subject| subject.role() == Role::RustSource)
    {
        for duty in [Duty::Build, Duty::Lint, Duty::Test] {
            assert!(
                inventory
                    .obligations()
                    .iter()
                    .any(|obligation| obligation.subject() == subject.name()
                        && obligation.duty() == duty)
            );
        }
    }
    drop(snapshot);
    // A directory resource cannot claim arbitrary new Rust code as a foreign input.
    write(
        dir.path(),
        "flake.nix",
        &flake.replace("origin = \"support/probe.rs\"", "origin = \"support\""),
    );
    let snapshot = Snapshot::discover(dir.path()).unwrap();
    assert!(
        matches!(Inventory::discover(&snapshot), Err(loom_gate::inventory::Error::Orphan { path: actual }) if actual == path("support/probe.rs"))
    );
}

#[test]
fn native_resource_discovery_reuses_only_current_constructor_facts() {
    let dir = fixture();
    native_project(
        &dir,
        "LOOM_SCHEMA = builtins.readFile ./settings.dat;",
        true,
        true,
    );
    write(dir.path(), "settings.dat", "actual evaluator input\n");
    let system = loom_gate::runner::result::current_platform();
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(dir.path(), "flake.nix", &flake.replace("x86_64-linux", &system).replace(
        "resources = [ source ];",
        "resources = [ source ]; evaluation = [ (checked.resource { root = ./.; origin = \"settings.dat\"; }) ];",
    ));
    let (snapshot, inventory) = context(&dir);
    let subject = name(&format!("nix:check:flake.nix:{system}:heading"));
    let first =
        Definition::hermetic_baseline(&snapshot, &inventory, name("first"), &subject).unwrap();
    let second =
        Definition::hermetic_baseline(&snapshot, &inventory, name("second"), &subject).unwrap();
    assert!(std::ptr::eq(
        first.nix_graph().unwrap(),
        second.nix_graph().unwrap()
    ));
    assert_ne!(first.description(), second.description());
    let settings = name("resource:settings.dat");
    assert_eq!(
        inventory.subject(&settings).unwrap().role(),
        loom_gate::inventory::Role::Configuration
    );
    for duty in [Duty::Evaluate, Duty::Check] {
        assert!(
            first
                .description()
                .responsibilities()
                .iter()
                .any(|responsibility| responsibility.subject() == &settings
                    && responsibility.responsible() == &subject
                    && responsibility.duty() == duty)
        );
    }
    write(dir.path(), "README.md", "# Changed source\n");
    assert!(Definition::hermetic_baseline(&snapshot, &inventory, name("stale"), &subject).is_err());
    let (changed, changed_inventory) = context(&dir);
    let fresh =
        Definition::hermetic_baseline(&changed, &changed_inventory, name("fresh"), &subject)
            .unwrap();
    assert_ne!(
        first.nix_graph().unwrap().identity(),
        fresh.nix_graph().unwrap().identity()
    );
}

fn add_sibling_native_check(dir: &TempDir, resources: &str, attributes: &str) {
    let system = loom_gate::runner::result::current_platform();
    let tool = bash();
    let store: PathBuf = tool.components().take(4).collect();
    let builder = serde_json::to_string(tool.to_str().unwrap()).unwrap();
    let store = serde_json::to_string(store.to_str().unwrap()).unwrap();
    let extra = format!(
        r#"
    checks.{system}.second = checked.check {{
      resources = {resources};
      build = sources: builtins.derivation {{
        name = "native-sibling";
        system = "{system}";
        builder = builtins.appendContext {builder} {{ {store} = {{ path = true; }}; }};
        src = builtins.head sources;
        args = [ "-c" ''for input in "$src"/*; do read -r actual < "$input"; done; printf '%s' "$actual" > "$out"'' ];
        {attributes}
      }};
    }};
"#
    );
    let flake = std::fs::read_to_string(dir.path().join("flake.nix"))
        .unwrap()
        .replace("x86_64-linux", &system);
    let position = flake.rfind("  };").unwrap();
    write(
        dir.path(),
        "flake.nix",
        &format!("{}{}{}", &flake[..position], extra, &flake[position..]),
    );
}

#[test]
fn batched_native_construction_keeps_distinct_source_partitions() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    write(dir.path(), "data/config.data", "actual native input\n");
    add_sibling_native_check(
        &dir,
        "[ (checked.resource { root = ./.; origin = \"data\"; }) ]",
        "",
    );
    let (snapshot, inventory) = context(&dir);
    let system = loom_gate::runner::result::current_platform();
    let first = name(&format!("nix:check:flake.nix:{system}:heading"));
    let second = name(&format!("nix:check:flake.nix:{system}:second"));
    let first =
        Definition::hermetic_baseline(&snapshot, &inventory, name("first"), &first).unwrap();
    let second =
        Definition::hermetic_baseline(&snapshot, &inventory, name("second"), &second).unwrap();
    assert!(
        first
            .nix_graph()
            .unwrap()
            .origins()
            .contains(&path("README.md"))
    );
    assert!(
        !first
            .nix_graph()
            .unwrap()
            .origins()
            .contains(&path("data/config.data"))
    );
    assert!(
        second
            .nix_graph()
            .unwrap()
            .origins()
            .contains(&path("data/config.data"))
    );
    assert!(
        !second
            .nix_graph()
            .unwrap()
            .origins()
            .contains(&path("README.md"))
    );
}

#[test]
fn batched_native_construction_rejects_an_invalid_later_sibling_atomically() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    write(
        dir.path(),
        "undeclared.bin",
        "must not disappear silently\n",
    );
    add_sibling_native_check(
        &dir,
        "[ source ]",
        "LOOM_UNDECLARED = builtins.pathExists ./undeclared.bin;",
    );
    let snapshot = loom_gate::inventory::snapshot::Snapshot::discover(dir.path()).unwrap();
    assert!(
        matches!(loom_gate::inventory::Inventory::discover(&snapshot), Err(loom_gate::inventory::Error::NativeResources { source, .. }) if matches!(source.as_ref(), NixError::Binding { source, .. } if matches!(source.as_ref(), NixError::Partition)))
    );
}

#[tokio::test]
async fn independently_constructed_native_aliases_share_the_current_execution_graph() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    let foreign = if std::env::consts::OS == "macos" {
        "x86_64-linux"
    } else {
        "aarch64-darwin"
    };
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(
        dir.path(),
        "flake.nix",
        &flake.replace("x86_64-linux", foreign),
    );
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Accounting [check](accounting)\n- First [system](first)\n- Second [system](second)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let subject = name(&format!("nix:check:flake.nix:{foreign}:heading"));
    let first =
        Definition::hermetic_baseline(&snapshot, &inventory, name("first"), &subject).unwrap();
    let second =
        Definition::hermetic_baseline(&snapshot, &inventory, name("second"), &subject).unwrap();
    let registry = Registry::new(
        &snapshot,
        vec![super::plan::offered(&inventory, &snapshot), first, second],
    )
    .unwrap();
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    let scope = loom_gate::scope::Request::Tree
        .resolve(dir.path())
        .await
        .unwrap();
    let selected = plan
        .select_tiers(&scope, &[loom_gate::Tier::System])
        .unwrap();
    for _ in 0..2 {
        let report = selected.run_report().unwrap();
        assert_eq!(report.fresh_invocations(), 1);
        assert_eq!(report.shared_invocations(), 1);
        assert_eq!(report.invocations().len(), 2);
        for invocation in report.invocations() {
            assert!(matches!(
                invocation.observed().outcome(),
                Outcome::Inapplicable { .. }
            ));
            assert_eq!(invocation.owners().len(), 1);
        }
    }
}

#[test]
fn explicit_rust_responsibility_requires_the_actual_fixed_cargo_operation() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(
        dir.path(),
        "flake.nix",
        &flake.replace(
            "path == toString (./. + \"/README.md\")",
            "builtins.elem (baseNameOf path) [ \"README.md\" \"Cargo.toml\" \"Cargo.lock\" \"lib.rs\" ]",
        ),
    );
    let (snapshot, inventory) = context(&dir);
    for duty in [Duty::Build, Duty::Lint, Duty::Test] {
        let result = Definition::hermetic(
            &snapshot,
            &inventory,
            name("heading"),
            &name("nix:check:flake.nix:x86_64-linux:heading"),
            &BTreeSet::from([name("cargo:provider-fixture:lib:provider_fixture")]),
            &BTreeSet::from([duty]),
        );
        assert!(matches!(result, Err(Error::Nix(NixError::CargoExecution))));
    }
}

#[test]
fn evaluator_only_native_definitions_do_not_invent_runtime_or_rust_responsibility() {
    let dir = fixture();
    native_project(&dir, "", false, true);
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    let flake = flake.replace("resources = [ source ];", r#"resources = [ ];
      evaluation = map (origin: checked.resource { root = ./.; inherit origin; }) [ "flake.nix" "verification.nix" ];"#);
    let flake = flake
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("args =") {
                r#"      args = [ "-c" "printf evaluated > \"$out\"" ];"#
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    write(dir.path(), "flake.nix", &flake);
    let (snapshot, inventory) = context(&dir);
    let native = name("nix:check:flake.nix:x86_64-linux:heading");
    let definition =
        Definition::hermetic_baseline(&snapshot, &inventory, name("heading"), &native).unwrap();
    assert!(
        definition
            .description()
            .responsibilities()
            .iter()
            .any(|responsibility| responsibility.duty() == Duty::Evaluate)
    );
    assert!(
        definition
            .description()
            .responsibilities()
            .iter()
            .all(|responsibility| matches!(responsibility.duty(), Duty::Evaluate | Duty::Check))
    );
    assert!(
        definition
            .description()
            .inputs()
            .iter()
            .all(|input| input.path() != &path("README.md"))
    );
}

#[test]
fn native_runtime_resources_are_discovered_from_actual_consumers_not_file_suffixes() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(
        dir.path(),
        "flake.nix",
        &flake.replace("README.md", "opaque-helper"),
    );
    write(dir.path(), "opaque-helper", "# Actual runtime resource\n");
    let (snapshot, inventory) = context(&dir);
    let resource = name("resource:opaque-helper");
    assert_eq!(
        inventory.subject(&resource).unwrap().role(),
        loom_gate::inventory::Role::RuntimeResource
    );
    let native = name("nix:check:flake.nix:x86_64-linux:heading");
    assert!(inventory.obligations().iter().any(|obligation| {
        obligation.subject() == &resource
            && obligation.responsible() == &native
            && obligation.duty() == Duty::Check
    }));
    let definition =
        Definition::hermetic_baseline(&snapshot, &inventory, name("heading"), &native).unwrap();
    assert!(
        definition
            .description()
            .responsibilities()
            .iter()
            .any(|responsibility| {
                responsibility.subject() == &resource && responsibility.responsible() == &native
            })
    );
    write(
        dir.path(),
        "undeclared-helper",
        "must not become an implicit fixture\n",
    );
    let changed = loom_gate::inventory::snapshot::Snapshot::discover(dir.path()).unwrap();
    assert!(matches!(
        loom_gate::inventory::Inventory::discover(&changed),
        Err(loom_gate::inventory::Error::Unclassified { path: unknown }) if unknown == path("undeclared-helper")
    ));
}

#[test]
fn declared_runtime_resource_without_actual_execution_input_is_rejected_during_discovery() {
    let dir = fixture();
    native_project(&dir, "", false, true);
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(
        dir.path(),
        "flake.nix",
        &flake.replace("README.md", "opaque-helper"),
    );
    write(dir.path(), "opaque-helper", "# Omitted input\n");
    let snapshot = loom_gate::inventory::snapshot::Snapshot::discover(dir.path()).unwrap();
    assert!(matches!(
        loom_gate::inventory::Inventory::discover(&snapshot),
        Err(loom_gate::inventory::Error::NativeResource { .. })
    ));
}

#[test]
fn a_cargo_responsibility_label_cannot_replace_the_actual_fixed_execution() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(dir.path(), "flake.nix", &flake.replace(
        "    checks.x86_64-linux.heading = checked.check",
        "    checks.x86_64-linux.heading = self.checks.x86_64-linux.actual // { loom = self.checks.x86_64-linux.actual.loom // { cargo = \"build\"; }; };\n    checks.x86_64-linux.actual = checked.check",
    ));
    assert!(matches!(failure(&dir), NixError::CargoExecution));
}

#[test]
fn cargo_phase_text_cannot_authorize_a_different_actual_builder() {
    let dir = fixture();
    let phase = concat!(
        "runHook preBuild\ncargo --version\n",
        "export HOME=$(mktemp -d)\nexport GIT_CONFIG_SYSTEM=/dev/null\n",
        "cargo build --offline --locked --workspace --all-targets --profile \"$CARGO_PROFILE\"\n",
        "\nrunHook postBuild\n"
    );
    native_project(
        &dir,
        &format!("buildPhase = {};", serde_json::to_string(phase).unwrap()),
        true,
        true,
    );
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(dir.path(), "flake.nix", &flake.replace(
        "    checks.x86_64-linux.heading = checked.check",
        "    checks.x86_64-linux.heading = self.checks.x86_64-linux.actual // { loom = self.checks.x86_64-linux.actual.loom // { cargo = \"build\"; }; };\n    checks.x86_64-linux.actual = checked.check",
    ));
    assert!(matches!(failure(&dir), NixError::CargoExecution));
}

#[test]
fn native_baseline_does_not_cover_unconsumed_nix_sources() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    write(
        dir.path(),
        "unconsumed.nix",
        "this is deliberately not valid Nix @",
    );
    let (snapshot, inventory) = context(&dir);
    let definition = Definition::hermetic_baseline(
        &snapshot,
        &inventory,
        name("heading"),
        &name("nix:check:flake.nix:x86_64-linux:heading"),
    )
    .unwrap();
    assert!(
        definition
            .description()
            .responsibilities()
            .iter()
            .all(|responsibility| {
                responsibility.subject() != &name("nix:unconsumed.nix")
                    && responsibility.subject() != &name("resource:README.md")
            })
    );
}

#[test]
fn native_baseline_does_not_claim_an_independent_native_output() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(dir.path(), "flake.nix", &flake.replace(
        "    checks.x86_64-linux.heading =",
        "    checks.x86_64-linux.other = self.checks.x86_64-linux.heading;\n    checks.x86_64-linux.heading =",
    ));
    let (snapshot, inventory) = context(&dir);
    let other = name("nix:check:flake.nix:x86_64-linux:other");
    assert!(inventory.subject(&other).is_ok());
    let definition = Definition::hermetic_baseline(
        &snapshot,
        &inventory,
        name("heading"),
        &name("nix:check:flake.nix:x86_64-linux:heading"),
    )
    .unwrap();
    assert!(
        definition
            .description()
            .responsibilities()
            .iter()
            .all(|responsibility| {
                responsibility.subject() != &other && responsibility.responsible() != &other
            })
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

fn root_error(error: NixError) -> NixError {
    match error {
        NixError::Binding { source, .. } => root_error(*source),
        error => error,
    }
}

#[expect(
    clippy::panic,
    reason = "Unexpected rejection boundaries are test failures, never successful fallback observations."
)]
fn failure(dir: &TempDir) -> NixError {
    let snapshot = loom_gate::inventory::snapshot::Snapshot::discover(dir.path()).unwrap();
    match loom_gate::inventory::Inventory::discover(&snapshot) {
        Err(
            loom_gate::inventory::Error::NativeResource { source, .. }
            | loom_gate::inventory::Error::NativeResources { source, .. },
        ) => return root_error(*source),
        Err(error) => panic!("unexpected discovery rejection: {error:?}"),
        Ok(_) => {}
    }
    match definition(dir) {
        Err(Error::Construction {
            target,
            producer,
            source,
        }) => {
            assert_eq!(target, name("heading"));
            assert_eq!(producer, path("flake.nix"));
            match *source {
                Error::Nix(error) => root_error(error),
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
    assert!(
        matches!(failure(&dir), NixError::Query { detail } if detail.contains("invalid native execution"))
    );
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

fn graph_identity(dir: &TempDir) -> (loom_gate::provider::Digest, BTreeSet<PathBuf>) {
    let (snapshot, inventory, definition) = definition(dir).unwrap();
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
    let graph = unit.nix_graph().unwrap();
    (
        graph.identity().clone(),
        graph
            .origins()
            .iter()
            .map(|path| path.as_path().to_path_buf())
            .collect(),
    )
}

#[test]
fn native_runtime_partition_excludes_unrelated_documents_and_preserves_graph_identity() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    let (before, origins) = graph_identity(&dir);
    assert_eq!(origins, BTreeSet::from([PathBuf::from("README.md")]));
    write(dir.path(), "unrelated.md", "# Unrelated runtime fixture\n");
    let (after, _) = graph_identity(&dir);
    assert_eq!(before, after);
    write(dir.path(), "README.md", "# Changed runtime fixture\n");
    assert_ne!(before, graph_identity(&dir).0);
}

#[test]
fn hidden_evaluator_reads_fail_in_the_actual_staged_partition() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    write(dir.path(), "hidden.md", "# Hidden evaluator input\n");
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(
        dir.path(),
        "flake.nix",
        &flake.replace(
            "name = \"loom-provider-heading\";",
            "name = \"loom-provider-heading\"; hidden = builtins.readFile ./hidden.md;",
        ),
    );
    assert!(matches!(failure(&dir), NixError::Query { detail } if detail.contains("hidden.md")));
}

#[test]
fn undeclared_existence_reads_cannot_silently_change_the_constructed_graph() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    write(dir.path(), "hidden.md", "# Hidden evaluator input\n");
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(
        dir.path(),
        "flake.nix",
        &flake.replace(
            "name = \"loom-provider-heading\";",
            "name = \"loom-provider-heading\"; hidden = builtins.pathExists ./hidden.md;",
        ),
    );
    assert!(matches!(failure(&dir), NixError::Partition));
}

#[test]
fn undeclared_existence_reads_cannot_change_only_the_native_runtime() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    write(dir.path(), "hidden.md", "# Hidden runtime selector\n");
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(dir.path(), "flake.nix", &flake
        .replace("heading = checked.check {", "heading = let actual = checked.check {")
        .replace("    };\n  };", "    }; in actual // { loom = actual.loom // { runtime = if builtins.pathExists ./hidden.md then \"nix\" else \"native-cgroup\"; }; };\n  };"));
    assert!(matches!(failure(&dir), NixError::Partition));
}

#[test]
fn resource_descriptions_cannot_claim_bytes_from_a_different_origin() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    write(dir.path(), "other.md", "# Different document\n");
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(
        dir.path(),
        "flake.nix",
        &flake.replace(
            "resources = [ source ];",
            "resources = [ (source // { origin = \"other.md\"; }) ];",
        ),
    );
    assert!(matches!(failure(&dir), NixError::Origin { .. }));
}

#[test]
fn a_declared_resource_cannot_offer_a_subject_absent_from_its_native_tree() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    write(dir.path(), "other.md", "# Different document\n");
    let (snapshot, inventory) = context(&dir);
    assert!(matches!(
        Definition::hermetic(
            &snapshot,
            &inventory,
            name("heading"),
            &name("nix:check:flake.nix:x86_64-linux:heading"),
            &BTreeSet::from([name("resource:other.md")]),
            &BTreeSet::from([Duty::Document])
        ),
        Err(Error::Nix(NixError::Omitted { .. }))
    ));
}

#[test]
fn whole_project_capture_is_not_a_partitioned_resource_contract() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(
        dir.path(),
        "flake.nix",
        &flake.replace(
            "src = builtins.head sources;",
            "src = self; declared = builtins.head sources;",
        ),
    );
    assert!(matches!(failure(&dir), NixError::WholeSource));
}

#[test]
fn packages_bind_their_actual_native_derivation_instead_of_a_check_alias() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(
        dir.path(),
        "flake.nix",
        &flake.replace(
            "checks.x86_64-linux.heading",
            "packages.x86_64-linux.heading",
        ),
    );
    let (snapshot, inventory) = context(&dir);
    let native = name("nix:package:flake.nix:x86_64-linux:heading");
    assert_eq!(
        inventory
            .subject(&native)
            .unwrap()
            .nix_target()
            .unwrap()
            .kind(),
        loom_gate::inventory::nix::Kind::Package
    );
    Definition::hermetic(
        &snapshot,
        &inventory,
        name("heading"),
        &native,
        &BTreeSet::from([name("resource:README.md")]),
        &BTreeSet::from([Duty::Document]),
    )
    .unwrap();
}

#[test]
fn app_programs_are_discovered_but_never_become_an_arbitrary_hermetic_bridge() {
    let dir = fixture();
    write(
        dir.path(),
        "flake.nix",
        "{ outputs = { self }: { apps.x86_64-linux.heading = { type = \"app\"; program = \"/host/program\"; }; }; }\n",
    );
    let (snapshot, inventory) = context(&dir);
    assert!(matches!(
        Definition::hermetic(
            &snapshot,
            &inventory,
            name("heading"),
            &name("nix:app:flake.nix:x86_64-linux:heading"),
            &BTreeSet::from([name("resource:README.md")]),
            &BTreeSet::from([Duty::Document])
        ),
        Err(Error::Nix(NixError::App { .. }))
    ));
}

#[test]
fn native_output_mapping_covers_every_constructed_output() {
    let dir = fixture();
    native_project(&dir, "outputs = [ \"out\" \"report\" ];", true, true);
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
    let outputs = unit.nix_graph().unwrap().outputs().unwrap();
    assert_eq!(
        outputs.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([name("out"), name("report")])
    );
    assert_ne!(outputs[&name("out")], outputs[&name("report")]);
}

#[test]
fn explicit_evaluator_resources_correct_an_omitted_read_through_normal_construction() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    write(dir.path(), "hidden.md", "# Declared evaluator input\n");
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(dir.path(), "flake.nix", &flake.replace("resources = [ source ];", "resources = [ source ]; evaluation = [ (checked.resource { root = ./.; origin = \"hidden.md\"; }) ];").replace("name = \"loom-provider-heading\";", "name = \"loom-provider-heading\"; hidden = builtins.readFile ./hidden.md;"));
    let (snapshot, inventory, definition) = definition(&dir).unwrap();
    assert!(
        definition
            .description()
            .inputs()
            .iter()
            .any(|input| input.path() == &path("hidden.md"))
    );
    let candidate = definition.description().clone();
    let registry = Registry::new(&snapshot, vec![definition]).unwrap();
    registry
        .resolve(
            &snapshot,
            &inventory,
            &candidate,
            path("specs/example/tests.md"),
        )
        .unwrap();
}

#[test]
fn cold_pinned_network_inputs_require_provisioning_before_offline_dispatch() {
    let dir = fixture();
    native_project(
        &dir,
        r#"pinned = builtins.derivation {
        name = "loom-never-acquire-this-input-during-verification";
        system = "builtin";
        builder = "builtin:fetchurl";
        url = "https://invalid.example/undeclared-network";
        outputHashAlgo = "sha256";
        outputHashMode = "flat";
        outputHash = "0000000000000000000000000000000000000000000000000000000000000000";
    };"#,
        true,
        true,
    );
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
    assert!(matches!(
        unit.run(),
        Err(Error::Nix(NixError::ColdInput { .. }))
    ));
}

#[test]
fn structured_native_impurity_is_rejected_at_the_same_graph_boundary() {
    let dir = fixture();
    native_project(
        &dir,
        "__structuredAttrs = true; __noChroot = true;",
        true,
        true,
    );
    assert!(matches!(failure(&dir), NixError::Unsafe { .. }));
}

#[test]
fn canonical_native_store_names_retain_their_query_punctuation() {
    assert!(
        StorePath::try_from(
            "/nix/store/00000000000000000000000000000000-source.patch?id=123.drv".to_owned()
        )
        .is_ok()
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

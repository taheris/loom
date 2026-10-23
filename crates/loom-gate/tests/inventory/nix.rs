use loom_gate::inventory::snapshot::Snapshot;
use loom_gate::inventory::{Duty, Error, Inventory};

use super::{fixture, inventory, name, write};

#[test]
fn native_check_names_preserve_foreign_obligations_without_forcing_derivations() {
    let dir = fixture("", "pub fn example() {}\n");
    write(
        dir.path(),
        "flake.nix",
        r#"{
  outputs = { self }: {
    checks.x86_64-linux.native = throw "discovery evaluated native execution";
    checks.x86_64-darwin.foreign = throw "discovery evaluated foreign execution";
  };
}
"#,
    );
    let inventory = inventory(dir.path());
    for (system, attribute) in [("x86_64-linux", "native"), ("x86_64-darwin", "foreign")] {
        let subject = name(&format!("nix:check:flake.nix:{system}:{attribute}"));
        let binding = inventory.subject(&subject).unwrap().nix_target().unwrap();
        assert_eq!(binding.system(), &name(system));
        assert_eq!(binding.attribute(), &name(attribute));
        assert!(inventory.obligations().iter().any(|obligation| {
            obligation.subject() == &subject && obligation.duty() == Duty::Check
        }));
    }
}

#[test]
fn packages_and_apps_preserve_all_platform_obligations_without_forcing_values() {
    let dir = fixture("", "pub fn example() {}\n");
    write(
        dir.path(),
        "flake.nix",
        r#"{
      outputs = { self }: let
        entries = builtins.listToAttrs (map (system: {
          name = system;
          value.native = throw "discovery forced a native output";
        }) [ "aarch64-darwin" "aarch64-linux" "x86_64-darwin" "x86_64-linux" ]);
      in { packages = entries; apps = entries; };
    }
    "#,
    );
    let inventory = inventory(dir.path());
    for system in [
        "aarch64-darwin",
        "aarch64-linux",
        "x86_64-darwin",
        "x86_64-linux",
    ] {
        for (kind, expected) in [
            ("package", loom_gate::inventory::nix::Kind::Package),
            ("app", loom_gate::inventory::nix::Kind::App),
        ] {
            let subject = name(&format!("nix:{kind}:flake.nix:{system}:native"));
            assert_eq!(
                inventory
                    .subject(&subject)
                    .unwrap()
                    .nix_target()
                    .unwrap()
                    .kind(),
                expected
            );
            assert!(
                inventory
                    .obligations()
                    .iter()
                    .any(|obligation| obligation.subject() == &subject
                        && obligation.duty() == Duty::Evaluate)
            );
        }
    }
}

#[test]
fn malformed_package_namespace_is_not_treated_as_absent() {
    let dir = fixture("", "pub fn example() {}\n");
    write(
        dir.path(),
        "flake.nix",
        "{ outputs = { self }: { packages = null; }; }\n",
    );
    let snapshot = Snapshot::discover(dir.path()).unwrap();
    assert!(matches!(
        Inventory::discover(&snapshot),
        Err(Error::Discovery { .. })
    ));
}

#[test]
fn malformed_app_system_is_not_treated_as_an_empty_inventory() {
    let dir = fixture("", "pub fn example() {}\n");
    write(
        dir.path(),
        "flake.nix",
        "{ outputs = { self }: { apps.x86_64-linux = []; }; }\n",
    );
    let snapshot = Snapshot::discover(dir.path()).unwrap();
    assert!(matches!(
        Inventory::discover(&snapshot),
        Err(Error::Discovery { .. })
    ));
}

#[test]
fn flake_without_checks_keeps_its_evaluation_obligation() {
    let dir = fixture("", "pub fn example() {}\n");
    write(dir.path(), "flake.nix", "{ outputs = { self }: {}; }\n");
    let inventory = inventory(dir.path());
    assert!(inventory.obligations().iter().any(|obligation| {
        obligation.subject() == &name("nix:flake:flake.nix") && obligation.duty() == Duty::Evaluate
    }));
}

#[test]
fn malformed_native_check_namespace_is_not_an_empty_success() {
    let dir = fixture("", "pub fn example() {}\n");
    write(
        dir.path(),
        "flake.nix",
        "{ outputs = { self }: { checks.x86_64-linux = 7; }; }\n",
    );
    let snapshot = Snapshot::discover(dir.path()).unwrap();
    assert!(matches!(
        Inventory::discover(&snapshot),
        Err(Error::Discovery { detail, .. }) if detail.contains("set")
    ));
}

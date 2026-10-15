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

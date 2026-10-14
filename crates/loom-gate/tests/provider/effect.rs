use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;

use loom_gate::inventory::Duty;
use loom_gate::provider::effect::{Client, Endpoint};
use loom_gate::provider::tracked::Outcome;
use loom_gate::provider::{Candidate, Capability, Definition, Discovery, Error, Mapping, Registry};

use super::{context, contracts, fixture, name, path, write};

fn client(dir: &tempfile::TempDir, response: &str) -> Client {
    let binary = dir.path().join("tools/podman");
    write(
        dir.path(),
        "tools/podman",
        &format!(
            "#!/bin/sh\nset -eu\n[ \"$1\" = --remote ] && [ \"$2\" = --url ] && [ \"$4\" = info ] && [ \"$5\" = --format ] && [ \"$6\" = json ] || exit 91\n{response}\n"
        ),
    );
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    Client::capture(
        binary,
        Endpoint::try_from(dir.path().join("engine.sock")).unwrap(),
    )
    .unwrap()
}

fn definition(
    dir: &tempfile::TempDir,
    client: Client,
) -> (
    loom_gate::inventory::snapshot::Snapshot,
    loom_gate::inventory::Inventory,
    Definition,
) {
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Engine info [system](heading)\n",
    );
    let (snapshot, inventory) = context(dir);
    let definition = Definition::container_engine_info(
        &snapshot,
        &inventory,
        name("heading"),
        path("src/lib.rs"),
        &BTreeSet::from([name("resource:README.md")]),
        &BTreeSet::from([Duty::Document]),
        client,
    )
    .unwrap();
    (snapshot, inventory, definition)
}

#[test]
fn supported_effect_uses_generated_query_and_fixed_execution_without_ambient_authority() {
    let dir = fixture();
    let tool = client(
        &dir,
        "printf '%s\\n' '{\"host\":{\"os\":\"linux\",\"arch\":\"amd64\",\"kernel\":\"6.18\",\"security\":{\"rootless\":true}},\"store\":{\"graphRoot\":\"/var/lib/containers/storage\",\"graphDriverName\":\"overlay\"}}'",
    );
    // Tool fixtures live outside project discovery, as real pinned client binaries do.
    std::fs::write(
        dir.path().join(".gitignore"),
        "/target/\n/tools/\n/engine.sock\n",
    )
    .unwrap();
    let _socket = UnixListener::bind(dir.path().join("engine.sock")).unwrap();
    let (snapshot, inventory, definition) = definition(&dir, tool);
    let registry = Registry::new(&snapshot, vec![definition]).unwrap();
    let json = registry.query(&[name("heading")]).unwrap();
    let units = registry
        .resolve_query(&snapshot, &inventory, &json, &contracts(&["heading"]))
        .unwrap();
    let candidate = units[0].description();
    assert_eq!(candidate.discovery(), Discovery::SupportedEffect);
    assert_eq!(candidate.mapping(), Mapping::ExitStatus);
    assert_eq!(
        candidate.capabilities(),
        BTreeSet::from([Capability::ContainerEngine, Capability::HostKernel])
    );
    assert!(matches!(candidate, Candidate::SupportedEffect { .. }));
    for native_input in ["Cargo.toml", "Cargo.lock", "src/lib.rs"] {
        assert!(
            units[0]
                .inputs()
                .iter()
                .any(|input| input.path() == &path(native_input))
        );
    }
    assert_eq!(units[0].run().unwrap().outcome(), &Outcome::Pass);
}

#[test]
fn missing_engine_endpoint_is_unavailable_not_a_pass_or_invalid_registration() {
    let dir = fixture();
    let tool = client(&dir, "exit 92");
    std::fs::write(dir.path().join(".gitignore"), "/target/\n/tools/\n").unwrap();
    let (snapshot, inventory, definition) = definition(&dir, tool);
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
        Outcome::Unavailable {
            capability: Capability::ContainerEngine,
            ..
        }
    ));
}

#[test]
fn engine_failure_cannot_become_pass_via_a_supported_effect_wrapper() {
    let dir = fixture();
    let tool = client(&dir, "printf 'engine rejected the request' >&2; exit 42");
    std::fs::write(
        dir.path().join(".gitignore"),
        "/target/\n/tools/\n/engine.sock\n",
    )
    .unwrap();
    let _socket = UnixListener::bind(dir.path().join("engine.sock")).unwrap();
    let (snapshot, inventory, definition) = definition(&dir, tool);
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
    assert!(
        matches!(unit.run().unwrap().outcome(), Outcome::Fail { detail } if detail.contains("engine rejected"))
    );
}

#[test]
fn successful_process_with_invalid_engine_response_is_not_a_pass() {
    let dir = fixture();
    let tool = client(&dir, "printf '%s\\n' '{}'");
    std::fs::write(
        dir.path().join(".gitignore"),
        "/target/\n/tools/\n/engine.sock\n",
    )
    .unwrap();
    let _socket = UnixListener::bind(dir.path().join("engine.sock")).unwrap();
    let (snapshot, inventory, definition) = definition(&dir, tool);
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
        Err(Error::Effect(loom_gate::provider::effect::Error::Shape))
    ));
}

#[test]
fn effect_binary_replacement_invalidates_the_checked_definition() {
    let dir = fixture();
    let tool = client(&dir, "exit 0");
    std::fs::write(dir.path().join(".gitignore"), "/target/\n/tools/\n").unwrap();
    let (snapshot, inventory, definition) = definition(&dir, tool);
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
    std::fs::write(dir.path().join("tools/podman"), "#!/bin/sh\nexit 0\n").unwrap();
    assert!(matches!(
        unit.run(),
        Err(Error::Effect(loom_gate::provider::effect::Error::Changed))
    ));
}

#[test]
fn supported_effect_cannot_relabel_itself_as_deterministic_check_or_test() {
    for tier in ["check", "test"] {
        let dir = fixture();
        let tool = client(&dir, "exit 92");
        std::fs::write(dir.path().join(".gitignore"), "/target/\n/tools/\n").unwrap();
        write(
            dir.path(),
            "specs/example/tests.md",
            &format!("## Success Criteria\n- Engine observation [{tier}](heading)\n"),
        );
        let (snapshot, inventory) = context(&dir);
        let definition = Definition::container_engine_info(
            &snapshot,
            &inventory,
            name("heading"),
            path("src/lib.rs"),
            &BTreeSet::from([name("resource:README.md")]),
            &BTreeSet::from([Duty::Document]),
            tool,
        )
        .unwrap();
        let candidate = definition.description().clone();
        let registry = Registry::new(&snapshot, vec![definition]).unwrap();
        let Err(error) = registry.resolve(
            &snapshot,
            &inventory,
            &candidate,
            path("specs/example/tests.md"),
        ) else {
            panic!("effect admitted in deterministic lane");
        };
        assert!(matches!(error, Error::Lane { .. }));
        assert!(error.to_string().contains("system lane"));
        assert!(error.to_string().contains("loom gate verify"));
    }
}

#[test]
fn empty_engine_objects_cannot_satisfy_a_complete_capability_response() {
    let dir = fixture();
    let tool = client(&dir, "printf '%s\\n' '{\"host\":{},\"store\":{}}'");
    std::fs::write(
        dir.path().join(".gitignore"),
        "/target/\n/tools/\n/engine.sock\n",
    )
    .unwrap();
    let _socket = UnixListener::bind(dir.path().join("engine.sock")).unwrap();
    let (snapshot, inventory, definition) = definition(&dir, tool);
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
        Err(Error::Effect(loom_gate::provider::effect::Error::Shape))
    ));
}

#[test]
fn supported_effect_definition_requires_actual_native_rust_producer_ownership() {
    let dir = fixture();
    let tool = client(&dir, "exit 92");
    std::fs::write(dir.path().join(".gitignore"), "/target/\n/tools/\n").unwrap();
    let (snapshot, inventory) = context(&dir);
    assert!(matches!(
        Definition::container_engine_info(
            &snapshot,
            &inventory,
            name("heading"),
            path("README.md"),
            &BTreeSet::from([name("resource:README.md")]),
            &BTreeSet::from([Duty::Document]),
            tool
        ),
        Err(Error::Checker { .. })
    ));
}

#[test]
fn supported_effect_has_no_arbitrary_command_or_endpoint_escape() {
    for value in [
        "relative.sock",
        "/run/../tmp/engine.sock",
        "/run/./engine.sock",
    ] {
        assert!(Endpoint::try_from(PathBuf::from(value)).is_err());
    }
    let endpoint = Endpoint::try_from(PathBuf::from("/run/podman/podman.sock")).unwrap();
    assert!(Client::capture(PathBuf::from("/bin/echo"), endpoint).is_err());
}

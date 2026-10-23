//! Admission and lane controls; actual report execution runs in the isolated native matrix.

use std::path::Path;

#[path = "common/reported.rs"]
mod reported;

#[test]
fn check_scope_does_not_execute_constructor_owned_system_reports() {
    let fixture = reported::Fixture::new("system", "json-lines", &["a", "b"]);
    fixture.policy("system", true);
    fixture.record("a", "intentionally malformed if executed\n", 1);
    fixture.record("b", "", 1);
    let output = fixture.run(Path::new(env!("CARGO_BIN_EXE_loom")), "check", true);
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{diagnostic}");
    assert!(
        !diagnostic.contains("\"kind\":\"fresh-execution\""),
        "{diagnostic}"
    );
    assert!(
        !diagnostic.contains(&format!("loom gate [{}]:", fixture.address("a"))),
        "{diagnostic}"
    );
    assert_eq!(
        loom_gate::cache::StatusCache::open(&fixture.path().join(".loom/state.db"))
            .unwrap()
            .read_all()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn native_package_report_aliases_bind_actual_current_system_definitions() {
    let fixture = reported::Fixture::new("system", "json-lines", &["a", "b"]);
    fixture.record("a", "would fail if executed", 1);
    fixture.record("b", "would fail if executed", 1);
    let flake = fixture.path().join("flake.nix");
    let body = std::fs::read_to_string(&flake)
        .unwrap()
        .replace("in { checks.", "in { packages.");
    std::fs::write(flake, body).unwrap();
    let contract = fixture.path().join("specs/fixture/tests.md");
    let body = std::fs::read_to_string(&contract)
        .unwrap()
        .replace(&fixture.address("a"), "nix report .#probe::a")
        .replace(&fixture.address("b"), "nix report .#probe::b");
    std::fs::write(contract, body).unwrap();
    let output = fixture.run(Path::new(env!("CARGO_BIN_EXE_loom")), "check", true);
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{diagnostic}");
    assert!(!diagnostic.contains("\"kind\":\"fresh-execution\""));
}

#[test]
fn native_package_root_aliases_retain_constructor_owned_system_lane() {
    let fixture = reported::Fixture::new("system", "json-lines", &["a", "b"]);
    fixture.record("a", "would fail if executed", 1);
    fixture.record("b", "would fail if executed", 1);
    let flake = fixture.path().join("flake.nix");
    let body = std::fs::read_to_string(&flake)
        .unwrap()
        .replace("in { checks.", "in { packages.");
    std::fs::write(flake, body).unwrap();
    std::fs::write(
        fixture.path().join("specs/fixture/tests.md"),
        "## Success Criteria\n- Native package [system](nix build .#probe)\n",
    )
    .unwrap();
    let output = fixture.run(Path::new(env!("CARGO_BIN_EXE_loom")), "check", true);
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{diagnostic}");
    assert!(!diagnostic.contains("\"kind\":\"fresh-execution\""));
}

#[test]
fn native_skip_policy_is_validated_before_any_execution() {
    let fixture = reported::Fixture::new("system", "json-lines", &["a", "b"]);
    fixture.record("a", "", 1);
    fixture.record("b", "", 1);
    std::fs::write(fixture.path().join("loom.toml"), "[runner.system.native]\nskip_policy='sandbox-capability'\nskip_capabilities=['invalid capability token']\n").unwrap();
    let output = fixture.run(Path::new(env!("CARGO_BIN_EXE_loom")), "verify", true);
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        diagnostic.contains("invalid platform/capability token"),
        "{diagnostic}"
    );
    assert!(
        !diagnostic.contains("\"kind\":\"fresh-execution\""),
        "{diagnostic}"
    );
}

#[test]
fn opaque_reports_cannot_manufacture_rust_test_identities() {
    let fixture = reported::Fixture::new("system", "json-lines", &["a", "b"]);
    fixture.record("a", "", 0);
    fixture.record("b", "", 0);
    let contract = fixture.path().join("specs/fixture/tests.md");
    let body = std::fs::read_to_string(&contract)
        .unwrap()
        .replace("[system]", "[test]");
    std::fs::write(contract, body).unwrap();
    let output = fixture.run(Path::new(env!("CARGO_BIN_EXE_loom")), "verify", true);
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("\"kind\":\"fresh-execution\""));
}

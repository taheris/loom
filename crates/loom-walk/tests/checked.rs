//! Actual manifest registrations use native inventory and captured analysis reads.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use loom_gate::inventory::snapshot::{ResourcePath, Snapshot};
use loom_gate::inventory::{Inventory, Name};
use loom_gate::provider::tracked::{Observation, Outcome};
use loom_gate::provider::{Error, Registry};
use loom_walk::walk::checked_definitions;
use tempfile::TempDir;

const RULES: &[(&str, &str, &str)] = &[
    (
        "loom_agent_deps",
        "loom-agent",
        include_str!("../src/walk/loom_agent_deps.rs"),
    ),
    (
        "loom_llm_deps",
        "loom-llm",
        include_str!("../src/walk/loom_llm_deps.rs"),
    ),
    (
        "loom_render_deps",
        "loom-render",
        include_str!("../src/walk/loom_render_deps.rs"),
    ),
    (
        "loom_skill_deps",
        "loom-skill",
        include_str!("../src/walk/loom_skill_deps.rs"),
    ),
    (
        "loom_templates_deps",
        "loom-templates",
        include_str!("../src/walk/loom_templates_deps.rs"),
    ),
    (
        "loom_tune_deps",
        "loom-tune",
        include_str!("../src/walk/loom_tune_deps.rs"),
    ),
    (
        "loom_events_is_leaf",
        "loom-events",
        include_str!("../src/walk/loom_events_is_leaf.rs"),
    ),
];

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

fn native(root: &Path, program: &str, args: &[&str]) {
    let output = Command::new(program)
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    native(root, "git", &["init"]);
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = ['crates/*']\nresolver = '3'\n",
    );
    for (_, package, _) in RULES {
        let dependencies: &[&str] = match *package {
            "loom-agent" => &["loom-llm", "loom-events", "loom-skill"],
            "loom-render" | "loom-skill" => &["loom-events"],
            "loom-tune" => &["loom-events", "loom-skill"],
            _ => &[],
        };
        let mut manifest = format!(
            "[package]\nname = '{package}'\nversion = '0.1.0'\nedition = '2024'\n\n[dependencies]\n"
        );
        for dependency in dependencies {
            writeln!(manifest, "{dependency} = {{ path = '../{dependency}' }}").unwrap();
        }
        write(root, &format!("crates/{package}/Cargo.toml"), &manifest);
        write(
            root,
            &format!("crates/{package}/src/lib.rs"),
            "pub fn fixture() {}\n",
        );
    }
    write(
        root,
        "crates/loom-walk/Cargo.toml",
        "[package]\nname = 'loom-walk'\nversion = '0.1.0'\nedition = '2024'\n",
    );
    let mut modules = String::new();
    for (target, _, source) in RULES {
        writeln!(modules, "#[path = \"walk/{target}.rs\"] mod {target};").unwrap();
        write(
            root,
            &format!("crates/loom-walk/src/walk/{target}.rs"),
            source,
        );
    }
    write(root, "crates/loom-walk/src/lib.rs", &modules);
    write(
        root,
        "docs/README.md",
        "- [walks](../specs/walks/spec.md)\n",
    );
    write(root, "specs/walks/spec.md", "# Manifest analysis\n");
    let mut criteria = String::new();
    for (target, _, _) in RULES {
        writeln!(criteria, "- Dependency rule [check]({target})").unwrap();
    }
    write(
        root,
        "specs/walks/tests.md",
        &format!("# Walks\n\n## Success Criteria\n\n{criteria}"),
    );
    native(root, "cargo", &["generate-lockfile", "--offline"]);
    dir
}

fn context(root: &Path) -> (Snapshot, Inventory) {
    let snapshot = Snapshot::discover(root).unwrap();
    let inventory = Inventory::discover(&snapshot).unwrap();
    (snapshot, inventory)
}

fn registry(snapshot: &Snapshot, inventory: &Inventory, targets: &[Name]) -> Registry {
    Registry::new(
        snapshot,
        checked_definitions(snapshot, inventory, targets).unwrap(),
    )
    .unwrap()
}

fn contracts(targets: &[Name]) -> BTreeMap<Name, ResourcePath> {
    targets
        .iter()
        .map(|target| (target.clone(), path("specs/walks/tests.md")))
        .collect()
}

#[test]
fn every_manifest_registration_executes_the_real_rule_with_tracked_content() {
    let dir = fixture();
    let (snapshot, inventory) = context(dir.path());
    let targets: Vec<_> = RULES.iter().map(|(target, _, _)| name(target)).collect();
    let registry = registry(&snapshot, &inventory, &targets);
    let query = registry.query(&targets).unwrap();
    let units = registry
        .resolve_query(&snapshot, &inventory, &query, &contracts(&targets))
        .unwrap();
    assert_eq!(units.len(), RULES.len());
    for (unit, (_, package, _)) in units.iter().zip(RULES) {
        let result = unit.run().unwrap();
        assert_eq!(result.outcome(), &Outcome::Pass, "{package}");
        assert!(
            matches!(result.observations(), [Observation::Content { path: input, .. }] if input == &path(&format!("crates/{package}/Cargo.toml")))
        );
    }
}

#[test]
fn bad_manifest_query_is_nonexecuting_and_correction_reenters_normal_admission() {
    let dir = fixture();
    let root = dir.path();
    let manifest = "crates/loom-render/Cargo.toml";
    let good = std::fs::read_to_string(root.join(manifest)).unwrap();
    write(
        root,
        manifest,
        &good.replace("loom-events = { path = '../loom-events' }", ""),
    );
    native(root, "cargo", &["generate-lockfile", "--offline"]);
    let targets = [name("loom_render_deps")];
    let (snapshot, inventory) = context(root);
    let registry = registry(&snapshot, &inventory, &targets);
    let query = registry.query(&targets).unwrap();
    assert!(!query.contains("verdict"));
    let units = registry
        .resolve_query(&snapshot, &inventory, &query, &contracts(&targets))
        .unwrap();
    assert!(
        matches!(units[0].run().unwrap().outcome(), Outcome::Fail { detail } if detail.contains("missing required dep"))
    );
    write(root, manifest, &good);
    native(root, "cargo", &["generate-lockfile", "--offline"]);
    assert!(registry.query(&targets).is_err());
    let (snapshot, inventory) = context(root);
    let fresh = self::registry(&snapshot, &inventory, &targets);
    let query = fresh.query(&targets).unwrap();
    let units = fresh
        .resolve_query(&snapshot, &inventory, &query, &contracts(&targets))
        .unwrap();
    assert_eq!(units[0].run().unwrap().outcome(), &Outcome::Pass);
}

#[test]
fn legacy_registration_cannot_be_promoted_from_input_metadata() {
    let dir = fixture();
    let (snapshot, inventory) = context(dir.path());
    let target = name("no_event_sentinels");
    assert!(
        matches!(checked_definitions(&snapshot, &inventory, std::slice::from_ref(&target)), Err(Error::Missing { target: missing }) if missing == target)
    );
}

#[test]
fn all_checked_names_are_resolved_before_any_definition_is_constructed() {
    let dir = fixture();
    let (snapshot, inventory) = context(dir.path());
    write(dir.path(), "new.md", "# Changed snapshot\n");
    let target = name("unknown_walk");
    assert!(
        matches!(checked_definitions(&snapshot, &inventory, &[name("loom_llm_deps"), target.clone()]), Err(Error::Missing { target: missing }) if missing == target)
    );
}

#[test]
fn duplicate_checked_requests_are_rejected_without_lossy_deduplication() {
    let dir = fixture();
    let (snapshot, inventory) = context(dir.path());
    let target = name("loom_llm_deps");
    assert!(
        matches!(checked_definitions(&snapshot, &inventory, &[target.clone(), target.clone()]), Err(Error::Duplicate { target: repeated }) if repeated == target)
    );
}

#[test]
fn checker_source_mutation_invalidates_a_previously_generated_query() {
    let dir = fixture();
    let (snapshot, inventory) = context(dir.path());
    let targets = [name("loom_llm_deps")];
    let registry = registry(&snapshot, &inventory, &targets);
    registry.query(&targets).unwrap();
    write(
        dir.path(),
        "crates/loom-walk/src/walk/loom_llm_deps.rs",
        "pub fn altered() {}\n",
    );
    assert!(registry.query(&targets).is_err());
}

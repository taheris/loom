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
        "crates/loom-events/src/lib.rs",
        "pub mod identifier;\npub fn fixture() {}\n",
    );
    write(
        root,
        "crates/loom-events/src/identifier/mod.rs",
        "pub struct FixtureId(String);\n",
    );
    write(
        root,
        "crates/loom-walk/Cargo.toml",
        "[package]\nname = 'loom-walk'\nversion = '0.1.0'\nedition = '2024'\n",
    );
    // Native module discovery follows the real checker modules, not a synthetic callback.
    let sources = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/walk");
    for entry in std::fs::read_dir(sources).unwrap() {
        let entry = entry.unwrap();
        let body = std::fs::read_to_string(entry.path()).unwrap();
        let production = body.split("\n#[cfg(test)]\n").next().unwrap();
        write(
            root,
            &format!(
                "crates/loom-walk/src/walk/{}",
                entry.file_name().to_string_lossy()
            ),
            production,
        );
    }
    write(
        root,
        "crates/loom-walk/src/selection.rs",
        include_str!("../src/selection.rs"),
    );
    write(
        root,
        "crates/loom-walk/src/lib.rs",
        "pub mod walk;\npub mod selection;\n",
    );
    write(
        root,
        "docs/README.md",
        "- [walks](../specs/walks/spec.md)\n",
    );
    write(root, "specs/walks/spec.md", "# Manifest analysis\n");
    let mut criteria = String::new();
    for walk in loom_walk::walk::REGISTRY {
        writeln!(criteria, "- Source rule [check]({})", walk.name).unwrap();
    }
    write(
        root,
        "specs/walks/tests.md",
        &format!("# Walks\n\n## Success Criteria\n\n{criteria}"),
    );
    native(root, "cargo", &["generate-lockfile", "--offline"]);
    dir
}

/// Complete captured namespace derived from the actual definitions, not a copied input table.
fn complete_fixture() -> TempDir {
    let dir = fixture();
    let root = dir.path();
    let project = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap();
    let acceptance = std::fs::read_to_string(root.join("specs/walks/tests.md")).unwrap();
    std::fs::remove_dir_all(root.join("specs/walks")).unwrap();
    write(root, "specs/walks.md", &acceptance);
    let mut paths = std::collections::BTreeSet::new();
    for walk in loom_walk::walk::REGISTRY {
        paths.extend((walk.inputs)(
            project,
            &loom_walk::walk::WalkInput::for_root(project),
        ));
    }
    for file in paths {
        if !file.is_file() {
            continue;
        } // Declared absences stay absent.
        let relative = file.strip_prefix(project).unwrap();
        if relative
            .file_name()
            .is_some_and(|name| name == "Cargo.toml")
        {
            continue;
        }
        let dest = root.join(relative);
        if dest.is_file() {
            continue;
        }
        let body = match relative.extension().and_then(|ext| ext.to_str()) {
            Some("rs") => "// Native fixture source.\n",
            Some("md") => "# Fixture document\n",
            Some("nix") if relative == Path::new("flake.nix") => "{ outputs = _: {}; }\n",
            Some("nix") => "{}\n",
            _ => "# Fixture resource\n",
        };
        write(root, &relative.to_string_lossy(), body);
    }
    // Each actual package definition remains native Cargo ownership, not a resource-role override.
    for entry in std::fs::read_dir(project.join("crates")).unwrap() {
        let package = entry.unwrap().file_name().to_string_lossy().into_owned();
        let manifest = root.join(format!("crates/{package}/Cargo.toml"));
        if !manifest.is_file() {
            write(
                root,
                &format!("crates/{package}/Cargo.toml"),
                &format!("[package]\nname='{package}'\nversion='0.1.0'\nedition='2024'\n"),
            );
        }
        let src = root.join(format!("crates/{package}/src"));
        let entrypoint = if package == "loom" {
            "main.rs"
        } else {
            "lib.rs"
        };
        let mut body = std::fs::read_to_string(src.join(entrypoint)).unwrap_or_default();
        if package == "loom" {
            body.push_str("\nfn main() {}\n");
        }
        if package != "loom-walk" {
            // Bind every synthetic source through actual native module declarations.
            let mut files: Vec<_> = walkdir::WalkDir::new(&src)
                .into_iter()
                .map(Result::unwrap)
                .filter(|entry| {
                    entry.file_type().is_file()
                        && entry.path().extension().is_some_and(|ext| ext == "rs")
                        && entry.path() != src.join(entrypoint)
                })
                .map(walkdir::DirEntry::into_path)
                .collect();
            files.sort();
            for (index, file) in files.iter().enumerate() {
                writeln!(
                    body,
                    "#[path = {:?}] mod fixture_{index};",
                    file.strip_prefix(&src).unwrap().to_string_lossy()
                )
                .unwrap();
            }
        }
        write(root, &format!("crates/{package}/src/{entrypoint}"), &body);
    }
    let mut index = String::new();
    for file in std::fs::read_dir(root.join("specs")).unwrap() {
        let file = file.unwrap().path();
        if file.extension().is_some_and(|ext| ext == "md") {
            let label = file.file_stem().unwrap().to_string_lossy();
            writeln!(index, "- [{label}](../specs/{label}.md)").unwrap();
        }
    }
    write(root, "docs/README.md", &index);
    // Non-source files here are real test fixtures, consumed by this fixture's testable target.
    let mut manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    manifest.push_str("\n[package]\nname='rootfixture'\nversion='0.1.0'\nedition='2024'\n[lib]\npath='fixture-root.rs'\n");
    let mut resources: Vec<_> = walkdir::WalkDir::new(root)
        .into_iter()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().is_file() && !entry.path().starts_with(root.join(".git")))
        .filter(|entry| {
            !matches!(
                entry.path().extension().and_then(|ext| ext.to_str()),
                Some("rs" | "nix" | "md" | "toml" | "lock")
            )
        })
        .map(walkdir::DirEntry::into_path)
        .collect();
    resources.sort();
    for (index, file) in resources.iter().enumerate() {
        writeln!(manifest, "\n[[package.metadata.loom.verify.resource]]\nname='fixture-resource-{index}'\nrole='test-fixture'\ndirectory={:?}\nconsumers=['cargo:rootfixture:lib:rootfixture']", file.strip_prefix(root).unwrap().to_string_lossy()).unwrap();
    }
    write(root, "Cargo.toml", &manifest);
    let mut fixture_source = String::from("pub fn fixture() {}\n");
    let mut fixture_modules: Vec<_> = walkdir::WalkDir::new(root)
        .into_iter()
        .map(Result::unwrap)
        .filter(|entry| {
            entry.file_type().is_file()
                && entry.path().extension().is_some_and(|ext| ext == "rs")
                && !entry
                    .path()
                    .components()
                    .any(|part| part.as_os_str() == "src")
                && !entry.path().starts_with(root.join(".git"))
        })
        .map(walkdir::DirEntry::into_path)
        .collect();
    fixture_modules.sort();
    for (index, file) in fixture_modules.iter().enumerate() {
        writeln!(
            fixture_source,
            "#[path = {:?}] mod fixture_{index};",
            file.strip_prefix(root).unwrap().to_string_lossy()
        )
        .unwrap();
    }
    write(root, "fixture-root.rs", &fixture_source);
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
    assert!(!query.contains("\"verdict\":"));
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
fn actual_manifest_and_checker_inputs_cannot_be_removed_from_query_descriptions() {
    let dir = fixture();
    let (snapshot, inventory) = context(dir.path());
    let targets = [name("loom_llm_deps")];
    let registry = registry(&snapshot, &inventory, &targets);
    let query = registry.query(&targets).unwrap();
    for omitted in [
        "crates/loom-llm/Cargo.toml",
        "crates/loom-walk/src/walk/loom_llm_deps.rs",
    ] {
        let mut description: serde_json::Value = serde_json::from_str(&query).unwrap();
        let inputs = description["units"][0]["contract"]["inputs"]
            .as_array_mut()
            .unwrap();
        let original = inputs.len();
        let omitted = serde_json::to_value(path(omitted)).unwrap();
        inputs.retain(|input| input["path"] != omitted);
        assert_eq!(inputs.len(), original - 1);
        let malformed = serde_json::to_string(&description).unwrap();
        assert!(matches!(
            registry.resolve_query(&snapshot, &inventory, &malformed, &contracts(&targets)),
            Err(Error::Admission { source, .. })
                if matches!(*source, Error::Inconsistent { .. })
        ));
    }
}

#[test]
fn every_dispatch_entry_has_an_actual_named_tracked_definition() {
    let dir = complete_fixture();
    let (snapshot, inventory) = context(dir.path());
    let targets: Vec<_> = loom_walk::walk::REGISTRY
        .iter()
        .map(|walk| name(walk.name))
        .collect();
    assert_eq!(targets.len(), 64);
    let definitions = checked_definitions(&snapshot, &inventory, &targets).unwrap();
    let registry = Registry::new(&snapshot, definitions).unwrap();
    let query = registry.query(&targets).unwrap();
    assert!(!query.contains("\"verdict\":"));
    let contracts: BTreeMap<_, _> = targets
        .iter()
        .map(|target| (target.clone(), path("specs/walks.md")))
        .collect();
    let units = registry
        .resolve_query(&snapshot, &inventory, &query, &contracts)
        .unwrap();
    assert_eq!(units.len(), targets.len());
    for unit in units {
        let walk = loom_walk::walk::lookup(unit.target().as_str()).unwrap();
        let standalone = (walk.run)(&loom_walk::walk::WalkInput::for_root(dir.path()));
        match unit.run() {
            Ok(result) => match result.outcome() {
                Outcome::Pass => assert!(standalone.pass, "{}: {}", walk.name, standalone.evidence),
                Outcome::Fail { detail } => {
                    assert!(!standalone.pass, "{}: {detail}", walk.name);
                    assert_eq!(
                        detail.split("\nsource errors:\n").next(),
                        standalone.evidence.split("\nsource errors:\n").next(),
                        "{}",
                        walk.name
                    );
                }
                other => panic!("{}: unexpected {other:?}", walk.name),
            },
            Err(error) => {
                // A missing fixture input is a failure in both paths, never a pass.
                assert!(
                    !standalone.pass,
                    "{}: checked failed but standalone passed: {error}",
                    walk.name
                );
                assert!(
                    matches!(error, Error::Snapshot(_)),
                    "{}: {error:?}",
                    walk.name
                );
            }
        }
    }
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

fn run_target(root: &Path, target: &str) -> Outcome {
    let (snapshot, inventory) = context(root);
    let targets = [name(target)];
    let registry = registry(&snapshot, &inventory, &targets);
    let query = registry.query(&targets).unwrap();
    registry
        .resolve_query(&snapshot, &inventory, &query, &contracts(&targets))
        .unwrap()[0]
        .run()
        .unwrap()
        .outcome()
        .clone()
}

#[test]
fn real_scan_additions_renames_and_deletions_reenter_fresh_admission() {
    let dir = fixture();
    let root = dir.path();
    let target = "no_event_sentinels";
    assert_eq!(run_target(root, target), Outcome::Pass);
    let (snapshot, inventory) = context(root);
    let targets = [name(target)];
    let old_registry = registry(&snapshot, &inventory, &targets);
    let old_query = old_registry.query(&targets).unwrap();
    let original = std::fs::read_to_string(root.join("crates/loom-events/src/lib.rs")).unwrap();
    write(
        root,
        "crates/loom-events/src/lib.rs",
        &format!("{original}\nmod added;\n"),
    );
    write(
        root,
        "crates/loom-events/src/added.rs",
        "fn f() { let _ = EventEnvelope::default(); }\n",
    );
    assert!(old_registry.query(&targets).is_err());
    assert!(
        old_registry
            .resolve_query(&snapshot, &inventory, &old_query, &contracts(&targets))
            .is_err()
    );
    assert!(
        matches!(run_target(root, target), Outcome::Fail { detail } if detail.contains("sentinel constructor"))
    );
    std::fs::rename(
        root.join("crates/loom-events/src/added.rs"),
        root.join("crates/loom-events/src/renamed.rs"),
    )
    .unwrap();
    write(
        root,
        "crates/loom-events/src/lib.rs",
        &format!("{original}\nmod renamed;\n"),
    );
    assert!(matches!(run_target(root, target), Outcome::Fail { .. }));
    let (snapshot, inventory) = context(root);
    let registry = registry(&snapshot, &inventory, &targets);
    let query = registry.query(&targets).unwrap();
    assert!(query.contains("crates/loom-events/src/renamed.rs"));
    assert!(!query.contains("crates/loom-events/src/added.rs"));
    std::fs::remove_file(root.join("crates/loom-events/src/renamed.rs")).unwrap();
    write(root, "crates/loom-events/src/lib.rs", &original);
    assert!(registry.query(&targets).is_err());
    assert_eq!(run_target(root, target), Outcome::Pass);
}

#[test]
fn batch_attribution_rejects_omitted_file_membership_configuration_and_checker_inputs() {
    let dir = fixture();
    let (snapshot, inventory) = context(dir.path());
    let targets = [name("no_event_sentinels"), name("workspace_deps_pinned")];
    let registry = registry(&snapshot, &inventory, &targets);
    let query = registry.query(&targets).unwrap();
    for (target, omitted) in [
        (
            "no_event_sentinels",
            "crates/loom-events/src/identifier/mod.rs",
        ),
        ("workspace_deps_pinned", "Cargo.toml"),
        (
            "no_event_sentinels",
            "crates/loom-walk/src/walk/no_event_sentinels.rs",
        ),
    ] {
        let mut document: serde_json::Value = serde_json::from_str(&query).unwrap();
        let unit = document["units"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|unit| unit["contract"]["target"] == target)
            .unwrap();
        let inputs = unit["contract"]["inputs"].as_array_mut().unwrap();
        let before = inputs.len();
        inputs.retain(|input| input["path"] != omitted);
        assert_eq!(before - inputs.len(), 1, "{omitted}");
        assert!(
            matches!(registry.resolve_query(&snapshot, &inventory, &document.to_string(), &contracts(&targets)), Err(Error::Admission { source, .. }) if matches!(*source, Error::Inconsistent { .. }))
        );
    }
    // Moving a description into its sibling slot must not change target attribution.
    let mut document: serde_json::Value = serde_json::from_str(&query).unwrap();
    document["units"][0] = document["units"][1].clone();
    assert!(
        registry
            .resolve_query(
                &snapshot,
                &inventory,
                &document.to_string(),
                &contracts(&targets)
            )
            .is_err()
    );
}

#[test]
fn raw_io_in_an_actual_checker_or_transitive_helper_blocks_registration() {
    let dir = fixture();
    let root = dir.path();
    for (file, raw) in [
        (
            "crates/loom-walk/src/walk/no_event_sentinels.rs",
            "fn untracked() { let _ = std::fs::read_to_string(\"secret\").ok(); }\n",
        ),
        (
            "crates/loom-walk/src/walk/util.rs",
            "use std::{fs as storage}; fn untracked() { let _ = storage::read(\"secret\"); }\n",
        ),
        (
            "crates/loom-walk/src/walk/source.rs",
            "impl Tracked<'_, '_> { fn raw() { std::fs::read(\"secret\"); } }\n",
        ),
    ] {
        let good = std::fs::read_to_string(root.join(file)).unwrap();
        write(root, file, &format!("{good}\n{raw}"));
        let (snapshot, inventory) = context(root);
        assert!(
            matches!(checked_definitions(&snapshot, &inventory, &[name("no_event_sentinels")]), Err(Error::UntrackedIo { checker, .. }) if checker == path(file))
        );
        write(root, file, &good);
    }
}

#[test]
fn tracked_transitive_template_reads_and_query_omissions_use_the_real_pinning_rule() {
    let dir = fixture();
    let root = dir.path();
    let acceptance = std::fs::read_to_string(root.join("specs/walks/tests.md")).unwrap();
    std::fs::remove_dir_all(root.join("specs/walks")).unwrap();
    write(root, "specs/walks.md", &acceptance);
    write(
        root,
        "docs/README.md",
        "- [walks](../specs/walks.md)\n- [templates](../specs/templates.md)\n",
    );
    write(
        root,
        "specs/templates.md",
        "# Templates\n\n### Pinning Policy\n\n| Partial | `plan` |\n|---|:-:|\n| `outer.md` | ✓ |\n| `inner.md` | ✓ |\n\n## Other\n",
    );
    write(
        root,
        "crates/loom-templates/templates/plan.md",
        "{% include \"partial/outer.md\" %}\n",
    );
    write(
        root,
        "crates/loom-templates/templates/partial/outer.md",
        "{% include \"partial/inner.md\" %}\n",
    );
    write(
        root,
        "crates/loom-templates/templates/partial/inner.md",
        "content\n",
    );
    let targets = [name("template_pinning_matrix")];
    let contracts = BTreeMap::from([(targets[0].clone(), path("specs/walks.md"))]);
    let (snapshot, inventory) = context(root);
    let registry = registry(&snapshot, &inventory, &targets);
    let query = registry.query(&targets).unwrap();
    let units = registry
        .resolve_query(&snapshot, &inventory, &query, &contracts)
        .unwrap();
    let result = units[0].run().unwrap();
    assert_eq!(result.outcome(), &Outcome::Pass);
    assert!(
        (loom_walk::walk::lookup("template_pinning_matrix")
            .unwrap()
            .run)(&loom_walk::walk::WalkInput::for_root(root))
        .pass
    );
    for relative in [
        "specs/templates.md",
        "crates/loom-templates/templates/partial/outer.md",
        "crates/loom-templates/templates/partial/inner.md",
    ] {
        assert!(result.observations().iter().any(|observation| matches!(observation, Observation::Content { path: read, .. } if read == &path(relative))), "{relative}");
        let mut document: serde_json::Value = serde_json::from_str(&query).unwrap();
        document["units"][0]["contract"]["inputs"]
            .as_array_mut()
            .unwrap()
            .retain(|input| input["path"] != relative);
        assert!(
            registry
                .resolve_query(&snapshot, &inventory, &document.to_string(), &contracts)
                .is_err()
        );
    }
    // The leaf loses reachability; unchanged top-level template cannot hide that mutation.
    write(
        root,
        "crates/loom-templates/templates/partial/outer.md",
        "no inner include\n",
    );
    assert!(registry.query(&targets).is_err());
    let (snapshot, inventory) = context(root);
    let registry = self::registry(&snapshot, &inventory, &targets);
    let query = registry.query(&targets).unwrap();
    assert!(matches!(
        registry
            .resolve_query(&snapshot, &inventory, &query, &contracts)
            .unwrap()[0]
            .run()
            .unwrap()
            .outcome(),
        Outcome::Fail { .. }
    ));
}

#[test]
fn captured_walk_checks_siblings_of_the_exact_fixture_exemption() {
    for sibling in ["fixture.rs_extra.rs", "fixture.rs_extra/ordinary.rs"] {
        let dir = fixture();
        let root = dir.path();
        let body = format!(
            "#[test]\nfn bad() {{ let _ = \"{}hidden\"; }}\n",
            concat!("/", "tmp/")
        );
        write(root, "crates/loom-walk/tests/fixture.rs", &body);
        assert_eq!(run_target(root, "no_hardcoded_tmp_paths"), Outcome::Pass);
        let manifest_path = root.join("crates/loom-walk/Cargo.toml");
        let manifest = std::fs::read_to_string(&manifest_path).unwrap();
        write(
            root,
            "crates/loom-walk/Cargo.toml",
            &format!("{manifest}\n[[test]]\nname='sibling'\npath='tests/{sibling}'\n"),
        );
        write(root, &format!("crates/loom-walk/tests/{sibling}"), &body);
        assert!(
            matches!(run_target(root, "no_hardcoded_tmp_paths"), Outcome::Fail { detail } if detail.contains(sibling) && detail.contains("hardcoded"))
        );
    }
}

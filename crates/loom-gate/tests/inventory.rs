#![expect(
    clippy::unwrap_used,
    reason = "Integration fixture helpers abort on invalid setup or external discovery-tool failure rather than manufacturing an inventory."
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use loom_gate::inventory::snapshot::{ResourcePath, Snapshot};
use loom_gate::inventory::{Duty, Error, Inventory, Name, Role};
use loom_test_support::proptest_config;
use tempfile::TempDir;

#[path = "inventory/native.rs"]
mod native;
#[path = "inventory/nix.rs"]
mod nix;

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn fixture(extra_manifest: &str, source: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let git = Command::new("git")
        .arg("init")
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(git.status.success());
    write(dir.path(), ".gitignore", "/target/\n");
    write(
        dir.path(),
        "Cargo.toml",
        &format!(
            "[package]\nname = 'fixture'\nversion = '0.1.0'\nedition = '2024'\n{extra_manifest}"
        ),
    );
    write(dir.path(), "src/lib.rs", source);
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        lock.status.success(),
        "{}",
        String::from_utf8_lossy(&lock.stderr)
    );
    dir
}

fn inventory(root: &Path) -> Inventory {
    Inventory::discover(&Snapshot::discover(root).unwrap()).unwrap()
}

fn name(text: &str) -> Name {
    text.to_owned().try_into().unwrap()
}
fn path(text: &str) -> ResourcePath {
    PathBuf::from(text).try_into().unwrap()
}

#[test]
fn native_cargo_target_derives_build_lint_and_applicable_test_obligations() {
    let dir = fixture("", "mod nested;\n");
    write(dir.path(), "src/nested.rs", "pub fn value() {}\n");
    let inventory = inventory(dir.path());
    let target = name("cargo:fixture:lib:fixture");
    let subject = inventory.subject(&target).unwrap();
    assert_eq!(subject.role(), Role::RustSource);
    assert_eq!(
        subject.members(),
        &BTreeSet::from([path("src/lib.rs"), path("src/nested.rs")])
    );
    let duties: BTreeSet<_> = inventory
        .obligations()
        .iter()
        .filter(|obligation| obligation.subject() == &target)
        .map(loom_gate::inventory::Obligation::duty)
        .collect();
    assert_eq!(
        duties,
        BTreeSet::from([Duty::Build, Duty::Lint, Duty::Test])
    );
}

#[test]
fn non_testable_targets_do_not_acquire_inapplicable_test_obligations() {
    let dir = fixture("[lib]\ntest = false\n", "pub fn value() {}\n");
    let inventory = inventory(dir.path());
    assert!(
        inventory
            .obligations()
            .iter()
            .all(|obligation| obligation.duty() != Duty::Test)
    );
    assert!(
        inventory
            .obligations()
            .iter()
            .any(|obligation| obligation.duty() == Duty::Build)
    );
}

#[test]
fn intentionally_invalid_fixture_keeps_consuming_test_obligations() {
    let dir = fixture("", "pub fn value() {}\n");
    write(dir.path(), "tests/fixtures/invalid.rs", "not Rust syntax!");
    let inventory = inventory(dir.path());
    let resource = name("cargo:fixture:resource:fixtures");
    let fixture = inventory.subject(&resource).unwrap();
    assert_eq!(fixture.role(), Role::TestFixture);
    assert_eq!(
        fixture.members(),
        &BTreeSet::from([path("tests/fixtures/invalid.rs")])
    );
    let obligations: Vec<_> = inventory
        .obligations()
        .iter()
        .filter(|obligation| obligation.subject() == &resource)
        .collect();
    assert_eq!(obligations.len(), 1);
    assert_eq!(obligations[0].duty(), Duty::Test);
    assert_eq!(
        obligations[0].responsible(),
        &name("cargo:fixture:lib:fixture")
    );
}

#[test]
fn fixture_without_applicable_consumer_is_not_implicitly_covered() {
    let dir = fixture("[lib]\ntest = false\n", "pub fn value() {}\n");
    write(dir.path(), "tests/fixtures/invalid.rs", "not Rust");
    assert!(matches!(
        Inventory::discover(&Snapshot::discover(dir.path()).unwrap()),
        Err(Error::Responsibility { .. })
    ));
}

#[test]
fn document_policy_survives_inclusion_in_a_native_execution_input() {
    let dir = fixture(
        "",
        "pub const DOC: &str = include_str!(\"../README.md\");\n",
    );
    write(dir.path(), "README.md", "# Documentation\n");
    let inventory = inventory(dir.path());
    assert!(
        inventory
            .obligations()
            .iter()
            .any(
                |obligation| obligation.subject() == &name("resource:README.md")
                    && obligation.duty() == Duty::Document
            )
    );
    assert!(
        inventory
            .subject(&name("cargo:fixture:lib:fixture"))
            .unwrap()
            .members()
            .contains(&path("README.md"))
    );
}

#[test]
fn generated_project_resource_requires_both_producer_and_consumer() {
    let declaration = r"
[[package.metadata.loom.verify.resource]]
name = 'generated:table'
directory = 'generated'
role = 'generated'
consumers = ['cargo:fixture:lib:fixture']
producer = 'cargo:fixture:custom-build:build-script-build'
";
    let dir = fixture(declaration, "pub fn value() {}\n");
    write(dir.path(), "build.rs", "fn main() {}\n");
    write(dir.path(), "generated/table.bin", "generated bytes");
    let inventory = inventory(dir.path());
    let duties: BTreeSet<_> = inventory
        .obligations()
        .iter()
        .filter(|obligation| obligation.subject() == &name("generated:table"))
        .map(loom_gate::inventory::Obligation::duty)
        .collect();
    assert_eq!(
        duties,
        BTreeSet::from([Duty::Build, Duty::Lint, Duty::Produce, Duty::Test])
    );
}

#[test]
fn generated_resource_without_producer_fails_actionably() {
    let declaration = r"
[[package.metadata.loom.verify.resource]]
name = 'generated:table'
directory = 'generated'
role = 'generated'
consumers = ['cargo:fixture:lib:fixture']
";
    let dir = fixture(declaration, "pub fn value() {}\n");
    write(dir.path(), "generated/table.bin", "generated bytes");
    let error = Inventory::discover(&Snapshot::discover(dir.path()).unwrap()).unwrap_err();
    let feedback = error.to_string();
    assert!(
        feedback.contains("generated:table")
            && feedback.contains("consumer/producer")
            && feedback.contains("loom gate verify")
    );
}

#[test]
fn project_resource_membership_tracks_additions_without_a_policy_file_list() {
    let dir = fixture("", "pub fn value() {}\n");
    write(dir.path(), "tests/fixtures/first.rs", "invalid Rust");
    let before = inventory(dir.path());
    write(dir.path(), "tests/fixtures/new.rs", "also invalid Rust");
    let after = inventory(dir.path());
    assert_ne!(before.snapshot(), after.snapshot());
    assert_eq!(
        after
            .subject(&name("cargo:fixture:resource:fixtures"))
            .unwrap()
            .members()
            .len(),
        2
    );
}

#[test]
fn new_unowned_rust_source_blocks_even_with_existing_native_target_coverage() {
    let dir = fixture("", "pub fn value() {}\n");
    let before = inventory(dir.path());
    assert!(!before.obligations().is_empty());
    write(dir.path(), "src/orphan.rs", "pub fn hidden() {}\n");
    let error = Inventory::discover(&Snapshot::discover(dir.path()).unwrap()).unwrap_err();
    assert!(matches!(error, Error::Orphan { .. }));
    assert!(error.to_string().contains("src/orphan.rs"));
}

#[test]
fn new_source_kind_is_not_covered_by_an_unrelated_native_target() {
    let dir = fixture("", "pub fn value() {}\n");
    write(dir.path(), "src/new.py", "print('new implementation')\n");
    assert!(matches!(
        Inventory::discover(&Snapshot::discover(dir.path()).unwrap()),
        Err(Error::Unclassified { .. })
    ));
}

#[test]
fn deleted_module_invalidates_discovery_instead_of_shrinking_required_coverage() {
    let dir = fixture("", "mod child;\n");
    write(dir.path(), "src/child.rs", "pub fn value() {}\n");
    let before = Snapshot::discover(dir.path()).unwrap();
    Inventory::discover(&before).unwrap();
    std::fs::remove_file(dir.path().join("src/child.rs")).unwrap();
    assert!(before.require_current().is_err());
    assert!(matches!(
        Inventory::discover(&Snapshot::discover(dir.path()).unwrap()),
        Err(Error::Snapshot(_))
    ));
}

#[test]
fn module_rename_requires_current_native_definition_and_changes_membership() {
    let dir = fixture("", "mod child;\n");
    write(dir.path(), "src/child.rs", "pub fn value() {}\n");
    let before = inventory(dir.path());
    std::fs::rename(
        dir.path().join("src/child.rs"),
        dir.path().join("src/renamed.rs"),
    )
    .unwrap();
    assert!(Inventory::discover(&Snapshot::discover(dir.path()).unwrap()).is_err());
    write(dir.path(), "src/lib.rs", "mod renamed;\n");
    let after = inventory(dir.path());
    assert_ne!(before.snapshot(), after.snapshot());
    let subject = after.subject(&name("cargo:fixture:lib:fixture")).unwrap();
    assert!(subject.members().contains(&path("src/renamed.rs")));
    assert!(!subject.members().contains(&path("src/child.rs")));
}

#[test]
fn new_cargo_target_adds_independent_obligations_without_provider_registration() {
    let dir = fixture("", "pub fn value() {}\n");
    let before = inventory(dir.path());
    write(dir.path(), "src/bin/new.rs", "fn main() {}\n");
    let after = inventory(dir.path());
    assert!(before.subject(&name("cargo:fixture:bin:new")).is_err());
    assert!(after.subject(&name("cargo:fixture:bin:new")).is_ok());
    assert!(after.obligations().len() > before.obligations().len());
}

#[test]
fn duplicate_resource_names_cannot_resolve_ambiguously() {
    let declaration = r"
[[package.metadata.loom.verify.resource]]
name = 'fixture:same'
directory = 'fixtures'
role = 'test-fixture'
consumers = ['cargo:fixture:lib:fixture']
[[package.metadata.loom.verify.resource]]
name = 'fixture:same'
directory = 'fixtures'
role = 'test-fixture'
consumers = ['cargo:fixture:lib:fixture']
";
    let dir = fixture(declaration, "pub fn value() {}\n");
    write(dir.path(), "fixtures/data.bin", "fixture");
    assert!(matches!(
        Inventory::discover(&Snapshot::discover(dir.path()).unwrap()),
        Err(Error::Ambiguous { .. })
    ));
}

fn strategy(root: &Path, subject: &str, role: &str) {
    write(root, "specs/example/spec.md", "# Example contract\n");
    write(
        root,
        "docs/README.md",
        "- [example](../specs/example/spec.md)\n",
    );
    write(
        root,
        "specs/example/tests.md",
        &format!(
            "```loom-verify\n[[role_exception]]\nsubject = '{subject}'\nrole = '{role}'\nreason = 'Project-defined classification.'\n```\n"
        ),
    );
}

#[test]
fn unknown_role_exception_names_report_owning_policy_source_and_retry_path() {
    let dir = fixture("", "pub fn value() {}\n");
    strategy(dir.path(), "fixture:absent", "test-fixture");
    let error = Inventory::discover(&Snapshot::discover(dir.path()).unwrap()).unwrap_err();
    let feedback = error.to_string();
    assert!(
        feedback.contains("fixture:absent")
            && feedback.contains("specs/example/tests.md")
            && feedback.contains("loom gate verify")
    );
}

#[test]
fn role_exception_keeps_native_consuming_test_responsibility() {
    let dir = fixture("", "pub fn value() {}\n");
    strategy(dir.path(), "cargo:fixture:lib:fixture", "test-fixture");
    let resolved = inventory(dir.path());
    let target = name("cargo:fixture:lib:fixture");
    let duties: BTreeSet<_> = resolved
        .obligations()
        .iter()
        .filter(|obligation| obligation.subject() == &target)
        .map(loom_gate::inventory::Obligation::duty)
        .collect();
    assert_eq!(duties, BTreeSet::from([Duty::Test]));
    assert_eq!(
        resolved.subject(&target).unwrap().members(),
        &BTreeSet::from([path("src/lib.rs")])
    );
}

#[test]
fn bootstrap_contract_owns_role_policy_without_loading_support_document_policy() {
    let dir = fixture("", "pub fn value() {}\n");
    strategy(dir.path(), "cargo:fixture:lib:fixture", "test-fixture");
    let acceptance = std::fs::read_to_string(dir.path().join("specs/example/tests.md")).unwrap();
    std::fs::remove_file(dir.path().join("specs/example/tests.md")).unwrap();
    std::fs::remove_file(dir.path().join("specs/example/spec.md")).unwrap();
    write(
        dir.path(),
        "docs/README.md",
        "- [example](../specs/example.md)\n",
    );
    write(dir.path(), "specs/example.md", &acceptance);
    write(
        dir.path(),
        "specs/example/support.md",
        "```loom-verify\n[[role_exception]]\nsubject = 'absent:owner'\nrole = 'generated'\nreason = 'Not acceptance policy.'\n```\n",
    );
    let resolved = inventory(dir.path());
    let target = name("cargo:fixture:lib:fixture");
    assert_eq!(resolved.subject(&target).unwrap().role(), Role::TestFixture);
    assert!(
        resolved
            .obligations()
            .iter()
            .filter(|obligation| obligation.subject() == &target)
            .all(|obligation| obligation.duty() == Duty::Test)
    );
}

#[test]
fn canonical_contract_body_cannot_override_owning_acceptance_policy() {
    let dir = fixture("", "pub fn value() {}\n");
    strategy(dir.path(), "cargo:fixture:lib:fixture", "test-fixture");
    write(
        dir.path(),
        "specs/example/spec.md",
        "```loom-verify\n[[role_exception]]\nsubject = 'cargo:fixture:lib:fixture'\nrole = 'generated'\nreason = 'Not acceptance policy.'\n```\n",
    );
    assert_eq!(
        inventory(dir.path())
            .subject(&name("cargo:fixture:lib:fixture"))
            .unwrap()
            .role(),
        Role::TestFixture
    );
}

#[test]
fn deleting_required_acceptance_document_fails_closed_with_ownership_feedback() {
    let dir = fixture("", "pub fn value() {}\n");
    strategy(dir.path(), "cargo:fixture:lib:fixture", "test-fixture");
    std::fs::remove_file(dir.path().join("specs/example/tests.md")).unwrap();
    let error = Inventory::discover(&Snapshot::discover(dir.path()).unwrap()).unwrap_err();
    assert!(matches!(error, Error::Package(_)));
    assert!(error.to_string().contains("specs/example/tests.md"));
    assert!(error.to_string().contains("loom gate verify"));
}

#[test]
fn source_to_document_exception_cannot_erase_native_obligations() {
    let dir = fixture("", "pub fn value() {}\n");
    strategy(dir.path(), "cargo:fixture:lib:fixture", "document");
    assert!(Inventory::discover(&Snapshot::discover(dir.path()).unwrap()).is_err());
}

#[test]
fn spec_owned_globs_never_become_project_subject_membership() {
    let dir = fixture("", "pub fn value() {}\n");
    strategy(dir.path(), "src/*.rs", "test-fixture");
    assert!(Inventory::discover(&Snapshot::discover(dir.path()).unwrap()).is_err());
}

#[test]
fn execution_wiring_cannot_remove_spec_owned_role_obligations() {
    let dir = fixture("", "pub fn value() {}\n");
    strategy(dir.path(), "cargo:fixture:lib:fixture", "test-fixture");
    let before = inventory(dir.path());
    write(dir.path(), "loom.toml", "[runner.test]\ncommand = 'true'\n");
    let after = inventory(dir.path());
    let target = name("cargo:fixture:lib:fixture");
    assert_eq!(after.subject(&target).unwrap().role(), Role::TestFixture);
    assert_eq!(
        before
            .obligations()
            .iter()
            .filter(|obligation| obligation.subject() == &target)
            .collect::<Vec<_>>(),
        after
            .obligations()
            .iter()
            .filter(|obligation| obligation.subject() == &target)
            .collect::<Vec<_>>()
    );
}

#[test]
fn removing_optional_role_policy_restores_native_baseline_instead_of_dropping_work() {
    let dir = fixture("", "pub fn value() {}\n");
    strategy(dir.path(), "cargo:fixture:lib:fixture", "test-fixture");
    let before = inventory(dir.path());
    write(
        dir.path(),
        "specs/example/tests.md",
        "# No optional role exceptions\n",
    );
    let after = inventory(dir.path());
    let target = name("cargo:fixture:lib:fixture");
    assert_eq!(before.subject(&target).unwrap().role(), Role::TestFixture);
    assert_eq!(after.subject(&target).unwrap().role(), Role::RustSource);
    assert!(
        after
            .obligations()
            .iter()
            .any(|obligation| obligation.subject() == &target && obligation.duty() == Duty::Test)
    );
    assert!(
        after
            .obligations()
            .iter()
            .any(|obligation| obligation.subject() == &target && obligation.duty() == Duty::Build)
    );
}

#[test]
fn project_document_resource_cannot_hide_unclassified_source() {
    let declaration = r"
[[package.metadata.loom.verify.resource]]
name = 'document:source'
directory = 'source'
role = 'document'
consumers = []
";
    let dir = fixture(declaration, "pub fn value() {}\n");
    write(dir.path(), "source/hidden.py", "print('implementation')\n");
    assert!(matches!(
        Inventory::discover(&Snapshot::discover(dir.path()).unwrap()),
        Err(Error::Unclassified { .. })
    ));
}

#[test]
fn malformed_project_resource_reports_native_definition_context() {
    let dir = fixture(
        "[package.metadata.loom.verify]\ntrusted = true\n",
        "pub fn value() {}\n",
    );
    let error = Inventory::discover(&Snapshot::discover(dir.path()).unwrap()).unwrap_err();
    assert!(matches!(error, Error::Definition { .. }));
    let feedback = error.to_string();
    assert!(feedback.contains("Cargo.toml") && feedback.contains("loom gate verify"));
}

#[test]
fn standalone_nix_sources_have_evaluation_and_check_responsibility() {
    let dir = fixture("", "pub fn value() {}\n");
    write(dir.path(), "check.nix", "{ value = 1; }\n");
    let inventory = inventory(dir.path());
    let duties: BTreeSet<_> = inventory
        .obligations()
        .iter()
        .filter(|obligation| obligation.subject() == &name("nix:check.nix"))
        .map(loom_gate::inventory::Obligation::duty)
        .collect();
    assert_eq!(duties, BTreeSet::from([Duty::Evaluate, Duty::Check]));
}

#[test]
fn ignored_build_artifacts_do_not_become_project_subjects() {
    let dir = fixture("", "pub fn value() {}\n");
    let before = Snapshot::discover(dir.path()).unwrap();
    write(dir.path(), "target/generated.rs", "not project source");
    let after = Snapshot::discover(dir.path()).unwrap();
    assert_eq!(before.identity(), after.identity());
    assert!(!after.files().contains_key(&path("target/generated.rs")));
}

#[test]
fn snapshot_reads_captured_content_but_rejects_changed_native_discovery() {
    let dir = fixture("", "pub fn value() {}\n");
    let snapshot = Snapshot::discover(dir.path()).unwrap();
    let cloned = snapshot.clone();
    assert!(std::ptr::eq(snapshot.files(), cloned.files()));
    write(dir.path(), "src/lib.rs", "pub fn changed() {}\n");
    assert_eq!(
        snapshot.text(&path("src/lib.rs")).unwrap(),
        "pub fn value() {}\n"
    );
    assert!(Inventory::discover(&snapshot).is_err());
    assert!(cloned.require_current().is_err());
    assert_eq!(
        cloned.text(&path("src/lib.rs")).unwrap(),
        "pub fn value() {}\n"
    );
}

#[test]
fn source_path_attributes_resolve_internal_traversal_without_external_reads() {
    let dir = fixture("", "#[path = \"../shared.rs\"] mod shared;\n");
    write(dir.path(), "shared.rs", "pub fn value() {}\n");
    assert!(
        inventory(dir.path())
            .subject(&name("cargo:fixture:lib:fixture"))
            .unwrap()
            .members()
            .contains(&path("shared.rs"))
    );
}

#[test]
fn integration_test_roots_resolve_modules_relative_to_the_native_root() {
    let dir = fixture("", "pub fn value() {}\n");
    write(dir.path(), "tests/integration.rs", "mod common;\n");
    write(dir.path(), "tests/common/mod.rs", "pub fn fixture() {}\n");
    assert!(
        inventory(dir.path())
            .subject(&name("cargo:fixture:test:integration"))
            .unwrap()
            .members()
            .contains(&path("tests/common/mod.rs"))
    );
}

#[test]
fn binary_include_captures_actual_resource_bytes_without_utf8_assumptions() {
    let dir = fixture(
        "",
        "pub const DATA: &[u8] = include_bytes!(\"../data.toml\");\n",
    );
    std::fs::write(dir.path().join("data.toml"), [0xff, 0xfe]).unwrap();
    assert!(
        inventory(dir.path())
            .subject(&name("cargo:fixture:lib:fixture"))
            .unwrap()
            .members()
            .contains(&path("data.toml"))
    );
}

#[test]
fn raw_module_identifiers_resolve_native_filesystem_names() {
    let dir = fixture("", "mod r#type;\nmod r#async { mod inner; }\n");
    write(dir.path(), "src/type.rs", "pub fn value() {}\n");
    write(dir.path(), "src/async/inner.rs", "pub fn value() {}\n");
    let inventory = inventory(dir.path());
    let members = inventory
        .subject(&name("cargo:fixture:lib:fixture"))
        .unwrap()
        .members();
    assert!(members.contains(&path("src/type.rs")));
    assert!(members.contains(&path("src/async/inner.rs")));
}

#[test]
fn ambiguous_module_layout_fails_instead_of_selecting_one_candidate() {
    let dir = fixture("", "mod child;\n");
    write(dir.path(), "src/child.rs", "pub fn value() {}\n");
    write(dir.path(), "src/child/mod.rs", "pub fn other() {}\n");
    assert!(matches!(
        Inventory::discover(&Snapshot::discover(dir.path()).unwrap()),
        Err(Error::Module { .. })
    ));
}

#[test]
fn manifest_relative_include_tracks_the_discovered_package_directory() {
    let dir = fixture("", "pub fn root() {}\n");
    let manifest = std::fs::read_to_string(dir.path().join("Cargo.toml")).unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        &format!("{manifest}\n[workspace]\nmembers = ['child']\n"),
    );
    write(dir.path(), "README.md", "# Shared input\n");
    write(
        dir.path(),
        "child/Cargo.toml",
        "[package]\nname='child'\nversion='0.1.0'\nedition='2024'\n",
    );
    write(
        dir.path(),
        "child/src/lib.rs",
        "mod inline { mod nested; }\n",
    );
    write(
        dir.path(),
        "child/src/inline/nested.rs",
        r#"pub const DOC: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../README.md"));"#,
    );
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        lock.status.success(),
        "{}",
        String::from_utf8_lossy(&lock.stderr)
    );
    assert!(
        inventory(dir.path())
            .subject(&name("cargo:child:lib:child"))
            .unwrap()
            .members()
            .contains(&path("README.md"))
    );
}

#[test]
fn nested_static_concat_include_collects_root_package_source() {
    let dir = fixture(
        "",
        r#"include!(concat!(concat!(env!("CARGO_MANIFEST_DIR")), "/src/", "generated.rs"));"#,
    );
    write(dir.path(), "src/generated.rs", "pub fn generated() {}\n");
    assert!(
        inventory(dir.path())
            .subject(&name("cargo:fixture:lib:fixture"))
            .unwrap()
            .members()
            .contains(&path("src/generated.rs"))
    );
}

#[test]
fn include_discovery_rejects_ambient_environment_membership() {
    let dir = fixture(
        "",
        r#"pub const DOC: &str = include_str!(concat!(env!("CARGO_HOME"), "/README.md"));"#,
    );
    assert!(
        matches!(Inventory::discover(&Snapshot::discover(dir.path()).unwrap()), Err(Error::Syntax { source, .. }) if source.to_string().contains("ambient variables cannot establish tracked membership"))
    );
}

#[test]
fn manifest_relative_include_cannot_escape_the_snapshot() {
    let dir = fixture(
        "",
        r#"pub const DOC: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../outside.md"));"#,
    );
    assert!(matches!(
        Inventory::discover(&Snapshot::discover(dir.path()).unwrap()),
        Err(Error::Outside { .. })
    ));
}

proptest::proptest! {
    #![proptest_config(proptest_config())]
    #[test]
    fn resource_paths_never_admit_traversal_or_glob_membership(segment in "[a-z]{1,20}") {
        for invalid in [format!("../{segment}"), format!("/{segment}"), format!("{segment}/*.rs"), format!("{segment}/?.rs")] {
            proptest::prop_assert!(ResourcePath::try_from(PathBuf::from(invalid)).is_err());
        }
        proptest::prop_assert!(ResourcePath::try_from(PathBuf::from(segment)).is_ok());
    }
}

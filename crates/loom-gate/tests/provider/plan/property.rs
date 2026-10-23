use std::collections::{BTreeSet, HashSet};
use std::process::Command;

use guppy::graph::PackageGraph;
use loom_gate::annotation::strategy::{Declaration, PropertyFilter, Source};
use loom_gate::provider::plan::property::{Applicability, Context, Error, Request};
use nextest_filtering::{
    CompiledExpr, EvalContext, FiltersetLeaf, GroupLookup, KnownGroups, NameMatcher, TestQuery,
};
use nextest_metadata::TestListSummary;

use super::{fixture, name, write};

#[derive(Debug)]
struct Global;
impl GroupLookup for Global {
    fn is_member_test(&self, _test: &TestQuery<'_>, matcher: &NameMatcher) -> bool {
        matcher.is_match("@global")
    }
}

struct Native {
    dir: tempfile::TempDir,
    graph: PackageGraph,
    tests: TestListSummary,
    default: CompiledExpr,
    groups: KnownGroups,
}

impl Native {
    fn discover() -> Self {
        let dir = fixture();
        write(
            dir.path(),
            "Cargo.toml",
            "[package]\nname = 'provider-fixture'\nversion = '0.1.0'\nedition = '2024'\n[workspace]\nmembers = ['.', 'support']\n[dependencies]\nshared-fixture = { path = 'support' }\n",
        );
        write(
            dir.path(),
            "support/Cargo.toml",
            "[package]\nname = 'shared-fixture'\nversion = '0.1.0'\nedition = '2024'\n",
        );
        write(
            dir.path(),
            "support/src/lib.rs",
            "#[test] fn support_test() {}\n",
        );
        Self::command(&dir, &["generate-lockfile", "--offline"]);
        write(
            dir.path(),
            "src/lib.rs",
            "pub fn shared() {}\n#[cfg(test)] mod tests { #[test] fn ordinary() {} #[test] fn campaign_one() {} #[test] #[ignore] fn campaign_ignored() {} }\n",
        );
        write(
            dir.path(),
            "tests/campaigns.rs",
            "#[test] fn independent_campaign() {}\n#[test] fn newly_added_without_annotation() {}\n",
        );
        let metadata = Self::command(
            &dir,
            &["metadata", "--offline", "--locked", "--format-version=1"],
        );
        let graph = PackageGraph::from_json(metadata).unwrap();
        let tests = TestListSummary::parse_json(Self::command(
            &dir,
            &[
                "nextest",
                "list",
                "--workspace",
                "--all-targets",
                "--offline",
                "--locked",
                "--message-format=json",
            ],
        ))
        .unwrap();
        assert_eq!(tests.test_count, 6);
        Self {
            dir,
            graph,
            tests,
            default: CompiledExpr::Set(FiltersetLeaf::All),
            groups: KnownGroups::Known {
                custom_groups: HashSet::new(),
            },
        }
    }

    fn command(dir: &tempfile::TempDir, args: &[&str]) -> String {
        let output = Command::new("cargo")
            .args(args)
            .env("CARGO_BUILD_JOBS", "1")
            .env("CARGO_TARGET_DIR", dir.path().join("target"))
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "cargo {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn context(&self) -> Context<'_> {
        Context {
            graph: &self.graph,
            tests: &self.tests,
            evaluation: EvalContext {
                default_filter: &self.default,
            },
            known_groups: &self.groups,
            groups: &Global,
        }
    }
}

fn filters(expressions: &[&str]) -> (Source, Vec<PropertyFilter>) {
    let content = format!(
        "```loom-verify\n[rust]\nproperty_filters = {}\n```\n",
        serde_json::to_string(expressions).unwrap()
    );
    let parsed = loom_gate::annotation::parse_content(
        std::path::Path::new("specs/example/tests.md"),
        &content,
    )
    .unwrap();
    parsed
        .strategy
        .into_iter()
        .find_map(|declaration| match declaration {
            Declaration::Package { source, policy } => {
                Some((source, policy.rust.unwrap().property_filters))
            }
            Declaration::Criterion { .. } => None,
        })
        .unwrap()
}

#[test]
fn actual_native_tests_partition_by_upstream_package_binary_and_test_expressions() {
    let native = Native::discover();
    let (source, filters) = filters(&[
        "package(=provider-fixture) & kind(lib) & test(campaign_)",
        "binary(=campaigns) & test(=independent_campaign)",
    ]);
    let requests: Vec<_> = filters
        .iter()
        .map(|filter| Request {
            source: &source,
            filter,
            applicability: Applicability::Applicable,
        })
        .collect();
    let partition = native.context().resolve(&requests).unwrap();
    assert_eq!(partition.groups()[0].members().len(), 2);
    assert!(
        partition.groups()[0]
            .members()
            .iter()
            .any(loom_gate::provider::plan::property::Test::ignored)
    );
    assert_eq!(partition.groups()[1].members().len(), 1);
    assert_eq!(partition.ordinary().len(), 3);
    assert!(
        partition
            .ordinary()
            .iter()
            .any(|test| test.name().as_str() == "newly_added_without_annotation")
    );
}

#[test]
fn upstream_dependency_predicates_use_the_real_cargo_graph() {
    let native = Native::discover();
    let (source, filters) = filters(&["deps(=provider-fixture) & package(=shared-fixture)"]);
    let partition = native
        .context()
        .resolve(&[Request {
            source: &source,
            filter: &filters[0],
            applicability: Applicability::Applicable,
        }])
        .unwrap();
    assert_eq!(partition.groups()[0].members().len(), 1);
    assert_eq!(
        partition.groups()[0]
            .members()
            .first()
            .unwrap()
            .name()
            .as_str(),
        "support_test"
    );
}

#[test]
fn upstream_group_profile_and_regex_semantics_are_not_replaced_by_test_name_matching() {
    let mut native = Native::discover();
    native.default = CompiledExpr::Set(FiltersetLeaf::None);
    let (source, expressions) =
        filters(&["group(@global) & test(/^independent_/) & not default()"]);
    let partition = native
        .context()
        .resolve(&[Request {
            source: &source,
            filter: &expressions[0],
            applicability: Applicability::Applicable,
        }])
        .unwrap();
    assert_eq!(partition.groups()[0].members().len(), 1);
    let (source, unknown) = filters(&["group(=undeclared-nextest-group)"]);
    assert!(matches!(
        native.context().resolve(&[Request {
            source: &source,
            filter: &unknown[0],
            applicability: Applicability::Applicable
        }]),
        Err(Error::Filter { .. })
    ));
}

#[test]
fn unavailable_native_nextest_group_context_is_explicit_not_an_upstream_panic_or_empty_inventory() {
    let mut native = Native::discover();
    native.groups = KnownGroups::Unavailable;
    let (source, filters) = filters(&["group(@global)"]);
    assert!(matches!(
        native.context().resolve(&[Request {
            source: &source,
            filter: &filters[0],
            applicability: Applicability::Applicable
        }]),
        Err(Error::GroupsUnavailable { .. })
    ));
}

#[test]
fn empty_applicable_filter_is_rejected_before_zero_affected_selection_and_correction_works() {
    let native = Native::discover();
    let (source, invalid) = filters(&["test(=misspelled_campaign)"]);
    let error = native.context().resolve(&[Request {
        source: &source,
        filter: &invalid[0],
        applicability: Applicability::Applicable,
    }]);
    assert!(
        matches!(error, Err(Error::Empty { expression, .. }) if expression == "test(=misspelled_campaign)")
    );
    let (source, corrected) = filters(&["test(=independent_campaign)"]);
    let partition = native
        .context()
        .resolve(&[Request {
            source: &source,
            filter: &corrected[0],
            applicability: Applicability::Applicable,
        }])
        .unwrap();
    assert_eq!(partition.groups()[0].members().len(), 1);
    assert!(
        partition
            .affected(&BTreeSet::new())
            .all(|(_, members)| members.is_empty())
    );
}

#[test]
fn inapplicable_and_capability_unavailable_groups_are_not_empty_configuration_errors() {
    let native = Native::discover();
    let (source, filters) = filters(&[
        "test(=not_on_this_platform)",
        "test(=needs_external_capability)",
    ]);
    let requests = [
        Request {
            source: &source,
            filter: &filters[0],
            applicability: Applicability::Inapplicable {
                system: name("aarch64-darwin"),
            },
        },
        Request {
            source: &source,
            filter: &filters[1],
            applicability: Applicability::Unavailable {
                capability: loom_gate::provider::Capability::HostKernel,
                detail: "fixture kernel unavailable".into(),
            },
        },
    ];
    let partition = native.context().resolve(&requests).unwrap();
    assert!(matches!(
        partition.groups()[0].applicability(),
        Applicability::Inapplicable { .. }
    ));
    assert!(matches!(
        partition.groups()[1].applicability(),
        Applicability::Unavailable { .. }
    ));
    assert_eq!(partition.ordinary().len(), 6);
}

#[test]
fn incomplete_binary_listing_is_not_a_successful_empty_group() {
    let mut native = Native::discover();
    let (source, filters) = filters(&["test(=independent_campaign)"]);
    let suite = native.tests.rust_suites.values_mut().next().unwrap();
    suite.status = nextest_metadata::RustTestSuiteStatusSummary::SKIPPED;
    assert!(matches!(
        native.context().resolve(&[Request {
            source: &source,
            filter: &filters[0],
            applicability: Applicability::Applicable
        }]),
        Err(Error::Incomplete { .. })
    ));
}

#[test]
fn overlapping_groups_keep_one_native_identity_instead_of_dropping_or_renaming_tests() {
    let native = Native::discover();
    let (source, filters) = filters(&["test(campaign)", "binary(=campaigns)"]);
    let requests: Vec<_> = filters
        .iter()
        .map(|filter| Request {
            source: &source,
            filter,
            applicability: Applicability::Applicable,
        })
        .collect();
    let partition = native.context().resolve(&requests).unwrap();
    let mut covered = partition.ordinary().clone();
    for group in partition.groups() {
        covered.extend(group.members().iter().cloned());
    }
    assert_eq!(covered.len(), native.tests.test_count);
    assert!(
        partition.groups()[0]
            .members()
            .intersection(partition.groups()[1].members())
            .any(|test| test.name().as_str() == "independent_campaign")
    );
    assert!(native.dir.path().join("tests/campaigns.rs").is_file());
}

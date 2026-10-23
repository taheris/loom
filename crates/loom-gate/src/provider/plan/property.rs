//! Native property membership is resolved before affectedness; a partition grants no execution authority.

use std::collections::BTreeSet;

use displaydoc::Display;
use guppy::{
    PackageId,
    graph::{BuildTargetId, PackageGraph},
};
use nextest_filtering::{
    BinaryQuery, EvalContext, Filterset, FiltersetKind, GroupLookup, KnownGroups, ParseContext,
    TestQuery,
};
use nextest_metadata::{
    RustBinaryId, RustTestBinaryKind, RustTestSuiteStatusSummary, TestCaseName, TestListSummary,
};
use thiserror::Error;

use crate::annotation::strategy::{PropertyFilter, Source};
use crate::inventory::Name;
use crate::provider::Capability;

/// Native/config-derived applicability is not inferred from an empty match or a diff.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Applicability {
    Applicable,
    Inapplicable {
        system: Name,
    },
    Unavailable {
        capability: Capability,
        detail: String,
    },
}

pub struct Request<'a> {
    pub source: &'a Source,
    pub filter: &'a PropertyFilter,
    pub applicability: Applicability,
}

/// Exact identities retain shared-binary and ignored tests without a property-name heuristic.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct Test {
    binary: RustBinaryId,
    name: TestCaseName,
    ignored: bool,
    kind: Option<String>,
    runnable: bool,
}

impl Test {
    pub const fn binary(&self) -> &RustBinaryId {
        &self.binary
    }
    pub const fn name(&self) -> &TestCaseName {
        &self.name
    }
    pub const fn ignored(&self) -> bool {
        self.ignored
    }
    pub fn kind(&self) -> Option<&str> {
        self.kind.as_deref()
    }
    pub const fn runnable(&self) -> bool {
        self.runnable
    }
}

pub struct Group {
    source: Source,
    filter: PropertyFilter,
    applicability: Applicability,
    members: BTreeSet<Test>,
}

impl Group {
    pub const fn source(&self) -> &Source {
        &self.source
    }
    pub const fn filter(&self) -> &PropertyFilter {
        &self.filter
    }
    pub const fn applicability(&self) -> &Applicability {
        &self.applicability
    }
    pub const fn members(&self) -> &BTreeSet<Test> {
        &self.members
    }
}

/// Overlapping explicit groups share exact test identities; ordinary tests are their complement.
pub struct Partition {
    groups: Vec<Group>,
    ordinary: BTreeSet<Test>,
}

/// Serialize-only classification diagnostics, not execution or receipt authority.
#[derive(serde::Serialize)]
pub struct Report {
    groups: Vec<GroupReport>,
    ordinary: BTreeSet<Test>,
}

#[derive(serde::Serialize)]
struct GroupReport {
    document: std::path::PathBuf,
    line: u32,
    filter: String,
    applicability: Applicability,
    members: BTreeSet<Test>,
}

/// Read owned strategy declarations from the captured contracts, not live files.
///
/// # Errors
/// Rejects missing or malformed source declarations.
pub fn declarations(
    snapshot: &crate::inventory::snapshot::Snapshot,
) -> Result<Vec<(Source, PropertyFilter)>, crate::provider::Error> {
    let mut declarations = Vec::new();
    for path in crate::inventory::contracts(snapshot)? {
        let parsed = crate::annotation::parse_content(path.as_path(), snapshot.text(&path)?)
            .map_err(crate::inventory::Error::from)?;
        for declaration in parsed.strategy {
            if let crate::annotation::strategy::Declaration::Package { source, policy } =
                declaration
                && let Some(rust) = policy.rust
            {
                declarations.extend(
                    rust.property_filters
                        .into_iter()
                        .map(|filter| (source.clone(), filter)),
                );
            }
        }
    }
    Ok(declarations)
}

impl Partition {
    pub fn report(&self) -> Report {
        Report {
            groups: self
                .groups
                .iter()
                .map(|group| GroupReport {
                    document: group.source.document.clone(),
                    line: group.source.line,
                    filter: group.filter.expression().to_owned(),
                    applicability: group.applicability.clone(),
                    members: group.members.clone(),
                })
                .collect(),
            ordinary: self.ordinary.clone(),
        }
    }

    pub fn groups(&self) -> &[Group] {
        &self.groups
    }
    pub const fn ordinary(&self) -> &BTreeSet<Test> {
        &self.ordinary
    }

    /// Membership validity is already established, so zero affected members is legitimate.
    pub fn affected<'a>(
        &'a self,
        affected: &BTreeSet<Test>,
    ) -> impl Iterator<Item = (&'a Group, BTreeSet<Test>)> {
        self.groups.iter().map(|group| {
            (
                group,
                group.members.intersection(affected).cloned().collect(),
            )
        })
    }
}

/// Actual Cargo graph and unfiltered native catalogue plus nextest's resolved profile/group context.
/// These are model inputs, not an evidence or provider-admission boundary.
pub struct Context<'a> {
    pub graph: &'a PackageGraph,
    pub tests: &'a TestListSummary,
    pub evaluation: EvalContext<'a>,
    pub known_groups: &'a KnownGroups,
    pub groups: &'a dyn GroupLookup,
}

impl Context<'_> {
    /// Compile upstream expressions against actual packages/binaries and partition all discovered tests.
    ///
    /// # Errors
    /// Incomplete catalogues and empty applicable filters fail before any affectedness selection.
    pub fn resolve(&self, requests: &[Request<'_>]) -> Result<Partition, Error> {
        Self::require_native_catalogue(self.graph, self.tests)?;
        let parse = ParseContext::new(self.graph);
        let mut groups = Vec::new();
        let mut assigned = BTreeSet::new();
        let all: BTreeSet<_> = self
            .tests
            .rust_suites
            .values()
            .flat_map(|suite| {
                suite.test_cases.iter().map(|(name, test)| Test {
                    binary: suite.binary.binary_id.clone(),
                    name: name.clone(),
                    ignored: test.ignored,
                    kind: test.kind.as_ref().map(|kind| kind.as_str().to_owned()),
                    runnable: !test.ignored
                        && test.filter_match.is_match()
                        && test.kind == Some(nextest_metadata::RustTestKind::TEST),
                })
            })
            .collect();
        for request in requests {
            let mut members = BTreeSet::new();
            if request.applicability == Applicability::Applicable {
                let kind = if matches!(self.known_groups, KnownGroups::Unavailable) {
                    FiltersetKind::OverrideFilter
                } else {
                    FiltersetKind::Test
                };
                let filter = Filterset::parse(
                    request.filter.expression().to_owned(),
                    &parse,
                    kind,
                    self.known_groups,
                )
                .map_err(|errors| {
                    if kind == FiltersetKind::OverrideFilter && errors.errors.iter().any(|error| matches!(error,
                        nextest_filtering::errors::ParseSingleError::BannedPredicate { reason: nextest_filtering::errors::BannedPredicateReason::GroupCircularDependency, .. })) {
                        Error::GroupsUnavailable { source_context: request.source.clone(), expression: request.filter.expression().to_owned() }
                    } else {
                        Error::Filter {
                            source_context: request.source.clone(),
                            expression: request.filter.expression().to_owned(),
                            detail: format!("{errors:?}"),
                        }
                    }
                })?;
                for suite in self.tests.rust_suites.values() {
                    let package_id = PackageId::new(suite.binary.package_id.clone());
                    let binary_query = BinaryQuery {
                        package_id: &package_id,
                        binary_id: &suite.binary.binary_id,
                        binary_name: &suite.binary.binary_name,
                        kind: &suite.binary.kind,
                        platform: match suite.binary.build_platform {
                            nextest_metadata::BuildPlatform::Host => {
                                guppy::graph::cargo::BuildPlatform::Host
                            }
                            nextest_metadata::BuildPlatform::Target => {
                                guppy::graph::cargo::BuildPlatform::Target
                            }
                        },
                    };
                    for (name, test) in &suite.test_cases {
                        if filter.matches_test_with_groups(
                            &TestQuery {
                                binary_query,
                                test_name: name,
                            },
                            &self.evaluation,
                            self.groups,
                        ) {
                            members.insert(Test {
                                binary: suite.binary.binary_id.clone(),
                                name: name.clone(),
                                ignored: test.ignored,
                                kind: test.kind.as_ref().map(|kind| kind.as_str().to_owned()),
                                runnable: !test.ignored
                                    && test.filter_match.is_match()
                                    && test.kind == Some(nextest_metadata::RustTestKind::TEST),
                            });
                        }
                    }
                }
                if members.is_empty() {
                    return Err(Error::Empty {
                        source_context: request.source.clone(),
                        expression: request.filter.expression().to_owned(),
                    });
                }
                assigned.extend(members.iter().cloned());
            }
            groups.push(Group {
                source: request.source.clone(),
                filter: request.filter.clone(),
                applicability: request.applicability.clone(),
                members,
            });
        }
        Ok(Partition {
            groups,
            ordinary: all.difference(&assigned).cloned().collect(),
        })
    }

    pub(crate) fn require_native_catalogue(
        graph: &PackageGraph,
        tests: &TestListSummary,
    ) -> Result<(), Error> {
        let mut count = 0;
        for (id, suite) in &tests.rust_suites {
            if id != &suite.binary.binary_id || suite.status != RustTestSuiteStatusSummary::LISTED {
                return Err(Error::Incomplete {
                    binary: id.clone(),
                    detail: format!(
                        "catalogue status {} or inconsistent binary identity",
                        suite.status.as_str()
                    ),
                });
            }
            let package_id = PackageId::new(suite.binary.package_id.clone());
            let package = graph
                .metadata(&package_id)
                .map_err(|source| Error::Package {
                    binary: id.clone(),
                    source: Box::new(source),
                })?;
            if package.name() != suite.package_name
                || RustBinaryId::from_parts(
                    package.name(),
                    &suite.binary.kind,
                    &suite.binary.binary_name,
                ) != *id
                || !package.build_targets().any(|target| {
                    target.name() == suite.binary.binary_name
                        && native_kind(&target.id()).as_ref() == Some(&suite.binary.kind)
                })
            {
                return Err(Error::Incomplete {
                    binary: id.clone(),
                    detail: "test suite does not match its Cargo package/target".into(),
                });
            }
            count += suite.test_cases.len();
        }
        if count != tests.test_count {
            return Err(Error::Count {
                declared: tests.test_count,
                actual: count,
            });
        }
        Ok(())
    }
}

const fn native_kind(target: &BuildTargetId<'_>) -> Option<RustTestBinaryKind> {
    match target {
        BuildTargetId::Library => Some(RustTestBinaryKind::LIB),
        BuildTargetId::Binary(_) => Some(RustTestBinaryKind::BIN),
        BuildTargetId::Test(_) => Some(RustTestBinaryKind::TEST),
        BuildTargetId::Example(_) => Some(RustTestBinaryKind::EXAMPLE),
        BuildTargetId::Benchmark(_) => Some(RustTestBinaryKind::BENCH),
        _ => None,
    }
}

#[derive(Debug, Display, Error)]
pub enum Error {
    /// property filter `{expression}` at `{source_context:?}` cannot resolve against current native inventory: {detail}; correct the filter/profile and retry `loom gate verify`
    Filter {
        source_context: Source,
        expression: String,
        detail: String,
    },
    /// applicable property filter `{expression}` at `{source_context:?}` matches no current tests; correct its package/binary/test expression and retry `loom gate verify`
    Empty {
        source_context: Source,
        expression: String,
    },
    /// property filter `{expression}` at `{source_context:?}` needs unavailable native nextest group configuration; resolve the actual profile before selecting affected work
    GroupsUnavailable {
        source_context: Source,
        expression: String,
    },
    /// native test catalogue for `{binary}` is incomplete: {detail}; collect the full unfiltered inventory before selecting affected work
    Incomplete {
        binary: RustBinaryId,
        detail: String,
    },
    /// native test catalogue declares {declared} tests but contains {actual}; collect the full inventory before selecting affected work
    Count { declared: usize, actual: usize },
    /// native test binary `{binary}` has no package in the current Cargo graph
    Package {
        binary: RustBinaryId,
        #[source]
        source: Box<guppy::Error>,
    },
}

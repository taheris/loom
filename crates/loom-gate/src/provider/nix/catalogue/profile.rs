//! Profile/group facts compiled by Nextest in the same sealed preparation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use nextest_filtering::{
    CompiledExpr, FiltersetLeaf, GroupLookup, KnownGroups, NameMatcher, TestQuery,
};
use nextest_metadata::{RustBinaryId, RustTestSuiteStatusSummary, TestCaseName, TestListSummary};

use super::{Error, read};

type Identity = (RustBinaryId, TestCaseName);

#[derive(Debug)]
pub(super) struct Profile {
    paths: Vec<PathBuf>,
    groups: BTreeMap<String, BTreeSet<Identity>>,
    pub(super) known: KnownGroups,
    pub(super) default: CompiledExpr,
}

impl Profile {
    pub(super) fn load(output: &Path, all: &TestListSummary) -> Result<Self, Error> {
        let names_path = output.join("groups.txt");
        let bytes = read(&names_path)?;
        let names: Vec<_> = std::str::from_utf8(&bytes)
            .map_err(|_| Error::Encoding)?
            .lines()
            .map(str::to_owned)
            .collect();
        let unique: BTreeSet<_> = names.iter().cloned().collect();
        if names.len() != unique.len()
            || !unique.contains("@global")
            || names.iter().any(String::is_empty)
        {
            return Err(invalid("invalid or incomplete native group identities"));
        }
        let default_path = output.join("default.json");
        let default_members = members(all, &serde_json::from_slice(&read(&default_path)?)?)?;
        let mut paths = vec![names_path, default_path];
        let mut groups = BTreeMap::new();
        let mut assigned = BTreeSet::new();
        for (index, name) in names.into_iter().enumerate() {
            let path = output.join(format!("group-{index}.json"));
            let group = members(all, &serde_json::from_slice(&read(&path)?)?)?;
            for identity in &group {
                if !assigned.insert(identity.clone()) {
                    return Err(invalid("native group assignments overlap"));
                }
            }
            groups.insert(name, group);
            paths.push(path);
        }
        let all_cases: BTreeSet<_> = all
            .rust_suites
            .values()
            .flat_map(|suite| {
                suite
                    .test_cases
                    .iter()
                    .filter(|(_, case)| case.filter_match.is_match())
                    .map(|(name, _)| (suite.binary.binary_id.clone(), name.clone()))
            })
            .collect();
        if assigned != all_cases {
            return Err(invalid("native group assignments omit compiled cases"));
        }
        let known = KnownGroups::Known {
            custom_groups: groups
                .keys()
                .filter(|name| name.as_str() != "@global")
                .cloned()
                .collect(),
        };
        let default = default_members
            .into_iter()
            .map(|(binary, name)| {
                let matcher = |value: String| NameMatcher::Equal {
                    value,
                    implicit: false,
                };
                CompiledExpr::Intersection(
                    Box::new(CompiledExpr::Set(FiltersetLeaf::BinaryId(
                        matcher(binary.as_str().to_owned()),
                        (0, 0).into(),
                    ))),
                    Box::new(CompiledExpr::Set(FiltersetLeaf::Test(
                        matcher(name.as_str().to_owned()),
                        (0, 0).into(),
                    ))),
                )
            })
            .fold(CompiledExpr::Set(FiltersetLeaf::None), |left, right| {
                CompiledExpr::Union(Box::new(left), Box::new(right))
            });
        Ok(Self {
            paths,
            groups,
            known,
            default,
        })
    }

    pub(super) fn bytes(&self) -> Result<Vec<Vec<u8>>, Error> {
        self.paths.iter().map(|path| read(path)).collect()
    }
}

impl GroupLookup for Profile {
    fn is_member_test(&self, test: &TestQuery<'_>, matcher: &NameMatcher) -> bool {
        let identity = (test.binary_query.binary_id.clone(), test.test_name.clone());
        self.groups
            .iter()
            .any(|(name, members)| matcher.is_match(name) && members.contains(&identity))
    }
}

fn members(all: &TestListSummary, filtered: &TestListSummary) -> Result<BTreeSet<Identity>, Error> {
    if all.rust_build_meta != filtered.rust_build_meta
        || !all.rust_suites.keys().eq(filtered.rust_suites.keys())
    {
        return Err(invalid("profile query changed native compilation context"));
    }
    let mut members = BTreeSet::new();
    for (id, suite) in &filtered.rust_suites {
        let original = &all.rust_suites[id];
        if suite.binary != original.binary || suite.package_name != original.package_name {
            return Err(invalid("profile query changed native binary identity"));
        }
        if suite.status == RustTestSuiteStatusSummary::SKIPPED
            || suite.status == RustTestSuiteStatusSummary::SKIPPED_DEFAULT_FILTER
        {
            if !suite.test_cases.is_empty() {
                return Err(invalid("skipped binary carries unverified profile cases"));
            }
            continue;
        }
        if suite.status != RustTestSuiteStatusSummary::LISTED
            || !suite.test_cases.keys().eq(original.test_cases.keys())
        {
            return Err(invalid("profile query has an incomplete case catalogue"));
        }
        for (name, case) in &suite.test_cases {
            if case.ignored != original.test_cases[name].ignored
                || case.kind != original.test_cases[name].kind
            {
                return Err(invalid("profile query changed ignored-case identity"));
            }
            if case.filter_match.is_match() {
                members.insert((id.clone(), name.clone()));
            }
        }
    }
    Ok(members)
}

fn invalid(detail: &str) -> Error {
    Error::Catalogue {
        detail: detail.into(),
    }
}

//! One combined Markdown ownership budget per indexed package.

use std::path::Path;

use loom_driver::spec::package::discover_indexed;

use super::util::{verdict_from, workspace_root};
use super::{Verdict, WalkInput};

pub fn run(_input: &WalkInput) -> Verdict {
    check(&workspace_root())
}

fn check(root: &Path) -> Verdict {
    let packages = match discover_indexed(root) {
        Ok(packages) => packages,
        Err(error) => {
            return Verdict {
                pass: false,
                evidence: format!("{error:#}"),
            };
        }
    };
    let mut violations = Vec::new();
    for package in packages {
        match package.line_count() {
            Ok(count) if count > 2_000 => violations.push(format!(
                "{}: spec package `{}` has {count} combined Markdown lines, maximum 2000",
                package.contract().display(),
                package.label(),
            )),
            Ok(_) => {}
            Err(error) => violations.push(format!("{error:#}")),
        }
    }
    verdict_from("spec package combined 2000-line ceiling", violations)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("specs/alpha")).unwrap();
        std::fs::create_dir(dir.path().join("docs")).unwrap();
        std::fs::write(
            dir.path().join("docs/README.md"),
            "- [alpha](../specs/alpha/spec.md)\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn combined_package_budget_rejects_two_individually_small_documents() {
        let dir = fixture();
        std::fs::write(
            dir.path().join("specs/alpha/spec.md"),
            "contract\n".repeat(1_000),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("specs/alpha/tests.md"),
            "acceptance\n".repeat(1_000),
        )
        .unwrap();
        assert!(check(dir.path()).pass);
        std::fs::write(
            dir.path().join("specs/alpha/tests.md"),
            "acceptance\n".repeat(1_001),
        )
        .unwrap();
        let verdict = check(dir.path());
        assert!(!verdict.pass);
        assert!(verdict.evidence.contains("2001 combined"));
    }

    #[test]
    fn size_walk_fails_incomplete_or_unindexed_packages() {
        let dir = fixture();
        assert!(!check(dir.path()).pass);
        std::fs::write(dir.path().join("specs/alpha/spec.md"), "# Contract").unwrap();
        std::fs::write(dir.path().join("specs/alpha/tests.md"), "# Acceptance").unwrap();
        assert!(check(dir.path()).pass);
        std::fs::write(
            dir.path().join("docs/README.md"),
            "- [beta](../specs/beta/spec.md)",
        )
        .unwrap();
        assert!(!check(dir.path()).pass);
    }

    #[test]
    fn workspace_packages_fit_budget() {
        assert!(check(&workspace_root()).pass);
    }
}

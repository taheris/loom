use std::path::PathBuf;

use displaydoc::Display;
use loom_events::identifier::ProfileName;
use thiserror::Error;

use crate::document::{DocumentError, FrontmatterError, RawSkillDocument, SkillDocument};
use crate::identity::SkillName;
use crate::registry::{NamedSkill, SkillSet};
use crate::source::SkillProvenance;

const BASE_PROFILE: &str = "base";
const RUST_PROFILE: &str = "rust";

/// Built-in profile bundle shipped with Loom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bundle {
    Base,
    Rust,
}

impl Bundle {
    pub fn profile_name(self) -> ProfileName {
        match self {
            Self::Base => ProfileName::new(BASE_PROFILE),
            Self::Rust => ProfileName::new(RUST_PROFILE),
        }
    }
}

/// Built-in skill package embedded in the Loom release.
#[derive(Debug, Clone, Copy)]
pub struct Package {
    pub bundle: Bundle,
    pub name: &'static str,
    pub relative_path: &'static str,
    pub markdown: &'static str,
}

/// Release-contract failures in the embedded built-in catalog.
#[derive(Debug, Display, Error)]
pub enum CatalogError {
    /// built-in skill `{name}` has invalid declared name
    InvalidDeclaredName {
        name: String,
        #[source]
        source: crate::identity::ParseSkillNameError,
    },
    /// built-in skill `{name}` has invalid markdown frontmatter
    Document {
        name: String,
        #[source]
        source: DocumentError,
    },
    /// built-in skill `{name}` is missing required identity fields
    Frontmatter {
        name: String,
        #[source]
        source: FrontmatterError,
    },
    /// built-in skill `{declared}` frontmatter name does not match package name `{expected}`
    NameMismatch {
        declared: SkillName,
        expected: SkillName,
    },
}

pub const PACKAGES: &[Package] = &[
    Package {
        bundle: Bundle::Base,
        name: "loom-context-before-edit",
        relative_path: "builtin/base/loom-context-before-edit/skill.md",
        markdown: include_str!("../builtin/base/loom-context-before-edit/skill.md"),
    },
    Package {
        bundle: Bundle::Base,
        name: "loom-workspace-discipline",
        relative_path: "builtin/base/loom-workspace-discipline/skill.md",
        markdown: include_str!("../builtin/base/loom-workspace-discipline/skill.md"),
    },
    Package {
        bundle: Bundle::Base,
        name: "loom-scope-discipline",
        relative_path: "builtin/base/loom-scope-discipline/skill.md",
        markdown: include_str!("../builtin/base/loom-scope-discipline/skill.md"),
    },
    Package {
        bundle: Bundle::Base,
        name: "loom-todo-decomposition",
        relative_path: "builtin/base/loom-todo-decomposition/skill.md",
        markdown: include_str!("../builtin/base/loom-todo-decomposition/skill.md"),
    },
    Package {
        bundle: Bundle::Base,
        name: "loom-verify-after-edit",
        relative_path: "builtin/base/loom-verify-after-edit/skill.md",
        markdown: include_str!("../builtin/base/loom-verify-after-edit/skill.md"),
    },
    Package {
        bundle: Bundle::Base,
        name: "loom-review-finding-recall",
        relative_path: "builtin/base/loom-review-finding-recall/skill.md",
        markdown: include_str!("../builtin/base/loom-review-finding-recall/skill.md"),
    },
    Package {
        bundle: Bundle::Base,
        name: "loom-inbox-resolution",
        relative_path: "builtin/base/loom-inbox-resolution/skill.md",
        markdown: include_str!("../builtin/base/loom-inbox-resolution/skill.md"),
    },
    Package {
        bundle: Bundle::Base,
        name: "loom-tune-proposal-handoff",
        relative_path: "builtin/base/loom-tune-proposal-handoff/skill.md",
        markdown: include_str!("../builtin/base/loom-tune-proposal-handoff/skill.md"),
    },
    Package {
        bundle: Bundle::Base,
        name: "loom-final-reporting",
        relative_path: "builtin/base/loom-final-reporting/skill.md",
        markdown: include_str!("../builtin/base/loom-final-reporting/skill.md"),
    },
    Package {
        bundle: Bundle::Rust,
        name: "loom-rust-change-planning",
        relative_path: "builtin/rust/loom-rust-change-planning/skill.md",
        markdown: include_str!("../builtin/rust/loom-rust-change-planning/skill.md"),
    },
    Package {
        bundle: Bundle::Rust,
        name: "loom-rust-verification",
        relative_path: "builtin/rust/loom-rust-verification/skill.md",
        markdown: include_str!("../builtin/rust/loom-rust-verification/skill.md"),
    },
    Package {
        bundle: Bundle::Rust,
        name: "loom-rust-review",
        relative_path: "builtin/rust/loom-rust-review/skill.md",
        markdown: include_str!("../builtin/rust/loom-rust-review/skill.md"),
    },
    Package {
        bundle: Bundle::Rust,
        name: "loom-rust-style-rules",
        relative_path: "builtin/rust/loom-rust-style-rules/skill.md",
        markdown: include_str!("../builtin/rust/loom-rust-style-rules/skill.md"),
    },
];

pub fn catalog() -> Result<SkillSet, CatalogError> {
    let mut set = SkillSet::default();
    for package in PACKAGES {
        set.push(parse_package(package)?);
    }
    Ok(set)
}

fn parse_package(package: &Package) -> Result<NamedSkill, CatalogError> {
    let expected_name: SkillName =
        package
            .name
            .parse()
            .map_err(|source| CatalogError::InvalidDeclaredName {
                name: package.name.to_string(),
                source,
            })?;
    let provenance = SkillProvenance::built_in(
        package.bundle.profile_name(),
        expected_name.clone(),
        source_path(package.relative_path),
        package.markdown,
    );
    let document = SkillDocument::parse(RawSkillDocument::new(package.markdown, provenance))
        .map_err(|source| CatalogError::Document {
            name: package.name.to_string(),
            source,
        })?;
    let skill =
        NamedSkill::from_document(document).map_err(|source| CatalogError::Frontmatter {
            name: package.name.to_string(),
            source,
        })?;
    if skill.name() != &expected_name {
        return Err(CatalogError::NameMismatch {
            declared: skill.name().clone(),
            expected: expected_name,
        });
    }
    Ok(skill)
}

fn source_path(relative_path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SkillSource;

    #[test]
    fn built_in_catalog_contains_v1_names() {
        let set = catalog().expect("catalog parses");
        let names = set
            .skills()
            .iter()
            .map(|skill| skill.name().as_str())
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 13);
        assert!(names.contains(&"loom-context-before-edit"));
        assert!(names.contains(&"loom-rust-style-rules"));
        assert!(
            set.skills()
                .iter()
                .all(|skill| skill.source() == SkillSource::BuiltIn)
        );
    }
}

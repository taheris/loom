use std::collections::BTreeSet;
use std::path::PathBuf;

use displaydoc::Display;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::disclosure::DisclosureMode;
use crate::identity::{SkillDescription, SkillName};
use crate::source::SkillSource;

/// Skill with required identity fields parsed and source classified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedSkill {
    pub name: SkillName,
    pub description: SkillDescription,
    pub source: SkillSource,
}

/// Loaded candidates before duplicate resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSet {
    skills: Vec<NamedSkill>,
}

impl SkillSet {
    pub fn new(skills: Vec<NamedSkill>) -> Self {
        Self { skills }
    }

    pub fn skills(&self) -> &[NamedSkill] {
        &self.skills
    }
}

/// Effective registry after duplicate and override resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillRegistry {
    skills: Vec<NamedSkill>,
}

impl SkillRegistry {
    pub fn new(skills: Vec<NamedSkill>) -> Result<Self, RegistryError> {
        let mut seen = BTreeSet::new();
        for skill in &skills {
            if !seen.insert(skill.name.clone()) {
                return Err(RegistryError::DuplicateName {
                    name: skill.name.clone(),
                });
            }
        }
        Ok(Self { skills })
    }

    pub fn skills(&self) -> &[NamedSkill] {
        &self.skills
    }
}

/// Registry narrowed to the current phase/profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicableRegistry {
    skills: Vec<NamedSkill>,
}

impl ApplicableRegistry {
    pub fn from_registry(registry: SkillRegistry) -> Self {
        Self {
            skills: registry.skills,
        }
    }

    pub fn skills(&self) -> &[NamedSkill] {
        &self.skills
    }
}

/// Materialized skill file ready for prompt or native registration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializedSkill {
    pub name: SkillName,
    pub description: SkillDescription,
    pub source: SkillSource,
    pub path: PathBuf,
}

/// Registry whose built-ins have been copied to readable paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializedRegistry {
    skills: Vec<MaterializedSkill>,
}

impl MaterializedRegistry {
    pub fn new(skills: Vec<MaterializedSkill>) -> Self {
        Self { skills }
    }

    pub fn skills(&self) -> &[MaterializedSkill] {
        &self.skills
    }
}

/// Skill set and disclosure mode passed into backend setup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisteredSkills {
    registry: MaterializedRegistry,
    disclosure: DisclosureMode,
}

impl RegisteredSkills {
    pub fn new(registry: MaterializedRegistry, disclosure: DisclosureMode) -> Self {
        Self {
            registry,
            disclosure,
        }
    }

    pub fn registry(&self) -> &MaterializedRegistry {
        &self.registry
    }

    pub fn disclosure(&self) -> DisclosureMode {
        self.disclosure
    }
}

/// Registry construction failures.
#[derive(Debug, Clone, PartialEq, Eq, Display, Error)]
pub enum RegistryError {
    /// duplicate skill name `{name}`
    DuplicateName { name: SkillName },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> NamedSkill {
        NamedSkill {
            name: SkillName::new(name).expect("valid name"),
            description: SkillDescription::new("Use when testing registry behavior.")
                .expect("valid description"),
            source: SkillSource::Workspace,
        }
    }

    #[test]
    fn registry_rejects_duplicate_names() {
        let err = SkillRegistry::new(vec![named("rust-review"), named("rust-review")])
            .expect_err("duplicate rejected");
        assert_eq!(
            err,
            RegistryError::DuplicateName {
                name: SkillName::new("rust-review").expect("valid name"),
            },
        );
    }

    #[test]
    fn registry_preserves_unique_skills() {
        let registry = SkillRegistry::new(vec![named("rust-review"), named("verify-after-edit")])
            .expect("unique skills accepted");
        assert_eq!(registry.skills().len(), 2);
    }
}

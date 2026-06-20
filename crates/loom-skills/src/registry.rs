use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use displaydoc::Display;
use loom_events::identifier::ProfileName;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::disclosure::{DisclosureMode, PathDisplay};
use crate::document::{FrontmatterError, LoomMetadata, SkillDocument, SkillFrontmatter};
use crate::identity::{PhaseName, SkillDescription, SkillName};
use crate::source::{SkillProvenance, SkillSource};

/// Skill with required identity fields parsed and source classified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedSkill {
    document: SkillDocument,
    frontmatter: SkillFrontmatter,
}

impl NamedSkill {
    pub fn from_document(document: SkillDocument) -> Result<Self, FrontmatterError> {
        let frontmatter = document.typed_frontmatter()?;
        Ok(Self {
            document,
            frontmatter,
        })
    }

    pub fn name(&self) -> &SkillName {
        &self.frontmatter.name
    }

    pub fn description(&self) -> &SkillDescription {
        &self.frontmatter.description
    }

    pub fn frontmatter(&self) -> &SkillFrontmatter {
        &self.frontmatter
    }

    pub fn document(&self) -> &SkillDocument {
        &self.document
    }

    pub fn provenance(&self) -> &SkillProvenance {
        self.document.provenance()
    }

    pub fn source(&self) -> SkillSource {
        self.provenance().source
    }

    pub fn applies_to(&self, phase: &PhaseName, profile: &ProfileName) -> bool {
        let Some(metadata) = self
            .frontmatter
            .metadata
            .as_ref()
            .and_then(|m| m.loom.as_ref())
        else {
            return true;
        };
        metadata_matches(metadata, phase, profile)
    }
}

/// Loaded candidates before duplicate and override resolution.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSet {
    skills: Vec<NamedSkill>,
}

impl SkillSet {
    pub fn new(skills: Vec<NamedSkill>) -> Self {
        Self { skills }
    }

    pub fn push(&mut self, skill: NamedSkill) {
        self.skills.push(skill);
    }

    pub fn extend(&mut self, other: Self) {
        self.skills.extend(other.skills);
    }

    pub fn skills(&self) -> &[NamedSkill] {
        &self.skills
    }

    pub fn into_skills(self) -> Vec<NamedSkill> {
        self.skills
    }
}

/// Effective registry after duplicate and override resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillRegistry {
    skills: Vec<NamedSkill>,
}

impl SkillRegistry {
    pub fn new(skills: Vec<NamedSkill>) -> Result<Self, RegistryError> {
        Self::from_set(SkillSet::new(skills))
    }

    pub fn from_set(set: SkillSet) -> Result<Self, RegistryError> {
        let skills = set.into_skills();
        let built_in_names = skills
            .iter()
            .filter(|skill| skill.source() == SkillSource::BuiltIn)
            .map(|skill| skill.name().clone())
            .collect::<Vec<_>>();
        let mut built_ins = BTreeMap::new();
        let mut overrides = BTreeMap::new();
        let mut workspace = BTreeMap::new();

        for skill in skills {
            match skill.source() {
                SkillSource::BuiltIn => insert_unique(&mut built_ins, skill, |name| {
                    RegistryError::DuplicateBuiltInName { name }
                })?,
                SkillSource::Override => {
                    if !built_in_names.iter().any(|name| name == skill.name()) {
                        return Err(RegistryError::UnknownBuiltInOverride {
                            name: skill.name().clone(),
                        });
                    }
                    insert_unique(&mut overrides, skill, |name| {
                        RegistryError::DuplicateOverride { name }
                    })?;
                }
                SkillSource::Workspace | SkillSource::Configured => {
                    if built_in_names.iter().any(|name| name == skill.name()) {
                        return Err(RegistryError::DuplicateName {
                            name: skill.name().clone(),
                        });
                    }
                    insert_unique(&mut workspace, skill, |name| RegistryError::DuplicateName {
                        name,
                    })?;
                }
            }
        }

        let mut resolved = Vec::new();
        for (name, built_in) in built_ins {
            if let Some(skill_override) = overrides.remove(&name) {
                resolved.push(skill_override);
            } else {
                resolved.push(built_in);
            }
        }
        resolved.extend(workspace.into_values());
        Ok(Self { skills: resolved })
    }

    pub fn skills(&self) -> &[NamedSkill] {
        &self.skills
    }

    pub fn into_skills(self) -> Vec<NamedSkill> {
        self.skills
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
            skills: registry.into_skills(),
        }
    }

    pub fn filter(registry: SkillRegistry, phase: &PhaseName, profile: &ProfileName) -> Self {
        let skills = registry
            .into_skills()
            .into_iter()
            .filter(|skill| skill.applies_to(phase, profile))
            .collect();
        Self { skills }
    }

    pub fn skills(&self) -> &[NamedSkill] {
        &self.skills
    }

    pub fn into_skills(self) -> Vec<NamedSkill> {
        self.skills
    }
}

/// Materialized skill file ready for prompt or native registration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializedSkill {
    pub name: SkillName,
    pub description: SkillDescription,
    pub source: SkillSource,
    pub provenance: SkillProvenance,
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

    pub fn materialize(
        registry: ApplicableRegistry,
        scratch_dir: impl AsRef<Path>,
    ) -> Result<Self, MaterializeError> {
        let scratch_dir = scratch_dir.as_ref();
        let mut skills = Vec::new();
        for skill in registry.into_skills() {
            let path = if skill.source() == SkillSource::BuiltIn {
                materialize_built_in(&skill, scratch_dir)?
            } else {
                skill.provenance().document_path.clone()
            };
            skills.push(MaterializedSkill {
                name: skill.name().clone(),
                description: skill.description().clone(),
                source: skill.source(),
                provenance: skill.provenance().clone(),
                path,
            });
        }
        Ok(Self { skills })
    }

    pub fn skills(&self) -> &[MaterializedSkill] {
        &self.skills
    }

    pub fn disclose(&self, mode: DisclosureMode, path_display: PathDisplay) -> DisclosureRegistry {
        let include_paths = mode == DisclosureMode::Prompt || path_display == PathDisplay::Always;
        let skills = self
            .skills
            .iter()
            .map(|skill| DisclosureSkill {
                name: skill.name.clone(),
                description: skill.description.clone(),
                path: include_paths.then(|| skill.path.clone()),
            })
            .collect();
        DisclosureRegistry {
            mode,
            path_display,
            skills,
        }
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

/// Compact disclosure entry rendered into prompt skill indexes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisclosureSkill {
    pub name: SkillName,
    pub description: SkillDescription,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

/// Disclosure-ready skill index independent of backend registrar mechanics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisclosureRegistry {
    pub mode: DisclosureMode,
    pub path_display: PathDisplay,
    pub skills: Vec<DisclosureSkill>,
}

/// Registry construction failures.
#[derive(Debug, Display, Error)]
pub enum RegistryError {
    /// duplicate skill name `{name}`
    DuplicateName { name: SkillName },
    /// duplicate built-in skill name `{name}`
    DuplicateBuiltInName { name: SkillName },
    /// duplicate override for built-in skill `{name}`
    DuplicateOverride { name: SkillName },
    /// override names unknown built-in skill `{name}`
    UnknownBuiltInOverride { name: SkillName },
}

/// Materialization failures.
#[derive(Debug, Display, Error)]
pub enum MaterializeError {
    /// failed to create skill materialization directory `{path}`
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// failed to write materialized skill `{path}`
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn insert_unique<F>(
    map: &mut BTreeMap<SkillName, NamedSkill>,
    skill: NamedSkill,
    error: F,
) -> Result<(), RegistryError>
where
    F: FnOnce(SkillName) -> RegistryError,
{
    let name = skill.name().clone();
    if map.insert(name.clone(), skill).is_some() {
        return Err(error(name));
    }
    Ok(())
}

fn metadata_matches(metadata: &LoomMetadata, phase: &PhaseName, profile: &ProfileName) -> bool {
    let phase_matches = metadata.phases.is_empty()
        || metadata
            .phases
            .iter()
            .any(|filter_phase| filter_phase.matches(phase));
    let profile_matches = metadata.profiles.is_empty()
        || metadata
            .profiles
            .iter()
            .any(|filter_profile| filter_profile == profile);
    phase_matches && profile_matches
}

fn materialize_built_in(
    skill: &NamedSkill,
    scratch_dir: &Path,
) -> Result<PathBuf, MaterializeError> {
    let skill_dir = scratch_dir.join("skills").join(skill.name().as_str());
    fs::create_dir_all(&skill_dir).map_err(|source| MaterializeError::CreateDir {
        path: skill_dir.clone(),
        source,
    })?;
    let path = skill_dir.join("skill.md");
    fs::write(&path, skill.document().markdown()).map_err(|source| MaterializeError::Write {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::RawSkillDocument;
    use crate::source::SourceShape;

    fn provenance(source: SkillSource, path: &str, markdown: &str) -> SkillProvenance {
        SkillProvenance {
            source,
            shape: SourceShape::LooseFile,
            document_path: PathBuf::from(path),
            base_dir: PathBuf::new(),
            tuning_path: None,
            built_in_bundle: None,
            built_in_name: None,
            source_hash: blake3::hash(markdown.as_bytes()).to_hex().to_string(),
        }
    }

    fn named(name: &str, source: SkillSource) -> NamedSkill {
        let markdown = format!(
            "---\nname: {name}\ndescription: Use when testing registry behavior.\n---\nBody\n"
        );
        let document = SkillDocument::parse(RawSkillDocument::new(
            markdown.clone(),
            provenance(source, &format!("{name}.md"), &markdown),
        ))
        .expect("document parses");
        NamedSkill::from_document(document).expect("valid named skill")
    }

    #[test]
    fn registry_rejects_duplicate_names() {
        let err = SkillRegistry::new(vec![
            named("rust-review", SkillSource::Workspace),
            named("rust-review", SkillSource::Configured),
        ])
        .expect_err("duplicate rejected");
        assert!(
            matches!(err, RegistryError::DuplicateName { name } if name.as_str() == "rust-review")
        );
    }

    #[test]
    fn registry_preserves_unique_skills() {
        let registry = SkillRegistry::new(vec![
            named("rust-review", SkillSource::Workspace),
            named("verify-after-edit", SkillSource::Configured),
        ])
        .expect("unique skills accepted");
        assert_eq!(registry.skills().len(), 2);
    }
}

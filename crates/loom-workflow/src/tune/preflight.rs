use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use loom_skill::disclosure::{DisclosureMode, PathDisplay};
use loom_skill::document::{RawSkillDocument, SkillDocument};
use loom_skill::registry::{
    ApplicableRegistry, MaterializedRegistry, NamedSkill, SkillRegistry, SkillSet,
};
use loom_skill::source::{SkillProvenance, SkillSource};
use loom_tune::checker::Implementation;

use super::{Budget, Target, TuneError, ValidationInput, ValidationRow, ValidationStatus};

pub(super) async fn run(input: &ValidationInput<'_>, budget: &Budget<'_>) -> Vec<ValidationRow> {
    let mut rows = Vec::new();
    for checker in &input.plan.preflight_checkers {
        let result = budget
            .run(async {
                let metadata = input.registry.require_active(checker)?;
                match metadata.implementation {
                    Implementation::SkillRegistry => {
                        candidate_registry(input)?;
                        Ok(Vec::new())
                    }
                    Implementation::SkillMaterialization => {
                        materialize(input)?;
                        Ok(Vec::new())
                    }
                    Implementation::SkillProtocolBoundary => {
                        let artifacts =
                            super::target_artifacts(input.context, input.targets, input.repo)?;
                        Ok(vec![super::validate_skill_protocol_boundary(&artifacts)])
                    }
                    Implementation::TemplateCompile | Implementation::TemplateConformance => Ok(
                        super::validate_templates(input.repo, budget, metadata.implementation)
                            .await,
                    ),
                    Implementation::TuneCaseValidation => {
                        validate_cases(input)?;
                        Ok(Vec::new())
                    }
                    _ => Err(invalid(format!("{checker} has no preflight executor"))),
                }
            })
            .await;
        match result {
            Ok(details) => {
                let passed = details
                    .iter()
                    .all(|row| row.status == ValidationStatus::Passed);
                if !details.iter().any(|row| row.check == checker.as_str()) {
                    rows.push(ValidationRow {
                        check: checker.to_string(),
                        status: if passed {
                            ValidationStatus::Passed
                        } else {
                            ValidationStatus::Failed
                        },
                        detail: if passed {
                            "candidate preflight passed"
                        } else {
                            "candidate preflight failed; see command results"
                        }
                        .to_owned(),
                    });
                }
                rows.extend(details);
            }
            Err(source) => rows.push(ValidationRow {
                check: checker.to_string(),
                status: ValidationStatus::Failed,
                detail: error_detail(&source),
            }),
        }
    }
    rows
}

fn candidate_registry(input: &ValidationInput<'_>) -> Result<SkillRegistry, TuneError> {
    let mut builtins = Vec::new();
    for skill in loom_skill::builtin::catalog()?.into_skills() {
        if let Ok(relative) = skill
            .provenance()
            .document_path
            .strip_prefix(&input.context.workspace)
        {
            builtins.push(read_skill(
                &input.repo.join(relative),
                SkillSource::BuiltIn,
            )?);
        } else {
            builtins.push(skill);
        }
    }
    let configured = input
        .context
        .loom_config
        .skills
        .paths
        .iter()
        .map(|path| {
            if path.is_absolute() {
                path.strip_prefix(&input.context.workspace)
                    .map(|relative| input.repo.join(relative))
                    .map_err(|_| {
                        invalid(format!(
                            "configured skill path {} is outside the candidate workspace",
                            path.display()
                        ))
                    })
            } else {
                Ok(path.clone())
            }
        })
        .collect::<Result<Vec<_>, TuneError>>()?;
    let tracked = candidate_tracked_files(input)?;
    let report = loom_skill::discovery::load_workspace(
        input.repo,
        &tracked.into_iter().collect::<Vec<_>>(),
        &configured,
    )?;
    let mut set = SkillSet::new(builtins);
    set.extend(report.into_set());
    let registry = SkillRegistry::from_set(set)?;
    for target in input.targets {
        let Target::Skill { name } = target else {
            continue;
        };
        let path = input.context.candidate_path(target, input.repo)?;
        require_inside(input.repo, &path)?;
        let actual = read_skill(&path, SkillSource::Workspace)?;
        if actual.name() != name
            || !registry.skills().iter().any(|skill| {
                skill.name() == name && skill.document().markdown() == actual.document().markdown()
            })
        {
            return Err(invalid(format!(
                "candidate {target} is missing, renamed, or not the skill selected by the candidate registry"
            )));
        }
    }
    Ok(registry)
}

fn materialize(input: &ValidationInput<'_>) -> Result<(), TuneError> {
    let registry = candidate_registry(input)?;
    let scratch = tempfile::tempdir().map_err(TuneError::ReplayWorkspace)?;
    for skill in registry.skills() {
        let metadata = skill
            .frontmatter()
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.loom.as_ref());
        let phase =
            metadata
                .and_then(|metadata| metadata.phases.first())
                .cloned()
                .unwrap_or("loop".parse().map_err(|source| {
                    invalid(format!("invalid materialization phase: {source}"))
                })?);
        let profile = metadata
            .and_then(|metadata| metadata.profiles.first())
            .cloned()
            .unwrap_or_else(loom_driver::identifier::ProfileName::base);
        let applicable = ApplicableRegistry::filter(registry.clone(), &phase, &profile);
        let materialized = MaterializedRegistry::materialize(applicable, scratch.path())
            .map_err(|source| invalid(error_detail(&source)))?;
        for skill in materialized.skills() {
            if skill.source() == SkillSource::BuiltIn {
                require_inside(scratch.path(), skill.path())?;
            } else {
                require_inside(input.repo, skill.path())?;
            }
            super::read_to_string(skill.path())?;
        }
        let disclosed = materialized.disclose(DisclosureMode::Prompt, PathDisplay::Always);
        if disclosed.skills.iter().any(|skill| skill.path.is_none()) {
            return Err(invalid(
                "candidate prompt disclosure omitted a materialized path",
            ));
        }
    }
    Ok(())
}

fn validate_cases(input: &ValidationInput<'_>) -> Result<(), TuneError> {
    let registry = candidate_registry(input)?;
    let skills = registry
        .skills()
        .iter()
        .map(super::skill_entry)
        .collect::<Vec<_>>();
    let loaded = super::load_tuning_cases(
        input.repo,
        &candidate_tracked_files(input)?,
        &skills,
        &input.context.target_catalog,
        input.targets,
        input.registry,
        &input.context.disabled_checkers,
    )?;
    let before = input.loaded_cases.cases();
    let after = loaded.cases.cases();
    if before.len() != after.len()
        || before.iter().zip(after).any(|(before, after)| {
            before.id != after.id
                || before.checker != after.checker
                || before.targets != after.targets
                || before.role != after.role
                || before.input != after.input
                || before.expected != after.expected
        })
    {
        return Err(invalid("candidate changed the frozen behavioral cases"));
    }
    Ok(())
}

fn candidate_tracked_files(input: &ValidationInput<'_>) -> Result<BTreeSet<PathBuf>, TuneError> {
    let mut tracked = input.context.tracked_files.clone();
    for path in input.touched {
        let relative = path
            .strip_prefix(input.repo)
            .map_err(|_| invalid("candidate target escapes proposal checkout"))?;
        tracked.insert(relative.to_owned());
    }
    Ok(tracked)
}

fn read_skill(path: &Path, source: SkillSource) -> Result<NamedSkill, TuneError> {
    let markdown = super::read_to_string(path)?;
    let provenance = SkillProvenance::package(source, path, None, &markdown);
    let document = SkillDocument::parse(RawSkillDocument::new(markdown, provenance))
        .map_err(|source| invalid(error_detail(&source)))?;
    NamedSkill::from_document(document).map_err(|source| invalid(error_detail(&source)))
}

fn require_inside(root: &Path, path: &Path) -> Result<(), TuneError> {
    let root = std::fs::canonicalize(root).map_err(TuneError::ReplayWorkspace)?;
    let canonical = std::fs::canonicalize(path).map_err(|source| TuneError::ReadFile {
        path: path.to_owned(),
        source,
    })?;
    if !canonical.starts_with(root) || !canonical.is_file() {
        return Err(invalid(format!(
            "materialized candidate path {} is not a file inside its checkout",
            path.display()
        )));
    }
    Ok(())
}

fn invalid(detail: impl Into<String>) -> TuneError {
    TuneError::CandidatePreflight {
        detail: detail.into(),
    }
}

pub(super) fn error_detail(error: &dyn std::error::Error) -> String {
    let mut detail = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        detail.push_str(": ");
        detail.push_str(&error.to_string());
        source = error.source();
    }
    detail
}

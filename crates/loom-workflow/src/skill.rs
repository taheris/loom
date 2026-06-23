use std::path::{Path, PathBuf};

use displaydoc::Display;
use loom_driver::agent::AgentRuntime;
use loom_driver::config::{SkillPathDisplay, SkillRegistration, SkillsConfig};
use loom_driver::git::{GitClient, GitError};
use loom_driver::identifier::ProfileName;
use loom_skills::builtin::CatalogError;
use loom_skills::disclosure::{
    DisclosureMode, NativeRegistration, PathDisplay, RegistrationPolicy,
};
use loom_skills::discovery::{DiscoveryError, load_workspace};
use loom_skills::identity::{ParsePhaseNameError, PhaseName};
use loom_skills::registry::{
    ApplicableRegistry, DisclosureRegistry, MaterializeError, MaterializedRegistry,
    RegisteredSkills, RegistryError, SkillRegistry,
};
use loom_templates::SkillIndexMarkdown;
use thiserror::Error;
use tracing::warn;

/// Errors raised while resolving skills for one agent session.
#[derive(Debug, Display, Error)]
pub enum SkillError {
    /// built-in skill catalog is invalid
    Catalog(#[from] CatalogError),
    /// skill discovery failed
    Discovery(#[from] DiscoveryError),
    /// skill registry resolution failed
    Registry(#[from] RegistryError),
    /// skill materialization failed
    Materialize(#[from] MaterializeError),
    /// git tracked-file discovery failed while resolving skills
    Git(#[from] GitError),
    /// phase name for skill filtering is invalid
    Phase(#[from] ParsePhaseNameError),
}

/// Applicable skill set plus disclosure policy for one phase/profile/backend.
#[derive(Debug, Clone)]
pub struct SkillPlan {
    applicable: ApplicableRegistry,
    disclosure_mode: DisclosureMode,
    path_display: PathDisplay,
}

impl SkillPlan {
    pub fn resolve_from_workspace_sync(
        workspace: &Path,
        phase: &str,
        profile: &ProfileName,
        runtime: AgentRuntime,
        config: &SkillsConfig,
    ) -> Result<Self, SkillError> {
        let tracked_files = if workspace.join(".git").exists() {
            let git = GitClient::open(workspace)?;
            git.tracked_files_sync()?
        } else {
            Vec::new()
        };
        Self::resolve(workspace, &tracked_files, phase, profile, runtime, config)
    }

    pub async fn resolve_from_workspace(
        workspace: &Path,
        phase: &str,
        profile: &ProfileName,
        runtime: AgentRuntime,
        config: &SkillsConfig,
    ) -> Result<Self, SkillError> {
        let tracked_files = if workspace.join(".git").exists() {
            let git = GitClient::open(workspace)?;
            git.tracked_files().await?
        } else {
            Vec::new()
        };
        Self::resolve(workspace, &tracked_files, phase, profile, runtime, config)
    }

    pub fn resolve(
        workspace: &Path,
        tracked_files: &[PathBuf],
        phase: &str,
        profile: &ProfileName,
        runtime: AgentRuntime,
        config: &SkillsConfig,
    ) -> Result<Self, SkillError> {
        let mut set = loom_skills::builtin::catalog()?;
        let report = load_workspace(workspace, tracked_files, &config.paths)?;
        for warning in report.warnings() {
            warn!(
                source = ?warning.source,
                path = %warning.path.display(),
                kind = ?warning.kind,
                message = %warning.message,
                "workspace skill skipped during discovery",
            );
        }
        set.extend(report.into_set());
        let registry = SkillRegistry::from_set(set)?;
        let phase_name = PhaseName::new(phase)?;
        Ok(Self {
            applicable: ApplicableRegistry::filter(registry, &phase_name, profile),
            disclosure_mode: disclosure_mode_for(runtime, config.registration),
            path_display: path_display(config.show_paths),
        })
    }

    pub fn materialize(
        &self,
        scratch_dir: &Path,
        workspace: &Path,
    ) -> Result<SkillSession, SkillError> {
        let registry = MaterializedRegistry::materialize(self.applicable.clone(), scratch_dir)?;
        let disclosure = registry.disclose(self.disclosure_mode, self.path_display);
        let skill_index = render_skill_index(&disclosure, workspace);
        let registered = RegisteredSkills::new(registry, self.disclosure_mode);
        Ok(SkillSession {
            skill_index,
            registered,
        })
    }
}

/// Materialized skills and pre-rendered index for a session.
#[derive(Debug, Clone)]
pub struct SkillSession {
    pub skill_index: SkillIndexMarkdown,
    pub registered: RegisteredSkills,
}

pub fn disclosure_mode_for(
    runtime: AgentRuntime,
    registration: SkillRegistration,
) -> DisclosureMode {
    if matches!(runtime, AgentRuntime::Direct) {
        return DisclosureMode::Prompt;
    }
    registration_policy(registration).disclosure_mode(native_registration(runtime))
}

pub fn disclosure_mode_from_policy(
    registration: SkillRegistration,
    native: NativeRegistration,
) -> DisclosureMode {
    registration_policy(registration).disclosure_mode(native)
}

fn native_registration(_runtime: AgentRuntime) -> NativeRegistration {
    NativeRegistration::Unsupported
}

fn registration_policy(registration: SkillRegistration) -> RegistrationPolicy {
    match registration {
        SkillRegistration::Auto => RegistrationPolicy::Auto,
        SkillRegistration::Prompt => RegistrationPolicy::Prompt,
    }
}

fn path_display(display: SkillPathDisplay) -> PathDisplay {
    match display {
        SkillPathDisplay::Needed => PathDisplay::Needed,
        SkillPathDisplay::Always => PathDisplay::Always,
    }
}

pub fn render_skill_index(disclosure: &DisclosureRegistry, workspace: &Path) -> SkillIndexMarkdown {
    if disclosure.skills.is_empty() {
        return SkillIndexMarkdown::empty();
    }
    let mut lines = Vec::with_capacity(disclosure.skills.len());
    for skill in &disclosure.skills {
        let mut line = format!("- `{}` — {}", skill.name, skill.description);
        if let Some(path) = &skill.path {
            line.push_str(" (path: `");
            line.push_str(&readable_path(path, workspace));
            line.push_str("`)");
        }
        lines.push(line);
    }
    SkillIndexMarkdown::new(lines.join("\n"))
}

fn readable_path(path: &Path, workspace: &Path) -> String {
    match path.strip_prefix(workspace) {
        Ok(rel) => Path::new("/workspace").join(rel).display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use loom_driver::config::{SkillRegistration, SkillsConfig};
    use loom_driver::git::{commit_all_in, init_test_repo};
    use loom_skills::disclosure::PathDisplay;
    use loom_skills::identity::{SkillDescription, SkillName};
    use loom_skills::registry::DisclosureSkill;

    fn disclosure(mode: DisclosureMode, path: Option<PathBuf>) -> DisclosureRegistry {
        DisclosureRegistry {
            mode,
            path_display: PathDisplay::Needed,
            skills: vec![DisclosureSkill {
                name: SkillName::new("rust-review").expect("valid skill name"),
                description: SkillDescription::new("Use when reviewing Rust code.")
                    .expect("valid description"),
                path,
            }],
        }
    }

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, body).expect("write file");
    }

    #[test]
    fn skill_registration_policy_auto_and_prompt() {
        assert_eq!(
            disclosure_mode_for(AgentRuntime::Direct, SkillRegistration::Auto),
            DisclosureMode::Prompt,
        );
        assert_eq!(
            disclosure_mode_for(AgentRuntime::Pi, SkillRegistration::Auto),
            DisclosureMode::Prompt,
        );
        assert_eq!(
            disclosure_mode_from_policy(SkillRegistration::Auto, NativeRegistration::Supported),
            DisclosureMode::Native,
        );
        assert_eq!(
            disclosure_mode_from_policy(SkillRegistration::Prompt, NativeRegistration::Supported),
            DisclosureMode::Prompt,
        );
        assert_eq!(
            disclosure_mode_from_policy(SkillRegistration::Prompt, NativeRegistration::Unsupported),
            DisclosureMode::Prompt,
        );
    }

    #[test]
    fn skill_disclosure_derives_from_backend_and_registration_policy() {
        assert_eq!(
            disclosure_mode_for(AgentRuntime::Direct, SkillRegistration::Auto),
            DisclosureMode::Prompt,
        );
        assert_eq!(
            disclosure_mode_for(AgentRuntime::Pi, SkillRegistration::Auto),
            DisclosureMode::Prompt,
        );
        assert_eq!(
            disclosure_mode_from_policy(SkillRegistration::Auto, NativeRegistration::Supported),
            DisclosureMode::Native,
        );
    }

    #[test]
    fn prompt_skill_registration_policy_disables_native() {
        assert_eq!(
            disclosure_mode_from_policy(SkillRegistration::Prompt, NativeRegistration::Supported),
            DisclosureMode::Prompt,
        );
        assert_eq!(
            disclosure_mode_from_policy(SkillRegistration::Prompt, NativeRegistration::Unsupported),
            DisclosureMode::Prompt,
        );
    }

    #[test]
    fn skill_prompt_index_disclosure_modes() {
        let workspace = tempfile::tempdir().expect("workspace");
        let skill_path = workspace.path().join("skills/rust-review.md");
        let prompt = render_skill_index(
            &disclosure(DisclosureMode::Prompt, Some(skill_path)),
            workspace.path(),
        );
        let prompt_rendered = prompt.as_str();
        assert!(prompt_rendered.contains("`rust-review`"));
        assert!(prompt_rendered.contains("/workspace/skills/rust-review.md"));
        assert!(!prompt_rendered.contains(&workspace.path().display().to_string()));

        let native =
            render_skill_index(&disclosure(DisclosureMode::Native, None), workspace.path());
        assert!(native.as_str().contains("`rust-review`"));
        assert!(!native.as_str().contains("path:"));

        let path = workspace.path().join("skills/rust-review.md");
        let mut disclosed = disclosure(DisclosureMode::Native, Some(path));
        disclosed.path_display = PathDisplay::Always;
        let with_path = render_skill_index(&disclosed, workspace.path());
        assert!(
            with_path
                .as_str()
                .contains("/workspace/skills/rust-review.md")
        );
    }

    #[test]
    fn direct_skill_disclosure_uses_readable_paths() {
        let workspace = tempfile::tempdir().expect("workspace");
        let skill_path = workspace.path().join("skills/rust-review.md");
        let index = render_skill_index(
            &disclosure(DisclosureMode::Prompt, Some(skill_path)),
            workspace.path(),
        );
        let rendered = index.as_str();

        assert!(rendered.contains("`rust-review`"));
        assert!(rendered.contains("/workspace/skills/rust-review.md"));
        assert!(!rendered.contains(&workspace.path().display().to_string()));
    }

    #[test]
    fn resolve_from_workspace_loads_tracked_packages_without_configured_paths() {
        let workspace = tempfile::tempdir().expect("workspace");
        init_test_repo(workspace.path()).expect("init git repo");
        write(
            &workspace.path().join("skills/review/skill.md"),
            "---\nname: repo-review\ndescription: Use when testing inbox skill discovery.\n---\nBody\n",
        );
        commit_all_in(workspace.path(), "add repo skill").expect("commit skill");

        let plan = SkillPlan::resolve_from_workspace_sync(
            workspace.path(),
            "inbox",
            &ProfileName::new("rust"),
            AgentRuntime::Direct,
            &SkillsConfig::default(),
        )
        .expect("skill plan resolves");
        let scratch = tempfile::tempdir().expect("scratch");
        let session = plan
            .materialize(scratch.path(), workspace.path())
            .expect("skills materialize");

        assert!(session.skill_index.as_str().contains("`repo-review`"));
        assert!(
            session
                .skill_index
                .as_str()
                .contains("/workspace/skills/review/skill.md")
        );
    }
}

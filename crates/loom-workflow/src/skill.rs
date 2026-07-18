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

    use std::os::unix::fs::PermissionsExt;

    use loom_agent::PiBackend;
    use loom_driver::agent::{ImageSourceKind, RePinContent, SpawnConfig};
    use loom_driver::config::{SkillRegistration, SkillsConfig};
    use loom_driver::git::{commit_all_in, init_test_repo};
    use loom_skills::disclosure::PathDisplay;
    use loom_skills::identity::{SkillDescription, SkillName};
    use loom_skills::registry::{DisclosureSkill, RegisteredSkills};

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

    fn spawn_config(skills: RegisteredSkills, prompt: String, scratch: &Path) -> SpawnConfig {
        SpawnConfig {
            image_ref: "localhost/wrix-test:pi".into(),
            image_source: PathBuf::from("/nix/store/zzz-wrix-test"),
            image_source_kind: Some(ImageSourceKind::NixDescriptor),
            wrix_launcher: None,
            profile_config: None,
            workspace: PathBuf::from("/workspace"),
            env: vec![],
            mounts: vec![],
            initial_prompt: prompt,
            agent_args: vec![],
            repin: RePinContent {
                orientation: String::new(),
                pinned_context: String::new(),
                partial_bodies: vec![],
            },
            skills: Some(skills),
            event_metadata: None,
            scratch_dir: scratch.to_path_buf(),
            model_id: None,
            model: None,
            thinking_level: None,
            observers: Default::default(),
            output_limits: None,
            shutdown_grace: None,
            denied_tools: Vec::new(),
            handshake_timeout: None,
            stall_warn_interval: None,
            launcher_env: Vec::new(),
        }
    }

    fn install_probe_wrix(directory: &Path, calls: &Path) -> PathBuf {
        let bash = std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
            .map(|path| path.join("bash"))
            .find(|path| path.is_file())
            .expect("bash");
        let script = directory.join("wrix");
        write(
            &script,
            &format!(
                "#!{}\nset -euo pipefail\nprintf 'x\\n' >> '{}'\necho '[wrix] Starting container (mock)...' >&2\nIFS= read -r probe\nid=\"$(sed -n 's/.*\"id\":\"\\([^\"]*\\)\".*/\\1/p' <<<\"$probe\")\"\nprintf '{{\"type\":\"response\",\"id\":\"%s\",\"command\":\"get_state\",\"success\":true,\"data\":{{\"isStreaming\":false,\"isCompacting\":false,\"messageCount\":0,\"pendingMessageCount\":0}}}}\\n' \"$id\"\nwhile IFS= read -r _line; do :; done\n",
                bash.display(),
                calls.display(),
            ),
        );
        let mut permissions = fs::metadata(&script).expect("stat wrix").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("chmod wrix");
        script
    }

    #[tokio::test]
    async fn skill_registration_policy_auto_and_prompt() {
        let workspace = tempfile::tempdir().expect("workspace");
        let scratch = tempfile::tempdir().expect("scratch");
        let calls = workspace.path().join("calls");
        let wrix = install_probe_wrix(workspace.path(), &calls);
        let profile = ProfileName::new("rust");

        let auto_plan = SkillPlan::resolve(
            workspace.path(),
            &[],
            "loop",
            &profile,
            AgentRuntime::Pi,
            &SkillsConfig::default(),
        )
        .expect("auto plan");
        let auto = auto_plan
            .materialize(scratch.path(), workspace.path())
            .expect("materialize auto skills");
        assert_eq!(auto.registered.disclosure(), DisclosureMode::Prompt);
        assert!(!auto.registered.registry().skills().is_empty());
        let auto_config = spawn_config(
            auto.registered.clone(),
            auto.skill_index.as_str().to_owned(),
            scratch.path(),
        );
        let auto_session = PiBackend::spawn_with_wrix_bin(&auto_config, wrix.as_os_str())
            .await
            .expect("auto prompt disclosure reaches real backend spawn");
        drop(auto_session);

        let prompt_policy = SkillsConfig {
            registration: SkillRegistration::Prompt,
            ..SkillsConfig::default()
        };
        let prompt_plan = SkillPlan::resolve(
            workspace.path(),
            &[],
            "loop",
            &profile,
            AgentRuntime::Pi,
            &prompt_policy,
        )
        .expect("prompt plan");
        let prompt = prompt_plan
            .materialize(scratch.path(), workspace.path())
            .expect("materialize prompt skills");
        assert_eq!(prompt.registered.disclosure(), DisclosureMode::Prompt);
        let prompt_config = spawn_config(
            prompt.registered.clone(),
            prompt.skill_index.as_str().to_owned(),
            scratch.path(),
        );
        let prompt_session = PiBackend::spawn_with_wrix_bin(&prompt_config, wrix.as_os_str())
            .await
            .expect("prompt policy reaches real backend spawn");
        drop(prompt_session);

        let native =
            RegisteredSkills::new(prompt.registered.registry().clone(), DisclosureMode::Native);
        let native_config = spawn_config(native, String::new(), scratch.path());
        let error = match PiBackend::spawn_with_wrix_bin(&native_config, wrix.as_os_str()).await {
            Ok(_) => panic!("declared native mode spawned without a registrar"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            loom_driver::agent::ProtocolError::Unsupported
        ));
        assert_eq!(
            fs::read_to_string(calls)
                .expect("spawn calls")
                .lines()
                .count(),
            2,
            "native registration failure must stop before the Wrix child starts",
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

//! Public skill artifact model and registry stages.

pub mod builtin;
pub mod disclosure;
pub mod discovery;
pub mod document;
pub mod identity;
pub mod registry;
pub mod source;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use loom_events::identifier::ProfileName;
    use tempfile::TempDir;

    use crate::builtin;
    use crate::discovery::{
        DiagnosticKind, DiagnosticSeverity, DiscoveryError, discover_workspace,
        load_configured_paths, load_overrides, load_workspace,
    };
    use crate::document::{RawSkillDocument, SkillDocument};
    use crate::identity::{PhaseName, SkillName};
    use crate::registry::{
        ApplicableRegistry, MaterializedRegistry, NamedSkill, RegistryError, SkillRegistry,
    };
    use crate::source::{SkillProvenance, SkillSource, SourceShape};

    fn skill_markdown(name: &str, description: &str, metadata: &str) -> String {
        format!("---\nname: {name}\ndescription: {description}\n{metadata}---\nBody\n")
    }

    fn loose_named(name: &str, source: SkillSource, metadata: &str) -> NamedSkill {
        let markdown = skill_markdown(name, "Use when testing skill registry behavior.", metadata);
        let provenance = SkillProvenance {
            source,
            shape: SourceShape::LooseFile,
            document_path: PathBuf::from(format!("{name}.md")),
            base_dir: PathBuf::new(),
            tuning_path: None,
            built_in_bundle: None,
            built_in_name: None,
            source_hash: blake3::hash(markdown.as_bytes()).to_hex().to_string(),
        };
        let document = SkillDocument::parse(RawSkillDocument::new(markdown, provenance))
            .expect("document parses");
        NamedSkill::from_document(document).expect("skill is named")
    }

    fn workspace() -> TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, body).expect("write file");
    }

    #[test]
    fn skill_registry_typestate_prevents_misuse() {
        let raw = RawSkillDocument::new(
            skill_markdown(
                "typed-skill",
                "Use when checking that raw markdown cannot be registered.",
                "",
            ),
            SkillProvenance {
                source: SkillSource::Workspace,
                shape: SourceShape::LooseFile,
                document_path: PathBuf::from("typed-skill.md"),
                base_dir: PathBuf::new(),
                tuning_path: None,
                built_in_bundle: None,
                built_in_name: None,
                source_hash: "hash".to_string(),
            },
        );
        let document = SkillDocument::parse(raw).expect("document parses");
        let named = NamedSkill::from_document(document).expect("named skill");
        let registry = SkillRegistry::new(vec![named]).expect("registry resolves");
        let applicable = ApplicableRegistry::filter(
            registry,
            &PhaseName::new("loop").expect("phase"),
            &ProfileName::new("base"),
        );
        let materialized = MaterializedRegistry::materialize(
            applicable,
            tempfile::tempdir().expect("scratch").path(),
        )
        .expect("materializes");
        assert_eq!(materialized.skills().len(), 1);
    }

    #[test]
    fn skill_registry_discovery_and_duplicate_policy() {
        let repo = workspace();
        let package = repo.path().join("skills/review/Skill.md");
        write(
            &package,
            &skill_markdown(
                "repo-review",
                "Use when testing git tracked package discovery.",
                "",
            ),
        );
        let loose = repo.path().join("configured/loose.md");
        write(
            &loose,
            &skill_markdown(
                "configured-loose",
                "Use when testing configured loose skill discovery.",
                "",
            ),
        );
        let nested = repo.path().join("configured/nested/extra.md");
        write(
            &nested,
            &skill_markdown(
                "configured-extra",
                "Use when testing configured directory recursion.",
                "",
            ),
        );
        let override_file = repo.path().join(".loom-override/skills/override.md");
        write(
            &override_file,
            &skill_markdown(
                "loom-final-reporting",
                "Use when testing loose built-in overrides.",
                "",
            ),
        );
        let override_package = repo.path().join(".loom-override/skills/verify/skill.md");
        write(
            &override_package,
            &skill_markdown(
                "loom-verify-after-edit",
                "Use when testing tracked package built-in overrides.",
                "",
            ),
        );
        let tracked = vec![
            PathBuf::from("skills/review/Skill.md"),
            PathBuf::from(".loom-override/skills/verify/skill.md"),
        ];
        let report = load_workspace(repo.path(), &tracked, &[PathBuf::from("configured")])
            .expect("workspace skills load");
        let names = report
            .set()
            .skills()
            .iter()
            .map(|skill| skill.name().as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"repo-review"));
        assert!(names.contains(&"configured-loose"));
        assert!(names.contains(&"configured-extra"));
        assert!(names.contains(&"loom-final-reporting"));
        assert!(names.contains(&"loom-verify-after-edit"));
        let override_sources = report
            .set()
            .skills()
            .iter()
            .filter(|skill| skill.name().as_str() == "loom-verify-after-edit")
            .map(NamedSkill::source)
            .collect::<Vec<_>>();
        assert_eq!(override_sources, vec![SkillSource::Override]);

        let duplicate = repo.path().join("skills/review/skill.md");
        write(
            &duplicate,
            &skill_markdown(
                "repo-review-duplicate",
                "Use when testing duplicate case variants.",
                "",
            ),
        );
        let tracked = vec![
            PathBuf::from("skills/review/Skill.md"),
            PathBuf::from("skills/review/skill.md"),
        ];
        let err =
            discover_workspace(repo.path(), &tracked).expect_err("duplicate case variant rejects");
        assert!(
            matches!(err, DiscoveryError::DuplicateCaseVariant { basename, .. } if basename == "skill.md")
        );

        let duplicate_name = SkillRegistry::new(vec![
            loose_named("same-name", SkillSource::Workspace, ""),
            loose_named("same-name", SkillSource::Configured, ""),
        ])
        .expect_err("duplicate names reject");
        assert!(
            matches!(duplicate_name, RegistryError::DuplicateName { name } if name.as_str() == "same-name")
        );
    }

    #[test]
    fn configured_directory_overlap_skips_package_contents() {
        let repo = workspace();
        write(
            &repo.path().join("skills/review/skill.md"),
            &skill_markdown(
                "repo-review",
                "Use when testing configured overlap with package discovery.",
                "",
            ),
        );
        write(&repo.path().join("skills/review/tuning.md"), "# Tuning\n");
        write(&repo.path().join("skills/review/notes.md"), "# Notes\n");

        let report = load_workspace(
            repo.path(),
            &[
                PathBuf::from("skills/review/skill.md"),
                PathBuf::from("skills/review/tuning.md"),
                PathBuf::from("skills/review/notes.md"),
            ],
            &[PathBuf::from("skills")],
        )
        .expect("overlapping configured directory skips package contents");

        let skills = report.set().skills();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name().as_str(), "repo-review");
        assert_eq!(
            skills[0].provenance().tuning_path,
            Some(repo.path().join("skills/review/tuning.md"))
        );
    }

    #[test]
    fn workspace_discovery_skips_loom_builtin_source_packages() {
        let repo = workspace();
        let source_path = repo
            .path()
            .join("crates/loom-skills/builtin/base/loom-context-before-edit/skill.md");
        write(
            &source_path,
            &skill_markdown(
                "loom-context-before-edit",
                "Use when testing built-in source discovery.",
                "",
            ),
        );
        let report = load_workspace(
            repo.path(),
            &[
                PathBuf::from("crates/loom-skills/src/builtin.rs"),
                PathBuf::from("crates/loom-skills/builtin/base/loom-context-before-edit/skill.md"),
            ],
            &[],
        )
        .expect("workspace skills load");
        assert_eq!(report.set().skills().len(), 0);

        let mut set = builtin::catalog().expect("catalog");
        set.extend(report.into_set());
        SkillRegistry::from_set(set).expect("built-in source packages are not duplicated");
    }

    #[test]
    fn skill_frontmatter_diagnostics_by_source() {
        let repo = workspace();
        write(
            repo.path().join("bad/skill.md").as_path(),
            "---\ndescription: Missing name.\n---\n",
        );

        let report = discover_workspace(repo.path(), &[PathBuf::from("bad/skill.md")])
            .expect("auto discovery warns");
        assert_eq!(report.set().skills().len(), 0);
        assert_eq!(report.warnings().len(), 1);
        assert_eq!(report.warnings()[0].severity, DiagnosticSeverity::Warning);
        assert_eq!(report.warnings()[0].source, SkillSource::Workspace);
        assert_eq!(report.warnings()[0].kind, DiagnosticKind::MissingName);

        let configured = repo.path().join("configured.md");
        write(&configured, "---\ndescription: Missing name.\n---\n");
        let err = load_configured_paths(repo.path(), &[PathBuf::from("configured.md")])
            .expect_err("configured invalid skill errors");
        assert!(matches!(
            err,
            DiscoveryError::InvalidSkill {
                skill_source: SkillSource::Configured,
                kind: DiagnosticKind::MissingName,
                ..
            }
        ));

        let override_path = repo.path().join(".loom-override/skills/bad.md");
        write(&override_path, "---\ndescription: Missing name.\n---\n");
        let err = load_overrides(repo.path()).expect_err("override invalid skill errors");
        assert!(matches!(
            err,
            DiscoveryError::InvalidSkill {
                skill_source: SkillSource::Override,
                kind: DiagnosticKind::MissingName,
                ..
            }
        ));

        let catalog =
            builtin::catalog().expect("built-ins are a fatal release contract when invalid");
        assert_eq!(catalog.skills().len(), 13);
    }

    #[test]
    fn builtin_skill_profile_selection_and_override_policy() {
        let repo = workspace();
        let override_path = repo.path().join(".loom-override/skills/final.md");
        write(
            &override_path,
            &skill_markdown(
                "loom-final-reporting",
                "Use when testing a valid built-in override.",
                "metadata:\n  loom:\n    phases: [\"loop\"]\n",
            ),
        );
        let mut set = builtin::catalog().expect("catalog");
        set.extend(load_overrides(repo.path()).expect("overrides load"));
        let registry = SkillRegistry::from_set(set).expect("override resolves");
        let applicable = ApplicableRegistry::filter(
            registry,
            &PhaseName::new("loop").expect("phase"),
            &ProfileName::new("rust"),
        );
        let scratch = tempfile::tempdir().expect("scratch");
        let materialized = MaterializedRegistry::materialize(applicable, scratch.path())
            .expect("materializes built-ins");
        let names = materialized
            .skills()
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"loom-context-before-edit"));
        assert!(names.contains(&"loom-rust-verification"));
        let built_in = materialized
            .skills()
            .iter()
            .find(|skill| skill.name.as_str() == "loom-context-before-edit")
            .expect("base built-in present");
        assert_eq!(
            built_in.path,
            scratch
                .path()
                .join("skills/loom-context-before-edit/skill.md")
        );
        assert!(built_in.path.is_file());
        let overridden = materialized
            .skills()
            .iter()
            .find(|skill| skill.name.as_str() == "loom-final-reporting")
            .expect("override present");
        assert_eq!(overridden.source, SkillSource::Override);
        assert_eq!(overridden.path, override_path);

        let unknown_override = loose_named("not-a-built-in", SkillSource::Override, "");
        let err = SkillRegistry::from_set(crate::registry::SkillSet::new(vec![unknown_override]))
            .expect_err("unknown override rejects");
        assert!(
            matches!(err, RegistryError::UnknownBuiltInOverride { name } if name == SkillName::new("not-a-built-in").expect("valid"))
        );
    }

    #[test]
    fn skill_frontmatter_phase_profile_filters() {
        let unfiltered = loose_named("all-phases", SkillSource::Workspace, "");
        let loop_rust = loose_named(
            "loop-rust",
            SkillSource::Workspace,
            "metadata:\n  loom:\n    phases: [\"loop\"]\n    profiles: [\"rust\"]\n",
        );
        let review_base = loose_named(
            "review-base",
            SkillSource::Workspace,
            "metadata:\n  loom:\n    phases: [\"review\"]\n    profiles: [\"base\"]\n",
        );
        let registry = SkillRegistry::new(vec![unfiltered, loop_rust, review_base])
            .expect("registry resolves");
        let applicable = ApplicableRegistry::filter(
            registry,
            &PhaseName::new("loop").expect("phase"),
            &ProfileName::new("rust"),
        );
        let names = applicable
            .skills()
            .iter()
            .map(|skill| skill.name().as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["all-phases", "loop-rust"]);
    }
}

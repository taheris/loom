use std::path::PathBuf;

use loom_driver::agent::{RePinContent, SpawnConfig, set_loom_inside};
use loom_driver::bd::Bead;
use loom_driver::identifier::ProfileName;
use loom_driver::profile_manifest::{ProfileError, ProfileImageManifest};

use super::profile::resolve_profile_image;

/// Internal helper. The public dispatch surface is
/// [`build_spawn_config_from_manifest`] — callers should never construct a
/// `SpawnConfig` field-by-field, because doing so silently bypasses the
/// profile-image resolution and the canonical claude/pi env wiring.
fn build_spawn_config(
    image_ref: String,
    image_source: PathBuf,
    workspace: PathBuf,
    initial_prompt: String,
    scratch_dir: PathBuf,
    extra_env: Vec<(String, String)>,
    agent_args: Vec<String>,
) -> SpawnConfig {
    let mut env = extra_env;
    set_loom_inside(&mut env);
    SpawnConfig {
        image_ref,
        image_source,
        workspace,
        env,
        initial_prompt,
        agent_args,
        repin: RePinContent {
            orientation: String::new(),
            pinned_context: String::new(),
            partial_bodies: vec![],
        },
        scratch_dir,
        model: None,
        thinking_level: None,
        shutdown_grace: None,
        handshake_timeout: None,
        stall_warn_interval: None,
    }
}

/// Build a [`SpawnConfig`] for `bead` by resolving its profile through the
/// parsed [`ProfileImageManifest`].
///
/// Implements `specs/harness.md` § Profile-Image Manifest per-bead
/// dispatch: the bead's `profile:X` label (or the CLI `--profile` override)
/// is looked up against the manifest to fill `image_ref` + `image_source`.
/// Missing manifest entries surface as [`ProfileError::UnknownProfile`] so
/// the dispatcher can fail loudly instead of falling back to a default
/// profile silently. `phase_default` carries the per-phase fallback name
/// (already chained through `[phase.run]` → `[phase.default]` → built-in
/// `base` by `LoomConfig::agent_for`).
#[expect(clippy::too_many_arguments, reason = "explicit dispatch surface")]
pub fn build_spawn_config_from_manifest(
    manifest: &ProfileImageManifest,
    bead: &Bead,
    override_: Option<&ProfileName>,
    phase_default: &ProfileName,
    workspace: PathBuf,
    initial_prompt: String,
    scratch_dir: PathBuf,
    extra_env: Vec<(String, String)>,
    agent_args: Vec<String>,
) -> Result<SpawnConfig, ProfileError> {
    let entry = resolve_profile_image(manifest, &bead.labels, override_, phase_default)?;
    Ok(build_spawn_config(
        entry.r#ref.clone(),
        entry.source.clone(),
        workspace,
        initial_prompt,
        scratch_dir,
        extra_env,
        agent_args,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::bd::Label;
    use loom_driver::identifier::BeadId;

    fn bead_with_labels(id: &str, labels: &[&str]) -> Bead {
        Bead {
            id: BeadId::new(id).expect("valid bead id"),
            title: format!("title-{id}"),
            description: "desc".into(),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            labels: labels.iter().map(|s| Label::new(*s)).collect(),
            parent: None,
            metadata: Default::default(),
            notes: None,
        }
    }

    fn three_profile_manifest(dir: &std::path::Path) -> ProfileImageManifest {
        let body = r#"{
          "base":   { "ref": "localhost/wrapix-base:abc",   "source": "/nix/store/aaa-image-base" },
          "rust":   { "ref": "localhost/wrapix-rust:def",   "source": "/nix/store/bbb-image-rust" },
          "python": { "ref": "localhost/wrapix-python:ghi", "source": "/nix/store/ccc-image-python" }
        }"#;
        let path = dir.join("profile-images.json");
        std::fs::write(&path, body).expect("write manifest");
        ProfileImageManifest::from_path(&path).expect("parse manifest")
    }

    fn base() -> ProfileName {
        ProfileName::new("base")
    }

    /// Per-bead dispatch: two beads with different `profile:X` labels
    /// produce SpawnConfigs with different `image_ref` + `image_source`.
    /// Argv-shape is verified by the integration test in
    /// `loom/tests/spawn_dispatch.rs`.
    #[test]
    fn per_bead_profile_dispatch_produces_distinct_image_refs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = three_profile_manifest(dir.path());

        let rust_bead = bead_with_labels("wx-1", &["spec:harness", "profile:rust"]);
        let python_bead = bead_with_labels("wx-2", &["spec:harness", "profile:python"]);

        let cfg_rust = build_spawn_config_from_manifest(
            &manifest,
            &rust_bead,
            None,
            &base(),
            PathBuf::from("/work/wx-1"),
            "rust prompt".into(),
            dir.path().join("scratch"),
            vec![],
            vec![],
        )
        .expect("rust dispatch");
        let cfg_python = build_spawn_config_from_manifest(
            &manifest,
            &python_bead,
            None,
            &base(),
            PathBuf::from("/work/wx-2"),
            "python prompt".into(),
            dir.path().join("scratch"),
            vec![],
            vec![],
        )
        .expect("python dispatch");

        assert_eq!(cfg_rust.image_ref, "localhost/wrapix-rust:def");
        assert_eq!(
            cfg_rust.image_source,
            PathBuf::from("/nix/store/bbb-image-rust")
        );
        assert_eq!(cfg_python.image_ref, "localhost/wrapix-python:ghi");
        assert_eq!(
            cfg_python.image_source,
            PathBuf::from("/nix/store/ccc-image-python")
        );
        assert_ne!(cfg_rust.image_ref, cfg_python.image_ref);
        assert_ne!(cfg_rust.image_source, cfg_python.image_source);
    }

    /// FR5 (`--profile` CLI override precedence): the same bead resolves
    /// to two different SpawnConfigs depending on whether the override is
    /// applied.
    #[test]
    fn cli_override_swaps_resolved_image() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = three_profile_manifest(dir.path());
        let bead = bead_with_labels("wx-1", &["spec:harness", "profile:rust"]);

        let labelled = build_spawn_config_from_manifest(
            &manifest,
            &bead,
            None,
            &base(),
            PathBuf::from("/work/wx-1"),
            "p".into(),
            dir.path().join("scratch"),
            vec![],
            vec![],
        )
        .expect("rust dispatch");
        let overridden = build_spawn_config_from_manifest(
            &manifest,
            &bead,
            Some(&ProfileName::new("python")),
            &base(),
            PathBuf::from("/work/wx-1"),
            "p".into(),
            dir.path().join("scratch"),
            vec![],
            vec![],
        )
        .expect("python dispatch");

        assert_eq!(labelled.image_ref, "localhost/wrapix-rust:def");
        assert_eq!(overridden.image_ref, "localhost/wrapix-python:ghi");
        assert_ne!(labelled.image_ref, overridden.image_ref);
    }

    /// wx-cmzob: sequential (`loom run`) and parallel (`loom run -p N`)
    /// must produce identical SpawnConfigs for the same bead modulo the
    /// workspace path — sequential dispatches against the repo root,
    /// parallel against a per-bead worktree, but every other field
    /// (image_ref, image_source, env, agent_args, prompt) must
    /// match. If either path adds an arg or rewrites the prompt format,
    /// this test trips before the divergence reaches users.
    #[test]
    fn sequential_and_parallel_dispatch_produce_identical_spawn_configs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = three_profile_manifest(dir.path());
        let bead = bead_with_labels("wx-1", &["spec:harness", "profile:rust"]);
        let prompt = format!("loom run: bead {}", bead.id);

        let seq = build_spawn_config_from_manifest(
            &manifest,
            &bead,
            None,
            &base(),
            PathBuf::from("/repo-root"),
            prompt.clone(),
            dir.path().join("scratch"),
            vec![],
            vec![],
        )
        .expect("sequential dispatch");
        let par = build_spawn_config_from_manifest(
            &manifest,
            &bead,
            None,
            &base(),
            PathBuf::from("/repo-root/.wrapix/worktree/wx-1"),
            prompt,
            dir.path().join("scratch"),
            vec![],
            vec![],
        )
        .expect("parallel dispatch");

        assert_eq!(seq.image_ref, par.image_ref);
        assert_eq!(seq.image_source, par.image_source);
        assert_eq!(seq.env, par.env);
        assert_eq!(seq.agent_args, par.agent_args);
        assert_eq!(seq.initial_prompt, par.initial_prompt);
        assert!(seq.model.is_none() && par.model.is_none());
        assert_ne!(
            seq.workspace, par.workspace,
            "workspace MUST differ — parallel uses a per-bead worktree",
        );
    }

    /// Every dispatched bead container receives `LOOM_INSIDE=1` via
    /// [`SpawnConfig::env`] so the nested-loom guard at CLI entry can
    /// refuse mutating subcommands. Spec: `harness.md` § Nested-Loom
    /// Guard, success criterion `test_loom_inside_env_set`.
    #[test]
    fn spawn_config_env_includes_loom_inside_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = three_profile_manifest(dir.path());
        let bead = bead_with_labels("wx-1", &["profile:rust"]);

        let cfg = build_spawn_config_from_manifest(
            &manifest,
            &bead,
            None,
            &base(),
            PathBuf::from("/work"),
            "p".into(),
            dir.path().join("scratch"),
            vec![("WRAPIX_AGENT".into(), "claude".into())],
            vec![],
        )
        .expect("dispatch");
        assert!(
            cfg.env.iter().any(|(k, v)| k == "LOOM_INSIDE" && v == "1"),
            "SpawnConfig.env missing LOOM_INSIDE=1: {:?}",
            cfg.env,
        );
        // Caller-supplied env entries must survive the injection.
        assert!(
            cfg.env
                .iter()
                .any(|(k, v)| k == "WRAPIX_AGENT" && v == "claude"),
            "SpawnConfig.env dropped caller env: {:?}",
            cfg.env,
        );
    }

    /// A bead with a `profile:X` not declared in the manifest fails
    /// loudly with [`ProfileError::UnknownProfile`] — no silent default.
    #[test]
    fn unknown_profile_label_returns_typed_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = three_profile_manifest(dir.path());
        let bead = bead_with_labels("wx-1", &["profile:ruby"]);

        let err = build_spawn_config_from_manifest(
            &manifest,
            &bead,
            None,
            &base(),
            PathBuf::from("/work"),
            "p".into(),
            dir.path().join("scratch"),
            vec![],
            vec![],
        )
        .expect_err("expected unknown profile");
        match err {
            ProfileError::UnknownProfile { name, .. } => {
                assert_eq!(name, ProfileName::new("ruby"));
            }
            other => panic!("expected UnknownProfile, got {other:?}"),
        }
    }
}

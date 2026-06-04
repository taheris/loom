use std::path::{Path, PathBuf};

use loom_driver::agent::{MountSpec, RePinContent, SpawnConfig, set_loom_inside};
use loom_driver::bd::Bead;
use loom_driver::config::LoomTopConfig;
use loom_driver::identifier::ProfileName;
use loom_driver::profile_manifest::{ProfileError, ProfileImageManifest};

use super::profile::resolve_profile_image;

/// Workspace-relative host path of the `wrapix-beads` dolt socket. Held as a
/// constant so the bead-container mount and any future host-side consumer of
/// the same socket stay in lockstep.
const DOLT_SOCKET_REL_PATH: &str = ".wrapix/dolt.sock";

/// Container-side path the dolt socket is projected to. Matches
/// `BEADS_DOLT_SERVER_SOCKET` set in the bead container env.
const DOLT_SOCKET_CONTAINER_PATH: &str = "/workspace/.wrapix/dolt.sock";

/// Build the [`MountSpec`] that projects the loom workspace's `wrapix-beads`
/// dolt socket into a bead container at [`DOLT_SOCKET_CONTAINER_PATH`].
///
/// Returns `None` when the host socket is absent (e.g. test fixtures that do
/// not stand up the wrapix-beads server), so the resulting spawn config has
/// no socket mount in that environment. Production callers running under a
/// real loom workspace always observe `Some`. Linux passes the socket file
/// through virtiofs directly; on Darwin the wrapix sandbox classifier
/// rejects Unix-socket `host_path` entries at launch — see
/// `specs/harness.md` § Bead dispatch / Darwin compatibility.
pub fn dolt_socket_mount(loom_workspace: &Path) -> Option<MountSpec> {
    let host_path = loom_workspace.join(DOLT_SOCKET_REL_PATH);
    if !host_path.exists() {
        return None;
    }
    Some(MountSpec {
        host_path,
        container_path: PathBuf::from(DOLT_SOCKET_CONTAINER_PATH),
        read_only: false,
    })
}

/// Build the optional [`MountSpec`] that projects the shared sccache
/// directory into a container at the configured container path.
///
/// Returns `Some(MountSpec)` when [`LoomTopConfig::sccache_dir`] is set;
/// `None` otherwise (the feature is disabled and every cargo invocation
/// pays the full cold-build cost). `read_only = false` because sccache
/// clients write through the cache. See `specs/harness.md` § Bead dispatch
/// — `sccache_mount_present_when_configured` /
/// `sccache_mount_omitted_when_unset`.
pub fn sccache_mount(cfg: &LoomTopConfig) -> Option<MountSpec> {
    let host_path = cfg.sccache_dir.clone()?;
    Some(MountSpec {
        host_path,
        container_path: cfg.sccache_container_path.clone(),
        read_only: false,
    })
}

/// Internal helper. The public dispatch surface is
/// [`build_spawn_config_from_manifest`] — callers should never construct a
/// `SpawnConfig` field-by-field, because doing so silently bypasses the
/// profile-image resolution and the canonical claude/pi env wiring.
#[expect(clippy::too_many_arguments, reason = "internal helper")]
fn build_spawn_config(
    image_ref: String,
    image_source: PathBuf,
    workspace: PathBuf,
    initial_prompt: String,
    scratch_dir: PathBuf,
    extra_env: Vec<(String, String)>,
    agent_args: Vec<String>,
    mounts: Vec<MountSpec>,
    launcher_env: Vec<(String, String)>,
) -> SpawnConfig {
    let mut env = extra_env;
    set_loom_inside(&mut env);
    SpawnConfig {
        image_ref,
        image_source,
        workspace,
        env,
        mounts,
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
        output_limits: None,
        shutdown_grace: None,
        handshake_timeout: None,
        stall_warn_interval: None,
        launcher_env,
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
///
/// `launcher_env` carries host-only key paths (`WRAPIX_DEPLOY_KEY` /
/// `WRAPIX_SIGNING_KEY`) onto [`SpawnConfig::launcher_env`] — resolve it via
/// [`GitClient::launcher_key_env`] at the dispatch site so the backend can
/// set them on the `wrapix spawn` child process.
///
/// [`GitClient::launcher_key_env`]: loom_driver::git::GitClient::launcher_key_env
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
    mounts: Vec<MountSpec>,
    launcher_env: Vec<(String, String)>,
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
        mounts,
        launcher_env,
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

        let rust_bead = bead_with_labels("lm-1", &["spec:harness", "profile:rust"]);
        let python_bead = bead_with_labels("lm-2", &["spec:harness", "profile:python"]);

        let cfg_rust = build_spawn_config_from_manifest(
            &manifest,
            &rust_bead,
            None,
            &base(),
            PathBuf::from("/work/lm-1"),
            "rust prompt".into(),
            dir.path().join("scratch"),
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .expect("rust dispatch");
        let cfg_python = build_spawn_config_from_manifest(
            &manifest,
            &python_bead,
            None,
            &base(),
            PathBuf::from("/work/lm-2"),
            "python prompt".into(),
            dir.path().join("scratch"),
            vec![],
            vec![],
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
        let bead = bead_with_labels("lm-1", &["spec:harness", "profile:rust"]);

        let labelled = build_spawn_config_from_manifest(
            &manifest,
            &bead,
            None,
            &base(),
            PathBuf::from("/work/lm-1"),
            "p".into(),
            dir.path().join("scratch"),
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .expect("rust dispatch");
        let overridden = build_spawn_config_from_manifest(
            &manifest,
            &bead,
            Some(&ProfileName::new("python")),
            &base(),
            PathBuf::from("/work/lm-1"),
            "p".into(),
            dir.path().join("scratch"),
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .expect("python dispatch");

        assert_eq!(labelled.image_ref, "localhost/wrapix-rust:def");
        assert_eq!(overridden.image_ref, "localhost/wrapix-python:ghi");
        assert_ne!(labelled.image_ref, overridden.image_ref);
    }

    /// lm-cmzob: sequential (`loom loop`) and parallel (`loom loop -p N`)
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
        let bead = bead_with_labels("lm-1", &["spec:harness", "profile:rust"]);
        let prompt = format!("loom loop: bead {}", bead.id);

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
            vec![],
            vec![],
        )
        .expect("sequential dispatch");
        let par = build_spawn_config_from_manifest(
            &manifest,
            &bead,
            None,
            &base(),
            PathBuf::from("/repo-root/.loom/beads/lm-1"),
            prompt,
            dir.path().join("scratch"),
            vec![],
            vec![],
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
        let bead = bead_with_labels("lm-1", &["profile:rust"]);

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
            vec![],
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

    /// Spec gate (`specs/harness.md` § Bead dispatch —
    /// `bead_container_dolt_socket_via_mounts`): the host
    /// `wrapix-beads` dolt socket is projected into bead containers via a
    /// `SpawnConfig.mounts` entry at `/workspace/.wrapix/dolt.sock`,
    /// replacing the historical hardlink shim that lived in
    /// `GitClient::create_worktree`. The `host_path` resolves against the
    /// loom workspace's `.wrapix/dolt.sock`; `read_only = false` because
    /// dolt clients write through the socket.
    #[test]
    fn bead_container_dolt_socket_via_mounts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let loom_workspace = dir.path().join("loom-workspace");
        let socket_path = loom_workspace.join(DOLT_SOCKET_REL_PATH);
        std::fs::create_dir_all(socket_path.parent().expect("socket parent"))
            .expect("create wrapix dir");
        std::fs::write(&socket_path, b"").expect("touch socket");

        let mount = dolt_socket_mount(&loom_workspace).expect("socket present → mount");
        assert_eq!(mount.host_path, socket_path);
        assert_eq!(
            mount.container_path,
            PathBuf::from("/workspace/.wrapix/dolt.sock"),
        );
        assert!(
            !mount.read_only,
            "dolt clients write through the socket; mount must not be read-only",
        );

        let manifest = three_profile_manifest(dir.path());
        let bead = bead_with_labels("lm-1", &["profile:rust"]);
        let bead_workspace = loom_workspace.join(".loom/beads/lm-1");
        let cfg = build_spawn_config_from_manifest(
            &manifest,
            &bead,
            None,
            &base(),
            bead_workspace,
            "p".into(),
            dir.path().join("scratch"),
            vec![],
            vec![],
            vec![mount.clone()],
            vec![],
        )
        .expect("dispatch");
        assert_eq!(
            cfg.mounts,
            vec![mount],
            "SpawnConfig.mounts must carry the dolt-socket projection verbatim",
        );
    }

    /// When the loom workspace has no `wrapix-beads` dolt socket on disk
    /// (test fixtures, CI sandboxes), [`dolt_socket_mount`] returns `None`
    /// rather than projecting a missing host path the wrapix launcher
    /// would reject at startup.
    #[test]
    fn dolt_socket_mount_returns_none_when_socket_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(dolt_socket_mount(dir.path()).is_none());
    }

    /// Spec gate (`specs/harness.md` § Bead dispatch —
    /// `sccache_mount_present_when_configured`): when `[loom] sccache_dir`
    /// is set, the per-bead [`SpawnConfig`] carries an entry projecting
    /// the host cache into the container at `sccache_container_path`,
    /// and `SCCACHE_DIR` + `RUSTC_WRAPPER=sccache` land in container env.
    #[test]
    fn sccache_mount_present_when_configured() {
        let dir = tempfile::tempdir().expect("tempdir");
        let host_cache = dir.path().join("loom-sccache");
        std::fs::create_dir_all(&host_cache).expect("create cache dir");
        let cfg = LoomTopConfig {
            sccache_dir: Some(host_cache.clone()),
            sccache_container_path: PathBuf::from("/sccache"),
            ..LoomTopConfig::default()
        };
        let mount = sccache_mount(&cfg).expect("sccache configured → mount");
        assert_eq!(mount.host_path, host_cache);
        assert_eq!(mount.container_path, PathBuf::from("/sccache"));
        assert!(
            !mount.read_only,
            "sccache clients write through the cache; mount must not be read-only",
        );

        let manifest = three_profile_manifest(dir.path());
        let bead = bead_with_labels("lm-1", &["profile:rust"]);
        let spawn = build_spawn_config_from_manifest(
            &manifest,
            &bead,
            None,
            &base(),
            PathBuf::from("/work/lm-1"),
            "p".into(),
            dir.path().join("scratch"),
            cfg.container_sccache_env(),
            vec![],
            vec![mount.clone()],
            vec![],
        )
        .expect("dispatch");
        assert!(
            spawn.mounts.contains(&mount),
            "SpawnConfig.mounts must carry the sccache projection: {:?}",
            spawn.mounts,
        );
        assert!(
            spawn
                .env
                .iter()
                .any(|(k, v)| k == "SCCACHE_DIR" && v == "/sccache"),
            "SpawnConfig.env missing SCCACHE_DIR=/sccache: {:?}",
            spawn.env,
        );
        assert!(
            spawn
                .env
                .iter()
                .any(|(k, v)| k == "RUSTC_WRAPPER" && v == "sccache"),
            "SpawnConfig.env missing RUSTC_WRAPPER=sccache: {:?}",
            spawn.env,
        );
    }

    /// Honors a non-default container path: `sccache_container_path =
    /// "/var/sccache"` lands in both the mount entry and the
    /// `SCCACHE_DIR` env var so the container's sccache binary reads
    /// from the right location.
    #[test]
    fn sccache_mount_honors_custom_container_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let host_cache = dir.path().join("loom-sccache");
        let cfg = LoomTopConfig {
            sccache_dir: Some(host_cache.clone()),
            sccache_container_path: PathBuf::from("/var/sccache"),
            ..LoomTopConfig::default()
        };
        let mount = sccache_mount(&cfg).expect("configured → mount");
        assert_eq!(mount.host_path, host_cache);
        assert_eq!(mount.container_path, PathBuf::from("/var/sccache"));
        let env = cfg.container_sccache_env();
        assert!(
            env.iter()
                .any(|(k, v)| k == "SCCACHE_DIR" && v == "/var/sccache"),
            "container env must use the configured container path: {env:?}",
        );
    }

    /// Spec gate (`specs/harness.md` § Bead dispatch —
    /// `sccache_mount_omitted_when_unset`): with no `[loom] sccache_dir`
    /// set, no sccache mount appears on the bead-container spawn args
    /// and no sccache env entries are exported.
    #[test]
    fn sccache_mount_omitted_when_unset() {
        let cfg = LoomTopConfig::default();
        assert!(
            sccache_mount(&cfg).is_none(),
            "default config must not emit an sccache mount",
        );
        assert!(
            cfg.container_sccache_env().is_empty(),
            "default config must not emit sccache container env: {:?}",
            cfg.container_sccache_env(),
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = three_profile_manifest(dir.path());
        let bead = bead_with_labels("lm-1", &["profile:rust"]);
        let spawn = build_spawn_config_from_manifest(
            &manifest,
            &bead,
            None,
            &base(),
            PathBuf::from("/work/lm-1"),
            "p".into(),
            dir.path().join("scratch"),
            cfg.container_sccache_env(),
            vec![],
            sccache_mount(&cfg).into_iter().collect(),
            vec![],
        )
        .expect("dispatch");
        assert!(
            spawn.mounts.is_empty(),
            "no mounts must be emitted when sccache is unconfigured: {:?}",
            spawn.mounts,
        );
        assert!(
            !spawn.env.iter().any(|(k, _)| k == "SCCACHE_DIR"),
            "no SCCACHE_DIR env when sccache is unconfigured: {:?}",
            spawn.env,
        );
        assert!(
            !spawn.env.iter().any(|(k, _)| k == "RUSTC_WRAPPER"),
            "no RUSTC_WRAPPER env when sccache is unconfigured: {:?}",
            spawn.env,
        );
    }

    /// The `launcher_env` passed at dispatch lands verbatim on
    /// [`SpawnConfig::launcher_env`] (the host key paths a backend sets on
    /// the `wrapix spawn` child) and stays out of the in-container `env`
    /// allowlist. A loop agent boots without git keys unless this
    /// threading holds.
    #[test]
    fn launcher_env_threads_onto_spawn_config_not_container_env() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = three_profile_manifest(dir.path());
        let bead = bead_with_labels("lm-1", &["profile:rust"]);
        let launcher_env = vec![(
            "WRAPIX_SIGNING_KEY".to_string(),
            "/home/op/.ssh/deploy_keys/loom-host-signing".to_string(),
        )];

        let cfg = build_spawn_config_from_manifest(
            &manifest,
            &bead,
            None,
            &base(),
            PathBuf::from("/work/lm-1"),
            "p".into(),
            dir.path().join("scratch"),
            vec![],
            vec![],
            vec![],
            launcher_env.clone(),
        )
        .expect("dispatch");

        assert_eq!(cfg.launcher_env, launcher_env);
        assert!(
            !cfg.env.iter().any(|(k, _)| k == "WRAPIX_SIGNING_KEY"),
            "launcher keys must NOT leak into the in-container env allowlist: {:?}",
            cfg.env,
        );
    }

    /// A bead with a `profile:X` not declared in the manifest fails
    /// loudly with [`ProfileError::UnknownProfile`] — no silent default.
    #[test]
    fn unknown_profile_label_returns_typed_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = three_profile_manifest(dir.path());
        let bead = bead_with_labels("lm-1", &["profile:ruby"]);

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

use std::path::{Path, PathBuf};

use loom_driver::agent::{AgentRuntime, MountSpec, RePinContent, SpawnConfig, set_loom_inside};
use loom_driver::git::{
    GitError, WRIX_DEPLOY_KEY_ENV, WRIX_SIGNING_KEY_ENV, resolve_deploy_key, resolve_signing_key,
};
use loom_driver::profile_manifest::ImageEntry;
use tracing::info;

const WRIX_AGENT_ENV: &str = "WRIX_AGENT";
const CONTAINER_WORKSPACE: &str = "/workspace";

/// Structured spawn diagnostics emitted for every non-interactive Wrix spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnDiagnostics {
    pub agent_backend: AgentRuntime,
    pub wrix_agent_env: String,
    pub image_ref: String,
}

#[expect(
    clippy::too_many_arguments,
    reason = "explicit spawn construction boundary"
)]
pub fn build_spawn_config(
    entry: &ImageEntry,
    runtime: AgentRuntime,
    workspace: PathBuf,
    initial_prompt: String,
    scratch_dir: PathBuf,
    extra_env: Vec<(String, String)>,
    agent_args: Vec<String>,
    mounts: Vec<MountSpec>,
    launcher_env: Vec<(String, String)>,
) -> SpawnConfig {
    let runtime_name = runtime.as_str().to_string();
    let env = spawn_env(extra_env, &runtime_name);
    let launcher_env = spawn_launcher_env(launcher_env, &runtime_name);
    let diagnostics = log_spawn_diagnostics(runtime, &runtime_name, &entry.r#ref);
    let image_source_kind = (!entry.source.as_os_str().is_empty()).then_some(entry.source_kind);
    SpawnConfig {
        image_ref: diagnostics.image_ref,
        image_source: entry.source.clone(),
        image_source_kind,
        profile_config: entry.profile_config.clone(),
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
        skills: None,
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

fn spawn_env(mut extra_env: Vec<(String, String)>, runtime_name: &str) -> Vec<(String, String)> {
    set_runtime_env(&mut extra_env, runtime_name);
    set_loom_inside(&mut extra_env);
    extra_env
}

fn spawn_launcher_env(
    mut launcher_env: Vec<(String, String)>,
    runtime_name: &str,
) -> Vec<(String, String)> {
    set_runtime_env(&mut launcher_env, runtime_name);
    launcher_env
}

fn set_runtime_env(env: &mut Vec<(String, String)>, runtime_name: &str) {
    env.retain(|(key, _)| key != WRIX_AGENT_ENV);
    env.push((WRIX_AGENT_ENV.to_string(), runtime_name.to_string()));
}

pub fn container_workspace_path(host_workspace: &Path, host_path: &Path) -> PathBuf {
    match host_path.strip_prefix(host_workspace) {
        Ok(rel) => Path::new(CONTAINER_WORKSPACE).join(rel),
        Err(_) => host_path.to_path_buf(),
    }
}

pub fn launcher_key_env_for_checkout(workspace: &Path) -> Result<Vec<(String, String)>, GitError> {
    if !workspace.join(".git").exists() {
        return Ok(Vec::new());
    }
    let mut env = Vec::new();
    if let Some(key) = resolve_signing_key(workspace)? {
        env.push((
            WRIX_SIGNING_KEY_ENV.to_string(),
            key.to_string_lossy().into_owned(),
        ));
    }
    if let Some(key) = resolve_deploy_key(workspace)? {
        env.push((
            WRIX_DEPLOY_KEY_ENV.to_string(),
            key.to_string_lossy().into_owned(),
        ));
    }
    Ok(env)
}

pub fn log_spawn_diagnostics(
    runtime: AgentRuntime,
    runtime_name: &str,
    image_ref: &str,
) -> SpawnDiagnostics {
    info!(
        agent_backend = %runtime,
        wrix_agent_env = %runtime_name,
        image_ref = %image_ref,
        "wrix spawn: resolved backend runtime and image",
    );
    SpawnDiagnostics {
        agent_backend: runtime,
        wrix_agent_env: runtime_name.to_string(),
        image_ref: image_ref.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use super::*;
    use loom_driver::agent::ImageSourceKind;

    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("capture not poisoned").extend(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn entry() -> ImageEntry {
        ImageEntry {
            r#ref: "localhost/wrix-rust-pi:test".into(),
            source: PathBuf::from("/nix/store/image-rust-pi"),
            source_kind: ImageSourceKind::NixDescriptor,
            profile_config: Some(PathBuf::from("/nix/store/wrix-rust-pi-profile-config.json")),
            digest: Some(PathBuf::from("/nix/store/image-rust-pi-digest")),
            runtime: Some(AgentRuntime::Pi),
        }
    }

    #[test]
    fn container_workspace_path_rewrites_host_workspace_prefix() {
        let host_workspace = Path::new("/home/user/repo");
        let host_path = Path::new("/home/user/repo/.loom/scratch/lm-1/scratch.md");
        assert_eq!(
            container_workspace_path(host_workspace, host_path),
            PathBuf::from("/workspace/.loom/scratch/lm-1/scratch.md"),
        );
    }

    #[test]
    fn container_workspace_path_leaves_external_paths_unchanged() {
        let host_workspace = Path::new("/home/user/repo");
        let host_path = Path::new("/home/user/other");
        assert_eq!(
            container_workspace_path(host_workspace, host_path),
            PathBuf::from("/home/user/other"),
        );
    }

    #[test]
    fn wrix_spawn_child_env_sets_backend_derived_wrix_agent() {
        let cfg = build_spawn_config(
            &entry(),
            AgentRuntime::Pi,
            PathBuf::from("/workspace"),
            "prompt".into(),
            PathBuf::from("/workspace/.loom/scratch/key"),
            vec![("WRIX_AGENT".into(), "claude".into())],
            vec![],
            vec![],
            vec![("WRIX_AGENT".into(), "claude".into())],
        );

        assert_eq!(
            cfg.env
                .iter()
                .filter(|(key, _)| key == "WRIX_AGENT")
                .collect::<Vec<_>>(),
            vec![&("WRIX_AGENT".to_string(), "pi".to_string())],
        );
        assert_eq!(
            cfg.launcher_env
                .iter()
                .filter(|(key, _)| key == "WRIX_AGENT")
                .collect::<Vec<_>>(),
            vec![&("WRIX_AGENT".to_string(), "pi".to_string())],
        );
        assert_eq!(
            cfg.profile_config,
            Some(PathBuf::from("/nix/store/wrix-rust-pi-profile-config.json")),
        );
        assert_eq!(cfg.image_source_kind, Some(ImageSourceKind::NixDescriptor));
    }

    #[test]
    fn build_spawn_config_copies_docker_archive_source_kind_from_manifest() {
        let mut image = entry();
        image.source = PathBuf::from("/nix/store/no-filename-inference");
        image.source_kind = ImageSourceKind::DockerArchive;

        let cfg = build_spawn_config(
            &image,
            AgentRuntime::Pi,
            PathBuf::from("/workspace"),
            "prompt".into(),
            PathBuf::from("/workspace/.loom/scratch/key"),
            vec![],
            vec![],
            vec![],
            vec![],
        );

        assert_eq!(
            cfg.image_source,
            PathBuf::from("/nix/store/no-filename-inference")
        );
        assert_eq!(cfg.image_source_kind, Some(ImageSourceKind::DockerArchive));
        let json = serde_json::to_string(&cfg).expect("serialize spawn config");
        let raw: serde_json::Value = serde_json::from_str(&json).expect("spawn config json");
        assert_eq!(raw["image_source_kind"], "docker-archive");
    }

    #[test]
    fn build_spawn_config_omits_image_source_kind_without_source_override() {
        let mut image = entry();
        image.source = PathBuf::new();

        let cfg = build_spawn_config(
            &image,
            AgentRuntime::Pi,
            PathBuf::from("/workspace"),
            "prompt".into(),
            PathBuf::from("/workspace/.loom/scratch/key"),
            vec![],
            vec![],
            vec![],
            vec![],
        );

        let json = serde_json::to_string(&cfg).expect("serialize spawn config");
        let raw: serde_json::Value = serde_json::from_str(&json).expect("spawn config json");
        assert!(
            raw.get("image_source").is_none(),
            "no source override: {json}"
        );
        assert!(
            raw.get("image_source_kind").is_none(),
            "no source kind without source override: {json}",
        );
    }

    #[test]
    fn spawn_config_omits_profile_manifest_host_only_fields_from_wrix_json() {
        let cfg = build_spawn_config(
            &entry(),
            AgentRuntime::Pi,
            PathBuf::from("/workspace"),
            "prompt".into(),
            PathBuf::from("/workspace/.loom/scratch/key"),
            vec![],
            vec![],
            vec![],
            vec![],
        );

        let json = serde_json::to_string(&cfg).expect("serialize spawn config");
        let raw: serde_json::Value = serde_json::from_str(&json).expect("spawn config json");
        assert_eq!(
            raw["image_source_kind"], "nix-descriptor",
            "SpawnConfig must carry the manifest source_kind when image_source is emitted: {json}",
        );
        assert!(
            !json.contains("profile_config"),
            "wrix rejects profile_config as a per-launch override: {json}",
        );
        assert!(
            !json.contains("image_digest_path"),
            "wrix rejects image_digest_path as a per-launch override: {json}",
        );
        assert!(
            !json.contains("image_digest"),
            "wrix digest selection must stay in ProfileConfig, not SpawnConfig: {json}",
        );
    }

    #[test]
    fn wrix_spawn_logs_backend_runtime_and_image_ref() {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let writer_buf = Arc::clone(&buf);
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_writer(move || CaptureWriter(Arc::clone(&writer_buf)))
            .finish();

        let diagnostics = tracing::subscriber::with_default(subscriber, || {
            log_spawn_diagnostics(
                AgentRuntime::Direct,
                AgentRuntime::Direct.as_str(),
                "localhost/wrix-base-direct:abc",
            )
        });
        let captured = String::from_utf8(buf.lock().expect("capture not poisoned").clone())
            .expect("captured tracing output is UTF-8");

        assert_eq!(diagnostics.agent_backend, AgentRuntime::Direct);
        assert_eq!(diagnostics.wrix_agent_env, "direct");
        assert_eq!(diagnostics.image_ref, "localhost/wrix-base-direct:abc");
        assert!(captured.contains("agent_backend=direct"), "{captured}");
        assert!(captured.contains("wrix_agent_env=direct"), "{captured}");
        assert!(
            captured.contains("image_ref=localhost/wrix-base-direct:abc")
                || captured.contains("image_ref=\"localhost/wrix-base-direct:abc\""),
            "{captured}",
        );
    }
}

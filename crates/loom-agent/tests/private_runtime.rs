#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};

    use loom_agent::{ClaudeBackend, DirectBackend, PiBackend};
    use loom_driver::agent::{AgentBackend, ProtocolError, SpawnConfig};
    use loom_driver::scratch::ScratchSession;
    use loom_events::ParsedAgentEvent;

    const LAUNCHER: &str = r#"#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == '--profile-config' ]]
[[ "$2" == 'test-profile' ]]
[[ "$3" == 'spawn' ]]
[[ "$4" == '--spawn-config' ]]
[[ "$6" == '--stdio' ]]
config="$5"
[[ -r "$config" ]]
printf '%s' "$config" > "$TEST_CONFIG_PATH"
printf '%s\n' '[wrix] Starting container' >&2
if [[ "$WRIX_AGENT" == 'pi' ]]; then
    IFS= read -r probe
    if [[ "${TEST_HANG_PROBE:-0}" == '1' ]]; then
        IFS= read -r cancelled
        exit 1
    fi
    if [[ "${TEST_BAD_PROBE:-0}" == '1' ]]; then
        printf '%s\n' '{"type":"response","id":"loom-pi-probe","command":"get_state","success":true,"data":{}}'
        exit 0
    fi
    printf '%s\n' '{"type":"response","id":"loom-pi-probe","command":"get_state","success":true,"data":{"isStreaming":false,"isCompacting":false,"messageCount":0,"pendingMessageCount":0}}'
fi
IFS= read -r prompt
[[ -r "$config" ]]
case "$WRIX_AGENT" in
    pi) printf '%s\n' '{"type":"agent_end","messages":[]}' ;;
    claude) printf '%s\n' '{"type":"result","subtype":"success","total_cost_usd":0}' ;;
    direct) printf '%s\n' '{"type":"session_complete","exit_code":0}' ;;
esac
"#;

    fn install_launcher(workspace: &Path) -> PathBuf {
        let path = workspace.join("wrix");
        fs::write(&path, LAUNCHER).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn config(workspace: &Path, scratch: &Path, runtime: &str) -> SpawnConfig {
        let mut config: SpawnConfig = serde_json::from_value(serde_json::json!({
            "image_ref": "test-image",
            "workspace": workspace,
            "env": [["WRIX_AGENT", runtime], ["ANTHROPIC_API_KEY", "synthetic-test-credential"]],
            "initial_prompt": "test prompt",
            "agent_args": [],
            "repin": {"orientation": "test", "pinned_context": "test", "partial_bodies": []},
            "scratch_dir": scratch,
        }))
        .unwrap();
        config.wrix_launcher = Some(install_launcher(workspace));
        config.profile_config = Some(PathBuf::from("test-profile"));
        config.launcher_env = vec![
            ("WRIX_AGENT".into(), runtime.into()),
            (
                "TEST_CONFIG_PATH".into(),
                workspace.join("config-path").to_str().unwrap().into(),
            ),
        ];
        config
    }

    async fn private_lifecycle<B: AgentBackend>(runtime: &str) {
        let workspace = tempfile::tempdir().unwrap();
        let scratch = ScratchSession::open(workspace.path(), "session", "test", "test").unwrap();
        let config = config(workspace.path(), scratch.path(), runtime);
        let path = scratch.path().join("spawn-config.json");
        let session = B::spawn(&config).await.unwrap();
        assert_eq!(
            fs::metadata(scratch.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let json: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(
            json["env"].as_array().unwrap().iter().any(|pair| pair
                == &serde_json::json!(["ANTHROPIC_API_KEY", "synthetic-test-credential"]))
        );
        let expected_scratch = if runtime == "direct" {
            PathBuf::from("/workspace/.loom/scratch/session")
        } else {
            scratch.path().to_path_buf()
        };
        assert_eq!(json["scratch_dir"], expected_scratch.to_str().unwrap());
        assert_eq!(json["workspace"], workspace.path().to_str().unwrap());
        let mut session = session.prompt("test").await.unwrap();
        loop {
            match session.next_event().await.unwrap() {
                Some(ParsedAgentEvent::SessionComplete { exit_code: 0, .. }) => break,
                Some(_) => {}
                None => panic!("launcher did not complete"),
            }
        }
        assert!(session.child_mut().wait().await.unwrap().success());
        assert_eq!(
            fs::read_to_string(workspace.path().join("config-path")).unwrap(),
            path.to_str().unwrap()
        );
        drop(session);
        assert!(
            path.exists(),
            "scratch owns config through consumer teardown"
        );
        scratch.close().unwrap();
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn pi_spawn_config_is_private_mapped_and_scratch_owned() {
        private_lifecycle::<PiBackend>("pi").await;
    }

    #[tokio::test]
    async fn claude_spawn_config_is_private_mapped_and_scratch_owned() {
        private_lifecycle::<ClaudeBackend>("claude").await;
    }

    #[tokio::test]
    async fn direct_spawn_config_is_private_mapped_and_scratch_owned() {
        private_lifecycle::<DirectBackend>("direct").await;
    }

    async fn failed_startup_retries<B: AgentBackend>(runtime: &str) {
        let workspace = tempfile::tempdir().unwrap();
        let scratch = ScratchSession::open(workspace.path(), "session", "test", "test").unwrap();
        let mut config = config(workspace.path(), scratch.path(), runtime);
        config.wrix_launcher = Some(workspace.path().join("missing-launcher"));
        let path = scratch.path().join("spawn-config.json");
        for _ in 0..2 {
            assert!(
                matches!(B::spawn(&config).await, Err(ProtocolError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound)
            );
            assert!(
                !path.exists(),
                "failed exec must roll back the exclusive file"
            );
        }
        config.wrix_launcher = Some(workspace.path().join("wrix"));
        let session = B::spawn(&config).await.unwrap();
        drop(session);
        drop(scratch);
        assert!(!path.exists(), "scratch drop cleans up after retry");
    }

    #[tokio::test]
    async fn all_backends_remove_config_on_exec_failure_and_allow_retry() {
        failed_startup_retries::<PiBackend>("pi").await;
        failed_startup_retries::<ClaudeBackend>("claude").await;
        failed_startup_retries::<DirectBackend>("direct").await;
    }

    #[tokio::test]
    async fn pi_handshake_failure_removes_config_and_allows_retry() {
        let workspace = tempfile::tempdir().unwrap();
        let scratch = ScratchSession::open(workspace.path(), "session", "test", "test").unwrap();
        let mut config = config(workspace.path(), scratch.path(), "pi");
        config
            .launcher_env
            .push(("TEST_BAD_PROBE".into(), "1".into()));
        assert!(matches!(
            PiBackend::spawn(&config).await,
            Err(ProtocolError::Unsupported)
        ));
        let path = scratch.path().join("spawn-config.json");
        assert!(!path.exists());
        config
            .launcher_env
            .retain(|(key, _)| key != "TEST_BAD_PROBE");
        drop(PiBackend::spawn(&config).await.unwrap());
        drop(scratch);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn cancelled_pi_startup_removes_pending_config() {
        use std::future::{Future, poll_fn};
        use std::task::Poll;

        let workspace = tempfile::tempdir().unwrap();
        let scratch = ScratchSession::open(workspace.path(), "session", "test", "test").unwrap();
        let mut config = config(workspace.path(), scratch.path(), "pi");
        config
            .launcher_env
            .push(("TEST_HANG_PROBE".into(), "1".into()));
        let mut startup = Box::pin(PiBackend::spawn(&config));
        poll_fn(|context| {
            assert!(startup.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;
        let path = scratch.path().join("spawn-config.json");
        assert!(path.exists(), "startup created the pending config");
        drop(startup);
        assert!(!path.exists());
    }

    async fn rejects_unsafe_directory<B: AgentBackend>(runtime: &str) {
        let workspace = tempfile::tempdir().unwrap();
        let scratch = ScratchSession::open(workspace.path(), "session", "test", "test").unwrap();
        let config = config(workspace.path(), scratch.path(), runtime);
        fs::set_permissions(scratch.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            matches!(B::spawn(&config).await, Err(ProtocolError::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied)
        );
        assert!(!scratch.path().join("spawn-config.json").exists());
        fs::set_permissions(scratch.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let moved = scratch.path().with_extension("moved");
        fs::rename(scratch.path(), &moved).unwrap();
        symlink(&moved, scratch.path()).unwrap();
        assert!(B::spawn(&config).await.is_err());
        assert!(!moved.join("spawn-config.json").exists());
        fs::remove_file(scratch.path()).unwrap();
        fs::rename(moved, scratch.path()).unwrap();
    }

    #[tokio::test]
    async fn all_backends_require_private_nonsymlink_scratch_directories() {
        rejects_unsafe_directory::<PiBackend>("pi").await;
        rejects_unsafe_directory::<ClaudeBackend>("claude").await;
        rejects_unsafe_directory::<DirectBackend>("direct").await;
    }

    async fn rejects_existing<B: AgentBackend>(runtime: &str, link: bool) {
        let workspace = tempfile::tempdir().unwrap();
        let scratch = ScratchSession::open(workspace.path(), "session", "test", "test").unwrap();
        let config = config(workspace.path(), scratch.path(), runtime);
        let path = scratch.path().join("spawn-config.json");
        let target = workspace.path().join("target");
        fs::write(&target, "untouched").unwrap();
        if link {
            symlink(&target, &path).unwrap();
        } else {
            fs::write(&path, "existing config").unwrap();
        }
        assert!(
            matches!(B::spawn(&config).await, Err(ProtocolError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists)
        );
        assert!(!workspace.path().join("config-path").exists());
        assert_eq!(fs::read_to_string(&target).unwrap(), "untouched");
        if link {
            assert_eq!(fs::read_link(&path).unwrap(), target);
        } else {
            assert_eq!(fs::read_to_string(&path).unwrap(), "existing config");
        }
    }

    #[tokio::test]
    async fn all_backends_reject_existing_config_without_overwriting() {
        rejects_existing::<PiBackend>("pi", false).await;
        rejects_existing::<ClaudeBackend>("claude", false).await;
        rejects_existing::<DirectBackend>("direct", false).await;
    }

    #[tokio::test]
    async fn all_backends_reject_config_symlinks_without_modifying_targets() {
        rejects_existing::<PiBackend>("pi", true).await;
        rejects_existing::<ClaudeBackend>("claude", true).await;
        rejects_existing::<DirectBackend>("direct", true).await;
    }
}

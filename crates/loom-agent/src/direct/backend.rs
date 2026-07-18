//! Direct backend: spawn `wrix spawn` so the container's entrypoint
//! exec's `loom-direct-runner` listening on stdin/stdout.
//!
//! The host-side driver wires the launcher's stdio to an
//! [`AgentSession`] backed by a tiny JSONL parser. The runner emits the
//! same parser-emitted event surface ([`ParsedAgentEvent`]) Pi and Claude
//! emit; outbound commands (prompt/steer/complete/abort) are encoded as
//! `{"type": "...", ...}` JSONL frames — see [`DirectParser`] for the
//! wire shape.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use loom_driver::agent::{
    AgentBackend, AgentSession, Idle, JsonlReader, LineParse, ParsedLine, ProtocolError,
    SpawnConfig,
};
use loom_events::identifier::ToolCallId;
use loom_events::{DriverEventPayload, DriverKind, ParsedAgentEvent};

use crate::skill::{NoNativeRegistrar, register_native_skills};
use crate::{apply_launcher_env, resolve_wrix_spawn_bin};
use serde::{Deserialize, Serialize};
use tokio::io::BufWriter;
use tokio::process::Command;
use tracing::info;

/// File name for the JSON-serialized [`SpawnConfig`] handed to `wrix
/// spawn --spawn-config`. Written into the per-session
/// [`SpawnConfig::scratch_dir`]; the wrapper reads it back to materialize
/// the container.
const SPAWN_CONFIG_FILE: &str = "spawn-config.json";
const CONTAINER_WORKSPACE: &str = "/workspace";

/// Zero-sized marker for the Direct backend.
///
/// All runtime state lives in the spawned [`AgentSession`] and the
/// [`SpawnConfig`] passed to [`AgentBackend::spawn`]. The type parameter
/// alone dispatches `<B: AgentBackend>` call sites in `loom-workflow`
/// (`run_agent::<DirectBackend>(..)`).
///
/// Unlike Pi and Claude which drive external agent binaries, Direct
/// drives `loom-direct-runner` — a Loom-owned binary that composes
/// [`loom_llm::Conversation`] with the six sandbox-aware tools in
/// [`super::tools`]. The host-side surface is still a JSONL wire over
/// the launcher's stdin/stdout: the trust boundary (loom on host =
/// trusted; agent in container = sandboxed) is preserved identically to
/// the subprocess backends.
pub struct DirectBackend;

impl AgentBackend for DirectBackend {
    async fn spawn(config: &SpawnConfig) -> Result<AgentSession<Idle>, ProtocolError> {
        register_native_skills::<NoNativeRegistrar>(config)?;
        let spawn_config_path = prepare_runtime(config)?;

        let wrix_bin = resolve_wrix_spawn_bin(config);
        info!(
            wrix = %wrix_bin.to_string_lossy(),
            spawn_config = %spawn_config_path.display(),
            "direct backend spawn",
        );

        let mut cmd = build_wrix_command(
            &wrix_bin,
            config.profile_config.as_deref(),
            &spawn_config_path,
        );
        apply_launcher_env(&mut cmd, &config.launcher_env);
        spawn_session(cmd).await
    }
}

/// Build the `<wrix_bin> --profile-config <file> spawn --spawn-config <file>
/// --stdio` command [`DirectBackend::spawn`] launches. The argv shape is the
/// load-bearing contract loom owes the wrix wrapper: the wrapper resolves
/// `<file>` as a JSON [`SpawnConfig`] and `--stdio` selects the JSONL wire path
/// (rather than a TTY attach). Extracted so tests can pin the contract without
/// spawning a child or mutating the process env.
pub(crate) fn build_wrix_command(
    wrix_bin: &OsStr,
    profile_config: Option<&Path>,
    spawn_config_path: &Path,
) -> Command {
    let mut cmd = Command::new(wrix_bin);
    if let Some(profile_config) = profile_config {
        cmd.arg("--profile-config").arg(profile_config);
    }
    cmd.arg("spawn")
        .arg("--spawn-config")
        .arg(spawn_config_path)
        .arg("--stdio");
    cmd
}

/// Serialize the [`SpawnConfig`] into the per-session
/// [`SpawnConfig::scratch_dir`]. The wrapper reads this back to
/// materialize the container; Direct adds nothing to the scratch
/// directory beyond the spawn-config because the runner constructs
/// orientation in-process from the rendered prompt rather than via a
/// hook script.
///
/// Module-public so tests can verify the side effects independently of
/// the launcher exec.
pub(crate) fn prepare_runtime(config: &SpawnConfig) -> Result<PathBuf, ProtocolError> {
    write_spawn_config(&config.scratch_dir, config)
}

fn write_spawn_config(runtime_dir: &Path, config: &SpawnConfig) -> Result<PathBuf, ProtocolError> {
    std::fs::create_dir_all(runtime_dir).map_err(ProtocolError::Io)?;
    let path = runtime_dir.join(SPAWN_CONFIG_FILE);
    let mut runner_config = config.clone();
    runner_config.scratch_dir = container_workspace_path(&config.workspace, &config.scratch_dir);
    let json = serde_json::to_vec(&runner_config)?;
    std::fs::write(&path, json).map_err(ProtocolError::Io)?;
    Ok(path)
}

fn container_workspace_path(host_workspace: &Path, host_path: &Path) -> PathBuf {
    match host_path.strip_prefix(host_workspace) {
        Ok(rel) => Path::new(CONTAINER_WORKSPACE).join(rel),
        Err(_) => host_path.to_path_buf(),
    }
}

/// Build an [`AgentSession`] from a launcher [`Command`].
///
/// Module-public so integration tests can substitute a mock runner
/// binary in place of the real `wrix spawn` exec.
pub(crate) async fn spawn_session(mut cmd: Command) -> Result<AgentSession<Idle>, ProtocolError> {
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn().map_err(ProtocolError::Io)?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| ProtocolError::Io(io::Error::other("direct child stdin not piped")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ProtocolError::Io(io::Error::other("direct child stdout not piped")))?;

    Ok(AgentSession::new(
        child,
        BufWriter::new(stdin),
        JsonlReader::new(stdout),
        Box::new(DirectParser),
    ))
}

/// JSONL bridge between [`AgentSession`] and `loom-direct-runner`.
///
/// Inbound lines are a `type`-tagged twin of [`ParsedAgentEvent`] in
/// snake_case; outbound commands are
/// `{"type": "prompt"|"steer"|"complete"|"abort", "message": "..."}`.
/// The runner owns the canonical wire shape; this host-side half
/// deserializes the matching set of variants and rejects unknown `type` values as
/// `InvalidJson`.
pub struct DirectParser;

impl LineParse for DirectParser {
    fn parse_line(&self, line: &str) -> Result<ParsedLine, ProtocolError> {
        let wire: DirectEvent = serde_json::from_str(line)?;
        Ok(ParsedLine {
            events: vec![wire.into_parsed()],
            response: None,
        })
    }

    fn encode_prompt(&self, msg: &str) -> Result<String, ProtocolError> {
        encode_command(&DirectCommand::Prompt {
            message: msg.to_string(),
        })
    }

    fn encode_steer(&self, msg: &str) -> Result<String, ProtocolError> {
        encode_command(&DirectCommand::Steer {
            message: msg.to_string(),
        })
    }

    fn encode_complete(&self) -> Result<Option<String>, ProtocolError> {
        encode_command(&DirectCommand::Complete).map(Some)
    }

    fn encode_abort(&self) -> Result<Option<String>, ProtocolError> {
        encode_command(&DirectCommand::Abort).map(Some)
    }
}

fn encode_command(cmd: &DirectCommand) -> Result<String, ProtocolError> {
    let mut line = serde_json::to_string(cmd)?;
    line.push('\n');
    Ok(line)
}

/// JSONL command frame from the host driver to the in-container
/// `loom-direct-runner`. Mirrored on the runner side via the same enum
/// (round-trips through `serde_json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DirectCommand {
    /// Drive one `Conversation::run` cycle against `message`.
    Prompt { message: String },
    /// Inject `message` as a steering user turn the runner queues for
    /// the next iteration.
    Steer { message: String },
    /// Request one-shot completion after commands queued during the active
    /// turn have drained.
    Complete,
    /// Cancel the in-flight `Conversation::run` and return to idle.
    Abort,
}

/// JSONL event frame from `loom-direct-runner` to the host driver. The
/// host-side [`DirectParser`] decodes this and joins it with the per-spawn
/// envelope on the way out to the workflow's [`AgentEvent`] stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DirectEvent {
    TextDelta {
        text: String,
    },
    TextEnd,
    ToolCall {
        id: ToolCallId,
        tool: String,
        params: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_tool_call_id: Option<ToolCallId>,
    },
    ToolResult {
        id: ToolCallId,
        output: String,
        is_error: bool,
    },
    TurnEnd,
    SessionComplete {
        exit_code: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
    },
    Error {
        message: String,
    },
    /// Driver-origin payload emitted by the Direct runner and stamped by
    /// the host-side session envelope.
    DriverEvent {
        driver_kind: DriverKind,
        summary: String,
        payload: serde_json::Value,
    },
}

impl DirectEvent {
    /// Lift the wire-level event into the parser-emitted shape the host's
    /// session layer expects. The session joins the result with its
    /// per-spawn `EventEnvelope` via `AgentEvent::from_parsed`.
    pub fn into_parsed(self) -> ParsedAgentEvent {
        match self {
            Self::TextDelta { text } => ParsedAgentEvent::TextDelta { text },
            Self::TextEnd => ParsedAgentEvent::TextEnd,
            Self::ToolCall {
                id,
                tool,
                params,
                parent_tool_call_id,
            } => ParsedAgentEvent::ToolCall {
                id,
                tool,
                params,
                parent_tool_call_id,
            },
            Self::ToolResult {
                id,
                output,
                is_error,
            } => ParsedAgentEvent::ToolResult {
                id,
                output,
                is_error,
            },
            Self::TurnEnd => ParsedAgentEvent::TurnEnd,
            Self::SessionComplete {
                exit_code,
                cost_usd,
            } => ParsedAgentEvent::SessionComplete {
                exit_code,
                cost_usd,
            },
            Self::Error { message } => ParsedAgentEvent::Error { message },
            Self::DriverEvent {
                driver_kind,
                summary,
                payload,
            } => ParsedAgentEvent::DriverEvent(DriverEventPayload::new(
                driver_kind,
                summary,
                payload,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::agent::RePinContent;
    use loom_driver::config::{AgentObserversConfig, DoomLoopConfig, DuplicateResultConfig};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn sample_repin() -> RePinContent {
        RePinContent {
            orientation: "test orientation".to_string(),
            pinned_context: "test context".to_string(),
            partial_bodies: vec!["partial one".to_string()],
        }
    }

    fn sample_config(scratch_dir: PathBuf) -> SpawnConfig {
        SpawnConfig {
            image_ref: "localhost/wrix-test:direct".to_string(),
            image_source: PathBuf::from("/nix/store/zzz-wrix-test-direct.tar"),
            image_source_kind: Some(loom_driver::agent::ImageSourceKind::NixDescriptor),
            wrix_launcher: None,
            profile_config: Some(PathBuf::from(
                "/nix/store/wrix-test-direct-profile-config.json",
            )),
            workspace: PathBuf::from("/workspace"),
            env: vec![("WRIX_AGENT".into(), "direct".into())],
            mounts: vec![],
            initial_prompt: "hello".to_string(),
            agent_args: vec![],
            repin: sample_repin(),
            skills: None,
            event_metadata: None,
            scratch_dir,
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

    #[test]
    fn direct_backend_is_zero_sized() {
        assert_eq!(
            std::mem::size_of::<DirectBackend>(),
            0,
            "DirectBackend must be a ZST: all state lives in SpawnConfig and AgentSession",
        );
    }

    #[test]
    fn prepare_runtime_writes_spawn_config_into_scratch_dir() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let cfg = sample_config(scratch.path().to_path_buf());

        let spawn_config_path = prepare_runtime(&cfg).expect("prepare_runtime");

        assert_eq!(spawn_config_path, scratch.path().join(SPAWN_CONFIG_FILE));
        assert!(spawn_config_path.exists());

        let bytes = std::fs::read(&spawn_config_path).expect("read");
        let decoded: SpawnConfig = serde_json::from_slice(&bytes).expect("decode");
        assert_eq!(decoded.image_ref, cfg.image_ref);
        assert_eq!(decoded.image_source, cfg.image_source);
        assert_eq!(decoded.initial_prompt, cfg.initial_prompt);
        assert_eq!(decoded.agent_args, cfg.agent_args);
    }

    #[test]
    fn prepare_runtime_serializes_observer_config_for_runner() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let mut cfg = sample_config(scratch.path().to_path_buf());
        cfg.observers = AgentObserversConfig {
            doom_loop: DoomLoopConfig {
                enabled: false,
                ..DoomLoopConfig::default()
            },
            duplicate_result: DuplicateResultConfig {
                enabled: false,
                min_bytes: 1024,
            },
        };

        let spawn_config_path = prepare_runtime(&cfg).expect("prepare_runtime");

        let bytes = std::fs::read(&spawn_config_path).expect("read");
        let decoded: SpawnConfig = serde_json::from_slice(&bytes).expect("decode");
        assert_eq!(decoded.observers, cfg.observers);
    }

    #[test]
    fn prepare_runtime_serializes_container_visible_scratch_dir_for_runner() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let scratch_dir = workspace.path().join(".loom/scratch/lm-direct");
        let mut cfg = sample_config(scratch_dir.clone());
        cfg.workspace = workspace.path().to_path_buf();

        let spawn_config_path = prepare_runtime(&cfg).expect("prepare_runtime");

        assert_eq!(spawn_config_path, scratch_dir.join(SPAWN_CONFIG_FILE));
        let bytes = std::fs::read(&spawn_config_path).expect("read");
        let decoded: SpawnConfig = serde_json::from_slice(&bytes).expect("decode");
        assert_eq!(
            decoded.scratch_dir,
            PathBuf::from("/workspace/.loom/scratch/lm-direct"),
            "runner-visible offload root must be inside the container workspace",
        );
    }

    /// `DirectBackend::spawn` invokes the external Wrix launcher with the
    /// profile/spawn-config argv and passes `WRIX_AGENT=direct` through both
    /// launcher and container configuration boundaries.
    #[tokio::test]
    async fn direct_session_spawn_invokes_wrix_spawn_with_direct_runtime() {
        let root = tempfile::tempdir().expect("tempdir");
        let launcher = root.path().join("wrix");
        let argv_log = root.path().join("argv.log");
        let env_log = root.path().join("env.log");
        let config_log = root.path().join("spawn-config-copy.json");
        fs::write(
            &launcher,
            r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$@" > "$WRIX_TEST_ARGV_LOG"
printf '%s\n' "$WRIX_AGENT" > "$WRIX_TEST_ENV_LOG"
spawn_config=""
while (( $# > 0 )); do
    if [[ "$1" == "--spawn-config" ]]; then
        spawn_config="$2"
        shift 2
    else
        shift
    fi
done
if [[ -z "$spawn_config" ]]; then
    printf '%s\n' 'missing --spawn-config' >&2
    exit 2
fi
cp "$spawn_config" "$WRIX_TEST_CONFIG_LOG"
IFS= read -r _prompt
printf '%s\n' '{"type":"session_complete","exit_code":0}'
"#,
        )
        .expect("write fake wrix");
        let mut permissions = fs::metadata(&launcher)
            .expect("stat fake wrix")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&launcher, permissions).expect("chmod fake wrix");

        let scratch_dir = root.path().join("scratch");
        let mut cfg = sample_config(scratch_dir.clone());
        cfg.wrix_launcher = Some(launcher);
        cfg.launcher_env = vec![
            ("WRIX_AGENT".into(), "direct".into()),
            (
                "WRIX_TEST_ARGV_LOG".into(),
                argv_log.to_string_lossy().into_owned(),
            ),
            (
                "WRIX_TEST_ENV_LOG".into(),
                env_log.to_string_lossy().into_owned(),
            ),
            (
                "WRIX_TEST_CONFIG_LOG".into(),
                config_log.to_string_lossy().into_owned(),
            ),
        ];

        let session = DirectBackend::spawn(&cfg)
            .await
            .expect("spawn through Wrix");
        let mut session = session.prompt("hello Direct").await.expect("send prompt");
        assert!(matches!(
            session.next_event().await.expect("read event"),
            Some(ParsedAgentEvent::SessionComplete { exit_code: 0, .. })
        ));
        let status = session.child_mut().wait().await.expect("wait fake wrix");
        assert!(status.success());

        let args = fs::read_to_string(&argv_log).expect("read argv log");
        let expected_spawn_config = scratch_dir.join(SPAWN_CONFIG_FILE);
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            vec![
                "--profile-config",
                "/nix/store/wrix-test-direct-profile-config.json",
                "spawn",
                "--spawn-config",
                expected_spawn_config.to_str().expect("UTF-8 scratch path"),
                "--stdio",
            ],
        );
        assert_eq!(
            fs::read_to_string(&env_log).expect("read env log"),
            "direct\n",
        );
        let decoded: SpawnConfig =
            serde_json::from_slice(&fs::read(&config_log).expect("read copied spawn config"))
                .expect("decode copied spawn config");
        assert!(
            decoded
                .env
                .iter()
                .any(|(key, value)| key == "WRIX_AGENT" && value == "direct")
        );
    }

    #[tokio::test]
    async fn direct_session_completes_after_unsteered_turn() {
        let mut command = Command::new("sh");
        command.arg("-c").arg(
            r#"set -euo pipefail
IFS= read -r prompt
case "$prompt" in
    *'"type":"prompt"'*) ;;
    *) exit 2 ;;
esac
printf '%s\n' '{"type":"turn_end"}'
IFS= read -r complete
case "$complete" in
    *'"type":"complete"'*) ;;
    *) exit 3 ;;
esac
printf '%s\n' '{"type":"session_complete","exit_code":0}'
"#,
        );
        let session = spawn_session(command).await.expect("spawn mock runner");
        let mut session = session.prompt("hello").await.expect("send prompt");

        assert!(matches!(
            session.next_event().await.expect("read turn end"),
            Some(ParsedAgentEvent::TurnEnd)
        ));
        session.finish_turn().await.expect("complete final turn");
        assert!(matches!(
            session.next_event().await.expect("read completion"),
            Some(ParsedAgentEvent::SessionComplete { exit_code: 0, .. })
        ));
        assert!(session.child_mut().wait().await.expect("wait").success());
    }

    #[tokio::test]
    async fn direct_session_waits_for_each_steered_turn_before_completion() {
        let mut command = Command::new("sh");
        command.arg("-c").arg(
            r#"set -euo pipefail
IFS= read -r prompt
printf '%s\n' '{"type":"turn_end"}'
IFS= read -r first_steer
case "$first_steer" in
    *'"type":"steer"'*'"message":"first steer"'*) ;;
    *) exit 2 ;;
esac
printf '%s\n' '{"type":"turn_end"}'
IFS= read -r second_steer
case "$second_steer" in
    *'"type":"steer"'*'"message":"second steer"'*) ;;
    *) exit 3 ;;
esac
printf '%s\n' '{"type":"turn_end"}'
IFS= read -r complete
case "$complete" in
    *'"type":"complete"'*) ;;
    *) exit 4 ;;
esac
printf '%s\n' '{"type":"session_complete","exit_code":0}'
"#,
        );
        let session = spawn_session(command).await.expect("spawn mock runner");
        let mut session = session.prompt("hello").await.expect("send prompt");

        for steer in ["first steer", "second steer"] {
            assert!(matches!(
                session.next_event().await.expect("read turn end"),
                Some(ParsedAgentEvent::TurnEnd)
            ));
            session.steer(steer).await.expect("send steer");
            session.finish_turn().await.expect("finish steered turn");
        }
        assert!(matches!(
            session.next_event().await.expect("read final turn end"),
            Some(ParsedAgentEvent::TurnEnd)
        ));
        session.finish_turn().await.expect("complete final turn");
        assert!(matches!(
            session.next_event().await.expect("read completion"),
            Some(ParsedAgentEvent::SessionComplete { exit_code: 0, .. })
        ));
        assert!(session.child_mut().wait().await.expect("wait").success());
    }

    #[test]
    fn parser_decodes_text_delta_to_parsed_event() {
        let parsed = DirectParser
            .parse_line(r#"{"type":"text_delta","text":"hi there"}"#)
            .expect("parse");
        assert_eq!(parsed.events.len(), 1);
        match &parsed.events[0] {
            ParsedAgentEvent::TextDelta { text } => assert_eq!(text, "hi there"),
            other => panic!("expected TextDelta, got {other:?}"),
        }
        assert!(parsed.response.is_none());
    }

    #[test]
    fn parser_decodes_tool_call_with_optional_parent_id() {
        let parsed = DirectParser
            .parse_line(
                r#"{"type":"tool_call","id":"toolu_01","tool":"Read","params":{"path":"x"}}"#,
            )
            .expect("parse");
        match &parsed.events[0] {
            ParsedAgentEvent::ToolCall {
                id,
                tool,
                params,
                parent_tool_call_id,
            } => {
                assert_eq!(id.as_str(), "toolu_01");
                assert_eq!(tool, "Read");
                assert_eq!(params["path"], "x");
                assert!(parent_tool_call_id.is_none());
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn parser_decodes_session_complete_with_optional_cost() {
        let parsed = DirectParser
            .parse_line(r#"{"type":"session_complete","exit_code":0}"#)
            .expect("parse");
        match &parsed.events[0] {
            ParsedAgentEvent::SessionComplete {
                exit_code,
                cost_usd,
            } => {
                assert_eq!(*exit_code, 0);
                assert!(cost_usd.is_none());
            }
            other => panic!("expected SessionComplete, got {other:?}"),
        }
    }

    #[test]
    fn parser_decodes_driver_event_into_parsed_event() {
        let line = r#"{"type":"driver_event","driver_kind":"token_usage","summary":"claude-sonnet-4-6 input=500 output=120 cache_read=200 cache_write=50","payload":{"model":"claude-sonnet-4-6","input":500,"output":120,"cache_read":200,"cache_write":50}}"#;
        let parsed = DirectParser.parse_line(line).expect("parse");
        assert_eq!(parsed.events.len(), 1);
        match &parsed.events[0] {
            ParsedAgentEvent::DriverEvent(payload) => {
                assert_eq!(payload.driver_kind, DriverKind::TokenUsage);
                assert_eq!(payload.payload["model"], "claude-sonnet-4-6");
                assert_eq!(payload.payload["input"], 500);
                assert_eq!(payload.payload["output"], 120);
                assert_eq!(payload.payload["cache_read"], 200);
                assert_eq!(payload.payload["cache_write"], 50);
            }
            other => panic!("expected DriverEvent, got {other:?}"),
        }
        assert!(parsed.response.is_none());
    }

    #[test]
    fn parser_lifts_driver_event_with_driver_source() {
        let line = r#"{"type":"driver_event","driver_kind":"offload","summary":"Read offloaded 42 bytes","payload":{"tool":"Read","total_bytes":42}}"#;
        let parsed = DirectParser.parse_line(line).expect("parse");
        let mut builder = loom_events::EnvelopeBuilder::new(
            loom_events::SessionScope::phase(
                loom_events::identifier::SessionId::new("direct-test"),
                None,
            ),
            loom_events::Source::Agent,
            || 0,
        );
        let event = loom_events::AgentEvent::from_parsed(
            parsed.events.into_iter().next().expect("event"),
            builder.build(),
        );
        match event {
            loom_events::AgentEvent::DriverEvent {
                envelope,
                driver_kind,
                payload,
                ..
            } => {
                assert_eq!(envelope.source, loom_events::Source::Driver);
                assert_eq!(driver_kind, DriverKind::Offload);
                assert_eq!(payload["tool"], "Read");
                assert_eq!(payload["total_bytes"], 42);
            }
            other => panic!("expected DriverEvent, got {other:?}"),
        }
    }

    #[test]
    fn parser_rejects_unknown_type_as_invalid_json() {
        match DirectParser.parse_line(r#"{"type":"not_a_real_event"}"#) {
            Err(ProtocolError::InvalidJson(_)) => {}
            Err(other) => panic!("expected InvalidJson, got {other:?}"),
            Ok(_) => panic!("unknown variant must fail"),
        }
    }

    #[test]
    fn parser_encodes_prompt_without_prequeued_completion() {
        let encoded = DirectParser.encode_prompt("hello").expect("encode");
        assert!(
            encoded.ends_with('\n'),
            "missing trailing newline: {encoded}"
        );
        let lines = encoded.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 1, "prompt should emit exactly one frame");
        let prompt: serde_json::Value = serde_json::from_str(lines[0]).expect("valid prompt json");
        assert_eq!(prompt["type"], "prompt");
        assert_eq!(prompt["message"], "hello");
    }

    #[test]
    fn parser_encodes_controlled_completion_as_jsonl() {
        let encoded = DirectParser
            .encode_complete()
            .expect("encode")
            .expect("complete wire command");
        let complete: serde_json::Value =
            serde_json::from_str(encoded.trim_end()).expect("valid complete json");
        assert_eq!(complete["type"], "complete");
    }

    #[test]
    fn parser_encodes_steer_as_jsonl() {
        let encoded = DirectParser.encode_steer("turn left").expect("encode");
        let decoded: serde_json::Value =
            serde_json::from_str(encoded.trim_end()).expect("valid json");
        assert_eq!(decoded["type"], "steer");
        assert_eq!(decoded["message"], "turn left");
    }

    #[test]
    fn parser_encodes_abort_as_jsonl() {
        let encoded = DirectParser
            .encode_abort()
            .expect("encode")
            .expect("abort wire command");
        let decoded: serde_json::Value =
            serde_json::from_str(encoded.trim_end()).expect("valid json");
        assert_eq!(decoded["type"], "abort");
    }
}

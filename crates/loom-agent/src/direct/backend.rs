//! Direct backend: spawn `wrapix spawn` so the container's entrypoint
//! exec's `loom-direct-runner` listening on stdin/stdout.
//!
//! The host-side driver wires the launcher's stdio to an
//! [`AgentSession`] backed by a tiny JSONL parser. The runner emits the
//! same parser-emitted event surface ([`ParsedAgentEvent`]) Pi and Claude
//! emit; outbound commands (prompt/steer/abort) are encoded as
//! `{"type": "...", ...}` JSONL frames — see [`DirectParser`] for the
//! wire shape.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use loom_driver::agent::{
    AgentBackend, AgentSession, Idle, JsonlReader, LineParse, ParsedLine, ProtocolError,
    SpawnConfig,
};
use loom_events::ParsedAgentEvent;
use loom_events::identifier::ToolCallId;

use crate::apply_launcher_env;
use serde::{Deserialize, Serialize};
use tokio::io::BufWriter;
use tokio::process::Command;
use tracing::info;

/// File name for the JSON-serialized [`SpawnConfig`] handed to `wrapix
/// spawn --spawn-config`. Written into the per-session
/// [`SpawnConfig::scratch_dir`]; the wrapper reads it back to materialize
/// the container.
const SPAWN_CONFIG_FILE: &str = "spawn-config.json";

/// Env var that overrides the launcher binary. Production resolves
/// `wrapix` from `PATH`; tests substitute a mock script via this.
const ENV_WRAPIX_BIN: &str = "LOOM_WRAPIX_BIN";

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
        let spawn_config_path = prepare_runtime(config)?;

        let wrapix_bin =
            std::env::var_os(ENV_WRAPIX_BIN).unwrap_or_else(|| OsString::from("wrapix"));
        info!(
            wrapix = %wrapix_bin.to_string_lossy(),
            spawn_config = %spawn_config_path.display(),
            "direct backend spawn",
        );

        let mut cmd = build_wrapix_command(&wrapix_bin, &spawn_config_path);
        apply_launcher_env(&mut cmd, &config.launcher_env);
        spawn_session(cmd).await
    }
}

/// Build the `<wrapix_bin> spawn --spawn-config <file> --stdio` command
/// [`DirectBackend::spawn`] launches. The argv shape is the load-bearing
/// contract loom owes the wrapix wrapper: the wrapper resolves `<file>`
/// as a JSON [`SpawnConfig`] and `--stdio` selects the JSONL wire path
/// (rather than a TTY attach). Extracted so tests can pin the contract
/// without spawning a child or mutating the process env.
pub(crate) fn build_wrapix_command(wrapix_bin: &OsStr, spawn_config_path: &Path) -> Command {
    let mut cmd = Command::new(wrapix_bin);
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
    let json = serde_json::to_vec(config)?;
    std::fs::write(&path, json).map_err(ProtocolError::Io)?;
    Ok(path)
}

/// Build an [`AgentSession`] from a launcher [`Command`].
///
/// Module-public so integration tests can substitute a mock runner
/// binary in place of the real `wrapix spawn` exec.
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
/// `{"type": "prompt"|"steer"|"abort", "message": "..."}`. The runner
/// owns the canonical wire shape; this host-side half deserializes the
/// matching set of variants and rejects unknown `type` values as
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
    /// Per-completion token accounting the runner emits after every
    /// `LlmClient::complete*` call. Host-side parser lifts this into a
    /// `DriverKind::TokenUsage` `AgentEvent::DriverEvent`.
    TokenUsage {
        model: String,
        input: u32,
        output: u32,
        cache_read: u32,
        cache_write: u32,
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
            Self::TokenUsage {
                model,
                input,
                output,
                cache_read,
                cache_write,
            } => ParsedAgentEvent::TokenUsage {
                model,
                input,
                output,
                cache_read,
                cache_write,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::agent::RePinContent;
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
            image_ref: "localhost/wrapix-test:direct".to_string(),
            image_source: PathBuf::from("/nix/store/zzz-wrapix-test-direct.tar"),
            image_digest_path: None,
            workspace: PathBuf::from("/workspace"),
            env: vec![("WRAPIX_AGENT".into(), "direct".into())],
            mounts: vec![],
            initial_prompt: "hello".to_string(),
            agent_args: vec![],
            repin: sample_repin(),
            scratch_dir,
            model: None,
            thinking_level: None,
            output_limits: None,
            shutdown_grace: None,
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

    /// Spec contract (`specs/agent.md` § Direct backend, L753–754):
    /// `DirectBackend::spawn` launches `wrapix spawn --spawn-config <file>
    /// --stdio`, and the spawn-config the wrapper reads carries
    /// `WRAPIX_AGENT=direct` so the container's entrypoint dispatches to
    /// the direct runtime layer (`lib/sandbox/linux/entrypoint.sh` exec's
    /// `loom-direct-runner` on this value). Both halves of the contract
    /// are pinned here: the argv shape via [`build_wrapix_command`]
    /// inspection (the in-process function `DirectBackend::spawn` calls)
    /// and the runtime-layer signal via the on-disk spawn-config the
    /// wrapper deserialises.
    #[test]
    fn direct_session_spawn_invokes_wrapix_spawn_with_direct_runtime() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let cfg = sample_config(scratch.path().to_path_buf());
        assert!(
            cfg.env
                .iter()
                .any(|(k, v)| k == "WRAPIX_AGENT" && v == "direct"),
            "sample_config must seed the direct runtime marker; got env={:?}",
            cfg.env,
        );

        let spawn_config_path = prepare_runtime(&cfg).expect("prepare_runtime");

        let wrapix_bin = OsStr::new("wrapix");
        let cmd = build_wrapix_command(wrapix_bin, &spawn_config_path);
        let std_cmd = cmd.as_std();

        assert_eq!(
            std_cmd.get_program(),
            wrapix_bin,
            "program must be the wrapix launcher, not podman directly",
        );
        let args: Vec<&OsStr> = std_cmd.get_args().collect();
        assert_eq!(
            args,
            vec![
                OsStr::new("spawn"),
                OsStr::new("--spawn-config"),
                spawn_config_path.as_os_str(),
                OsStr::new("--stdio"),
            ],
            "argv contract is `spawn --spawn-config <file> --stdio`; got={args:?}",
        );

        let bytes = std::fs::read(&spawn_config_path).expect("read spawn-config");
        let decoded: SpawnConfig = serde_json::from_slice(&bytes).expect("decode spawn-config");
        assert!(
            decoded
                .env
                .iter()
                .any(|(k, v)| k == "WRAPIX_AGENT" && v == "direct"),
            "spawn-config.json must round-trip WRAPIX_AGENT=direct so the \
             entrypoint exec's loom-direct-runner; got env={:?}",
            decoded.env,
        );
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
    fn parser_decodes_token_usage_into_parsed_event() {
        let line = r#"{"type":"token_usage","model":"claude-sonnet-4-6","input":500,"output":120,"cache_read":200,"cache_write":50}"#;
        let parsed = DirectParser.parse_line(line).expect("parse");
        assert_eq!(parsed.events.len(), 1);
        match &parsed.events[0] {
            ParsedAgentEvent::TokenUsage {
                model,
                input,
                output,
                cache_read,
                cache_write,
            } => {
                assert_eq!(model, "claude-sonnet-4-6");
                assert_eq!(*input, 500);
                assert_eq!(*output, 120);
                assert_eq!(*cache_read, 200);
                assert_eq!(*cache_write, 50);
            }
            other => panic!("expected TokenUsage, got {other:?}"),
        }
        assert!(parsed.response.is_none());
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
    fn parser_encodes_prompt_as_jsonl_with_trailing_newline() {
        let encoded = DirectParser.encode_prompt("hello").expect("encode");
        assert!(
            encoded.ends_with('\n'),
            "missing trailing newline: {encoded}"
        );
        let decoded: serde_json::Value =
            serde_json::from_str(encoded.trim_end()).expect("valid json");
        assert_eq!(decoded["type"], "prompt");
        assert_eq!(decoded["message"], "hello");
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
